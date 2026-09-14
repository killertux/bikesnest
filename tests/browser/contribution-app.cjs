const assert = require('node:assert/strict');
const { chromium } = require('playwright');

const origin = process.env.BIKESNEST_CONTRIBUTION_TEST_ORIGIN;
const id = process.env.BIKESNEST_CONTRIBUTION_TEST_ID;
const session = process.env.BIKESNEST_CONTRIBUTION_TEST_SESSION;
const timeout = setTimeout(() => { console.error('B13 contribution browser regression exceeded 45 seconds'); process.exit(1); }, 45_000);

async function assertVisibleFocus(page, selector) {
  assert.equal(await page.evaluate(() => document.activeElement && document.activeElement.id), selector.slice(1));
  const box = await page.locator(selector).boundingBox();
  assert.ok(box && box.y >= 0 && box.y < 844, 'focused invalid field must be visible at390');
}

async function submitBoosted(page, path, submit) {
  const finished = page.evaluate(() => new Promise(resolve => {
    document.addEventListener('htmx:finally:request', resolve, { once: true });
  }));
  const [response] = await Promise.all([
    page.waitForResponse(candidate => candidate.request().method() === 'POST' && new URL(candidate.url()).pathname === path),
    submit.click(),
  ]);
  await finished;
  return response;
}

async function assertPaidFields(page, visible, message) {
  await page.waitForFunction(expected => {
    const fields = document.querySelector('[x-show="costKind === \'paid\'"]');
    return fields && (getComputedStyle(fields).display !== 'none') === expected;
  }, visible);
  assert.equal(await page.locator('[x-show="costKind === \'paid\'"]').isVisible(), visible, message);
}

(async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const context = await browser.newContext({ locale: 'en-US', viewport: { width: 390, height: 844 } });
    await context.addCookies([{ name: 'session_id', value: session, url: origin }]);
    await context.route(origin + '/b13-test-style.json', route => route.fulfill({ contentType: 'application/json', body: '{"version":8,"sources":{},"layers":[]}' }));
    const page = await context.newPage();
    await page.goto(origin + '/parking/new');
    await assertPaidFields(page, false, 'unknown cost keeps paid-only fields collapsed');
    await page.locator('#cost_kind').selectOption('paid');
    await assertPaidFields(page, true, 'paid cost discloses price fields');
    await page.locator('#price_currency').fill('X');
    await page.locator('#price').fill('10');
    await page.locator('#price_unit').selectOption('hour');
    await page.locator('#name').fill('B13 browser new rack');
    await page.locator('#address').fill('Rua B13, 1');
    await page.locator('#description').fill('Kept through rejected submissions');
    const hours = page.locator('details').filter({ hasText: 'Hours' }).first();
    await hours.evaluate(el => el.open = true);
    await page.locator('#h_mon_state').selectOption('closed');
    const security = page.locator('details').filter({ hasText: 'Security attributes' }).first();
    await security.evaluate(el => el.open = true);
    const newCctvNo = page.locator('input[name="sec_cctv"][value="no"]');
    await newCctvNo.focus();
    await page.keyboard.press('Space');
    await page.locator('details').filter({ hasText: 'Advanced' }).first().evaluate(el => el.open = true);
    await page.locator('#lat').fill('-23.4');
    await page.locator('#lon').fill('-46.6');
    const createSubmit = page.locator('form[action="/parking/new"] button[type="submit"]');
    assert.equal((await submitBoosted(page, '/parking/new', createSubmit)).status(), 400);
    await page.locator('#price-error').waitFor();
    await assertVisibleFocus(page, '#price');
    assert.equal(await page.locator('#name').inputValue(), 'B13 browser new rack');
    assert.equal(await page.locator('#description').inputValue(), 'Kept through rejected submissions');
    assert.equal(await page.locator('#lat').inputValue(), '-23.4');
    assert.equal(await page.locator('#h_mon_state').inputValue(), 'closed');
    assert.equal(await newCctvNo.isChecked(), true);
    assert.equal(await hours.getAttribute('open'), '');
    assert.equal(await security.getAttribute('open'), '');

    await page.locator('#price_currency').fill('BRL');
    await page.locator('#h_mon_state').selectOption('ranges');
    await page.locator('#h_mon_1_open').fill('09:00');
    await page.locator('#h_mon_1_close').fill('12:00');
    await page.locator('#h_mon_2_open').fill('11:00');
    await page.locator('#h_mon_2_close').fill('13:00');
    assert.equal((await submitBoosted(page, '/parking/new', createSubmit)).status(), 400);
    await page.locator('#h_mon_state-error').waitFor();
    await assertVisibleFocus(page, '#h_mon_state');
    assert.equal(await hours.getAttribute('open'), '', 'an optional-hours error reopens its disclosure');
    assert.equal(await page.locator('#name').inputValue(), 'B13 browser new rack');
    assert.equal(await newCctvNo.isChecked(), true);
    await page.locator('#h_mon_state').selectOption('closed');

    await page.locator('details').filter({ hasText: 'Advanced' }).first().evaluate(el => el.open = true);
    await page.locator('#timezone').fill('not/a-timezone');
    assert.equal((await submitBoosted(page, '/parking/new', createSubmit)).status(), 400);
    await page.locator('#timezone-error').waitFor();
    await assertVisibleFocus(page, '#timezone');
    const advanced = page.locator('details').filter({ hasText: 'Advanced' }).first();
    assert.equal(await advanced.getAttribute('open'), '', 'an advanced error reopens its native disclosure');
    assert.equal(await page.locator('#name').inputValue(), 'B13 browser new rack');
    assert.equal(await page.locator('#h_mon_state').inputValue(), 'closed');
    assert.equal(await newCctvNo.isChecked(), true);

    await page.locator('#timezone').fill('America/Sao_Paulo');
    await page.locator('#cost_kind').selectOption('free');
    assert.equal(await page.locator('#price').inputValue(), '10', 'changing away from paid preserves an entered price');
    await assertPaidFields(page, false, 'free cost collapses paid-only fields');
    await page.locator('#cost_kind').selectOption('unknown');
    await assertPaidFields(page, false, 'unknown cost also keeps paid-only fields collapsed');
    await page.locator('#cost_kind').selectOption('free');
    const created = await submitBoosted(page, '/parking/new', createSubmit);
    assert.equal(created.status(), 303, 'corrected values create through the normal route when no duplicate is found');
    await page.getByRole('heading', { name: 'B13 browser new rack' }).waitFor();
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.goto(origin + '/parking/' + id + '/edit');
    await page.locator('#cost_kind').selectOption('paid');
    await page.locator('#price_currency').fill('BRL');
    await page.locator('#price').fill('5');
    await page.locator('#price_unit').selectOption('hour');
    await page.locator('details').filter({ hasText: 'Hours' }).first().evaluate(el => el.open = true);
    await page.locator('#h_mon_state').selectOption('closed');
    await page.locator('details').filter({ hasText: 'Security attributes' }).first().evaluate(el => el.open = true);
    const editCctvNo = page.locator('input[name="sec_cctv"][value="no"]');
    await editCctvNo.focus();
    await page.keyboard.press('Space');
    assert.equal(await editCctvNo.isChecked(), true, 'styled tri-state radio remains keyboard operable');
    const proposed = await submitBoosted(
      page,
      '/parking/' + id + '/edit',
      page.locator('form[action="/parking/' + id + '/edit"] button[type="submit"]'),
    );
    assert.equal(proposed.status(), 303, 'existing facts go through the proposal route');
    await page.waitForURL(url => url.pathname === '/parking/' + id && url.searchParams.get('proposed') === '1' && url.searchParams.get('tab') === 'approvals');
    await context.close();

    const native = await browser.newContext({ javaScriptEnabled: false, locale: 'en-US', viewport: { width: 390, height: 844 } });
    await native.addCookies([{ name: 'session_id', value: session, url: origin }]);
    const nativePage = await native.newPage();
    await nativePage.goto(origin + '/parking/new');
    assert.equal(await nativePage.locator('#price_currency').isVisible(), true, 'no-JS paid fields remain reachable');
    await nativePage.locator('#cost_kind').selectOption('paid');
    await nativePage.locator('#price_currency').fill('BRL');
    await nativePage.locator('#price').fill('7.50');
    await nativePage.locator('#price_unit').selectOption('day');
    assert.equal(await nativePage.locator('#price').inputValue(), '7.50', 'paid values remain native form controls');
    const nativeHours = nativePage.locator('details').filter({ hasText: 'Hours' }).first();
    await nativeHours.locator('summary').focus();
    await nativeHours.locator('summary').press('Enter');
    assert.equal(await nativeHours.getAttribute('open'), '');
    await native.close();
    console.log('B13 real contribution forms passed at390, desktop, boosted error focus and native details.');
  } finally { await browser.close(); clearTimeout(timeout); }
})().catch(error => { console.error(error); process.exitCode = 1; });
