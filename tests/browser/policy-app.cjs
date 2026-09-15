const assert = require('node:assert/strict');
const { chromium } = require('playwright');

const { origin, ids, sessions, futureEffectiveAt } = JSON.parse(process.env.BIKESNEST_POLICY_BROWSER_INPUT);
const watchdog = setTimeout(() => {
  console.error('Terms browser regression exceeded 60 seconds');
  process.exit(1);
}, 60_000);

async function postForm(page, form, path, javascript) {
  if (javascript) {
    const completed = page.evaluate(() => new Promise(resolve => {
      document.addEventListener('htmx:finally:request', resolve, { once: true });
    }));
    const [response] = await Promise.all([
      page.waitForResponse(r => r.request().method() === 'POST' && new URL(r.url()).pathname === path),
      form.locator('button[type="submit"]').click(),
    ]);
    await completed;
    return response.status();
  }
  const [response] = await Promise.all([
    page.waitForResponse(r => r.request().method() === 'POST' && new URL(r.url()).pathname === path),
    page.waitForNavigation(),
    form.locator('button[type="submit"]').click(),
  ]);
  return response.status();
}

(async () => {
  const browser = await chromium.launch({ headless: true });
  const external = [];
  try {
    for (const [index, locale] of ['en', 'pt-BR'].entries()) {
      const javascript = locale === 'en';
      const context = await browser.newContext({
        locale, javaScriptEnabled: javascript,
        viewport: { width: javascript ? 390 : 1280, height: 844 },
      });
      await context.route('**/*', route => {
        if (new URL(route.request().url()).origin === origin) return route.continue();
        external.push(route.request().url());
        return route.abort();
      });
      await context.addCookies([{ name: 'lang', value: locale, url: origin }]);
      const page = await context.newPage();
      const current = ids[`browser-current:${locale}`];
      const old = ids[`browser-old:${locale}`];
      const future = ids[`browser-future:${locale}`];

      await page.goto(origin + '/register');
      assert.equal(await page.locator('html').getAttribute('lang'), locale);
      assert.equal(await page.locator('input[name="terms_policy_id"]').inputValue(), String(current));
      assert.equal(await page.locator(`a[href="/terms/versions/${current}"]`).count(), 1);
      await page.locator('#email').fill(`b15-browser-signup-${locale}@example.com`);
      await page.locator('#display_name').fill('Browser retained name');
      await page.locator('#password').fill('browser-password-15');
      // A form opened before activation has no proof fields. Simulate that
      // submitted markup without changing process-global configuration.
      await page.locator('input[name="terms_policy_id"], input[name="terms_version"]')
        .evaluateAll(elements => elements.forEach(element => element.remove()));
      assert.equal(await postForm(page, page.locator('form[action="/register"]'), '/register', javascript), 409);
      assert.equal(await page.locator('#email').count(), 1, 'old forms recover to the signup form');
      assert.equal(await page.locator('#email').inputValue(), `b15-browser-signup-${locale}@example.com`);
      assert.equal(await page.locator('#display_name').inputValue(), 'Browser retained name');
      assert.equal(await page.locator('#password').inputValue(), '', 'recovery never retains passwords');
      assert.equal(await page.locator('input[name="terms_policy_id"]').inputValue(), String(current));
      assert.equal(await page.locator('input[name="terms_version"]').inputValue(), 'browser-current');
      assert.equal(await page.locator(`a[href="/terms/versions/${current}"]`).count(), 1);
      await page.locator('#password').fill('browser-password-15');
      // Exercise actual stale-form recovery without a timing-dependent sleep.
      // The independent DB test covers activation during an actual lock wait.
      await page.locator('input[name="terms_policy_id"]').evaluate((el, value) => el.value = value, String(old));
      await page.locator('input[name="terms_version"]').evaluate(el => el.value = 'browser-old');
      assert.equal(await postForm(page, page.locator('form[action="/register"]'), '/register', javascript), 409);
      assert.equal(await page.locator('#email').inputValue(), `b15-browser-signup-${locale}@example.com`);
      assert.equal(await page.locator('#display_name').inputValue(), 'Browser retained name');
      assert.equal(await page.locator('#password').inputValue(), '', 'stale recovery never retains passwords');
      assert.equal(await page.locator('input[name="terms_policy_id"]').inputValue(), String(current));
      assert.equal(await page.locator('input[name="terms_version"]').inputValue(), 'browser-current');
      await page.locator('#password').fill('browser-password-15');
      const signupStatus = await postForm(page, page.locator('form[action="/register"]'), '/register', javascript);
      assert.ok([200, 303].includes(signupStatus), `signup status ${signupStatus}`);
      console.log(JSON.stringify({ stage: 'signup-result', locale, status: signupStatus,
        url: page.url(), heading: await page.locator('h1').allTextContents(),
        alerts: await page.locator('[role="alert"]').allTextContents() }));
      await page.locator('form[action="/login"]').waitFor();
      await page.waitForURL(url => url.pathname === '/login' && url.searchParams.get('registered') === '1');

      await context.addCookies([{ name: 'session_id', value: sessions[index], url: origin }]);
      await page.goto(origin + '/account');
      assert.ok(await page.locator('[data-terms-notice]').first().isVisible());
      assert.equal(await page.locator(`[data-terms-notice] a[href="/account/terms-notice/${current}"]`).count(), 1,
        'upcoming terms must not hide the unacknowledged current release');
      assert.equal(await page.locator(`[data-terms-notice] a[href="/account/terms-notice/${future}"]`).count(), 1);
      assert.equal((await page.goto(origin + '/account/privacy')).status(), 200);
      assert.equal(new URL(page.url()).pathname, '/account/privacy',
        'unacknowledged current terms must not redirect away from privacy controls');
      await page.goto(origin + `/account/terms-notice/${future}`);
      assert.ok(await page.locator('[data-terms-future]').isVisible());
      assert.ok((await page.locator('main').innerText()).includes(`terms browser-future ${locale}`));
      const futureEffective = await page.locator('[data-terms-future]').getAttribute('data-effective-at');
      assert.equal(Date.parse(futureEffective), Date.parse(futureEffectiveAt));
      assert.ok(Date.parse(futureEffective) > Date.now(), 'future notice identifies its actual future effective date');
      assert.equal(await page.locator('[data-terms-ack]').count(), 0);
      await page.reload();
      assert.ok(await page.locator('[data-terms-future]').isVisible());

      await page.goto(origin + `/terms/versions/${old}`);
      assert.ok((await page.locator('main').innerText()).includes('browser-old'));
      await page.goto(origin + '/terms');
      assert.ok((await page.locator('main').innerText()).includes('browser-current'));
      assert.ok(!(await page.locator('main').innerText()).includes('browser-future'));

      await page.goto(origin + `/account/terms-notice/${current}`);
      await page.reload();
      const ackPath = `/account/terms-notice/${current}/acknowledge`;
      const ack = page.locator(`form[action="${ackPath}"]`);
      assert.ok(await ack.isVisible());
      assert.ok((await page.locator('main').innerText()).includes(`terms browser-current ${locale}`));
      assert.equal(await ack.locator('input[name="policy_id"]').inputValue(), String(current));
      assert.equal(await ack.locator('input[name="terms_version"]').inputValue(), 'browser-current');
      const ackStatus = await postForm(page, ack, ackPath, javascript);
      assert.ok([200, 303].includes(ackStatus), `acknowledgement status ${ackStatus}`);
      await page.waitForURL(url => url.pathname === '/account' && url.searchParams.get('terms_acknowledged') === '1');
      // Changing locale preserves release satisfaction; the future release remains a notice.
      const otherLocale = locale === 'en' ? 'pt-BR' : 'en';
      await context.addCookies([{ name: 'lang', value: otherLocale, url: origin }]);
      await page.goto(origin + '/account');
      assert.equal(await page.locator('html').getAttribute('lang'), otherLocale);
      assert.equal(await page.locator(`[data-terms-notice] a[href="/account/terms-notice/${ids[`browser-current:${otherLocale}`]}"]`).count(), 0);
      assert.equal(await page.locator(`[data-terms-notice] a[href="/account/terms-notice/${ids[`browser-future:${otherLocale}`]}"]`).count(), 1);
      // Neither current nor upcoming terms restrict access to privacy controls.
      assert.equal((await page.goto(origin + '/account/privacy')).status(), 200);
      await context.close();
    }
    assert.deepEqual(external, [], 'these legal journeys must not contact outside providers');
    console.log('Terms browser journeys passed: EN boosted/mobile + PT native/desktop, exact versions, stale recovery, current ack, future notice, locale change.');
  } finally {
    await browser.close();
    clearTimeout(watchdog);
  }
})().catch(error => { console.error(error); process.exitCode = 1; });
