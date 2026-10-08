// Invoked only by csrf_browser_test.rs, against its loopback Axum server.
// Unlike the fast navigation fixtures, HTML and CSRF enforcement are real.
const assert = require('node:assert/strict');
const { randomUUID } = require('node:crypto');
const { chromium } = require('playwright');
const mutation = require('./mutation.cjs').scriptMutation('csrf-stale-head');

const origin = process.env.BIKESNEST_CSRF_TEST_ORIGIN;
const target = new URL(origin);
assert.equal(target.protocol, 'http:');
assert.equal(target.hostname, '127.0.0.1');
assert.ok(target.port, 'the Rust harness must supply its ephemeral port');

let browser;
const deadline = setTimeout(() => {
  console.error('Real-app CSRF browser regression exceeded 90 seconds');
  process.exit(1);
}, 90_000);

async function submitReset(page, expectedStatus) {
  const [result] = await Promise.all([
    page.waitForResponse(r =>
      r.request().method() === 'POST' && new URL(r.url()).pathname === '/password-reset'),
    page.locator('form[action="/password-reset"] button[type="submit"]').click(),
  ]);
  assert.equal(result.status(), expectedStatus);
  if (expectedStatus === 303) {
    await page.getByText('If that address exists, a reset link has been sent.').waitFor();
  }
  return result;
}

async function resetForm(page) {
  await page.goto(origin + '/password-reset');
  await page.locator('form[action="/password-reset"] input[name="csrf"]').waitFor({ state: 'attached' });
}

async function submitForm(page, action, expectedStatus) {
  const button = page.locator('form[action="' + action + '"] button[type="submit"]').first();
  if (action === '/logout' && !await button.isVisible()) {
    await page.locator('[aria-controls="account-menu"]').click();
  }
  const [response] = await Promise.all([
    page.waitForResponse(r => r.request().method() === 'POST' && new URL(r.url()).pathname === action),
    button.click(),
  ]);
  assert.equal(response.status(), expectedStatus);
}

(async () => {
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ locale: 'en-US' });
  context.setDefaultTimeout(10_000);
  await context.route('**/*', async route => {
    const url = new URL(route.request().url());
    if (url.origin !== target.origin) return route.abort();
    if (await mutation.intercept(route)) return;
    return route.continue();
  });
  const page = await context.newPage();
  page.on('pageerror', error => console.error('Browser script error:', error.message));
  page.on('requestfailed', request => console.error('Browser request failed:',
    request.method(), new URL(request.url()).pathname, request.failure()?.errorText));
  const requests = [];
  page.on('request', request => requests.push(request));
  await page.goto(origin + '/login');
  const first = await page.locator('form[action="/login"] input[name="csrf"]').inputValue();
  mutation.assertApplied();
  assert.ok(first);
  await page.evaluate(() => { window.csrfDocumentMarker = 'original-document'; });
  await page.locator('a[href="/password-reset"]').click();
  await page.locator('form[action="/password-reset"]').waitFor();
  assert.equal(await page.evaluate(() => window.csrfDocumentMarker), 'original-document',
    'the regression must exercise a boosted swap, not a full navigation');
  assert.equal(await page.locator('input[name="csrf"]').inputValue(), first,
    'visiting another anonymous form must not rotate the cookie');
  // Force the stale-head condition independently of the cookie-reuse fix.
  await page.evaluate(() => document.querySelector('meta[name="csrf"]').content = 'obsolete-head');
  await page.locator('input[name="email"]').fill('csrf-browser-missing@example.invalid');
  await submitReset(page, 303);
  assert.equal(requests.filter(r => r.method() === 'POST').length, 1);

  // Explicit per-element headers (as used by uploads) must remain authoritative.
  await resetForm(page);
  // A second tab and a history restore must not invalidate this form either.
  const token = await page.locator('input[name="csrf"]').inputValue();
  const second = await context.newPage();
  await second.goto(origin + '/register');
  assert.equal(await second.locator('input[name="csrf"]').inputValue(), token);
  await page.locator('a[href="/login"]').last().click();
  await page.locator('form[action="/login"]').waitFor();
  await page.goBack();
  await page.locator('form[action="/password-reset"]').waitFor();
  await page.evaluate(() => {
    const form = document.querySelector('form[action="/password-reset"]');
    const token = form.querySelector('input[name="csrf"]').value;
    form.setAttribute('hx-headers', JSON.stringify({ 'X-CSRF-Token': token }));
    form.querySelector('input[name="csrf"]').value = 'obsolete-form';
    document.querySelector('meta[name="csrf"]').content = 'obsolete-head';
  });
  await page.locator('input[name="email"]').fill('csrf-browser-missing@example.invalid');
  await submitReset(page, 303);

  // Reject bad tokens without replacing the form or replaying the mutation.
  await resetForm(page);
  await page.locator('input[name="email"]').fill('keep-this-input@example.invalid');
  await page.evaluate(() => {
    document.querySelector('form[action="/password-reset"] input[name="csrf"]').value = 'invalid-token';
  });
  const before = requests.filter(r => r.method() === 'POST').length;
  const denied = await submitReset(page, 403);
  assert.equal(denied.headers()['x-bikesnest-csrf-recovery'], 'reload-required');
  await page.locator('#csrf-recovery[role="alert"]').waitFor();
  assert.equal(await page.locator('input[name="email"]').inputValue(), 'keep-this-input@example.invalid');
  await page.waitForTimeout(200);
  assert.equal(requests.filter(r => r.method() === 'POST').length, before + 1,
    'a CSRF denial must not replay the mutation');
  await page.locator('#csrf-recovery a').first().click();
  await page.locator('#csrf-recovery').waitFor({ state: 'detached' });
  await page.locator('input[name="email"]').fill('keep-this-input@example.invalid');
  await submitReset(page, 303);

  // Real registration/login switch the token context to the new session.
  const accountEmail = 'csrf-browser-' + randomUUID() + '@example.invalid';
  await page.goto(origin + '/register');
  await page.locator('input[name="email"]').fill(accountEmail);
  await page.locator('input[name="password"]').fill('test-password-123');
  await submitForm(page, '/register', 303);
  await page.locator('form[action="/login"]').waitFor();
  const anonymousToken = await page.locator('input[name="csrf"]').inputValue();
  await page.locator('input[name="email"]').fill(accountEmail);
  await page.locator('input[name="password"]').fill('test-password-123');
  await submitForm(page, '/login', 303);
  await page.locator('form[action="/logout"]').first().waitFor({ state: 'attached' });
  const sessionToken = await page.locator('form[action="/logout"] input[name="csrf"]').first().inputValue();
  assert.notEqual(sessionToken, anonymousToken);
  assert.ok((await context.cookies()).some(c => c.name === 'session_id'));

  // Keep a genuinely stale session form in another tab while the first tab
  // follows the real logout redirect and renders the scoped homepage reader.
  const staleSession = await context.newPage();
  await staleSession.goto(origin + '/account');
  assert.equal(await staleSession.locator('form[action="/logout"] input[name="csrf"]').first().inputValue(), sessionToken);
  await submitForm(page, '/logout', 303);
  await page.waitForURL(origin + '/');
  await page.getByText('From destination to parked bike').waitFor();
  assert.ok(!(await context.cookies()).some(c => c.name === 'session_id'));
  await submitForm(staleSession, '/logout', 403);
  await staleSession.locator('#csrf-recovery').waitFor();
  await staleSession.close();
  await page.goto(origin + '/login');
  assert.equal(await page.locator('input[name="csrf"]').inputValue(), anonymousToken);

  // Force two genuinely cookieless first requests to complete before either
  // page is delivered. Both responses/CSRF tokens come from the real server.
  // This deterministic transport barrier avoids a timing-dependent race test.
  const cold = await browser.newContext({ locale: 'en-US' });
  cold.setDefaultTimeout(10_000);
  const firstResponses = [];
  await cold.route('**/*', async route => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.origin !== target.origin) return route.abort();
    if (url.pathname === '/login' && request.isNavigationRequest() && firstResponses.length < 2) {
      const response = await route.fetch({ headers: { ...request.headers(), cookie: '' } });
      firstResponses.push({ route, response });
      if (firstResponses.length === 2) {
        for (const held of firstResponses) await held.route.fulfill({ response: held.response });
      }
      return;
    }
    return route.continue();
  });
  const coldPages = await Promise.all([cold.newPage(), cold.newPage()]);
  await Promise.all(coldPages.map(tab => tab.goto(origin + '/login')));
  const coldTokens = await Promise.all(coldPages.map(tab => tab.locator('input[name="csrf"]').inputValue()));
  assert.notEqual(coldTokens[0], coldTokens[1]);
  const winningCookie = (await cold.cookies()).find(c => c.name === '__Host-csrf');
  assert.ok(winningCookie);
  const stale = coldPages[coldTokens.findIndex(value => value !== winningCookie.value)];
  let racePosts = 0;
  stale.on('request', request => { if (request.method() === 'POST') racePosts++; });
  await stale.locator('input[name="email"]').fill('cookie-race-missing@example.invalid');
  await stale.locator('input[name="password"]').fill('wrong-password');
  await submitForm(stale, '/login', 403);
  await stale.locator('#csrf-recovery').waitFor();
  assert.equal(await stale.locator('input[name="email"]').inputValue(), 'cookie-race-missing@example.invalid');
  await stale.waitForTimeout(200);
  assert.equal(racePosts, 1);
  await stale.locator('#csrf-recovery a').first().click();
  await stale.locator('#csrf-recovery').waitFor({ state: 'detached' });
  assert.equal(await stale.locator('input[name="csrf"]').inputValue(), winningCookie.value);
  await stale.locator('input[name="email"]').fill('cookie-race-missing@example.invalid');
  await stale.locator('input[name="password"]').fill('wrong-password');
  // Invalid credentials render the real login error at 200; CSRF now passes.
  await submitForm(stale, '/login', 200);
  assert.equal(racePosts, 2);
  await cold.close();

  // Native forms carry their own rendered token without the htmx hook.
  const native = await browser.newContext({ javaScriptEnabled: false, locale: 'en-US' });
  native.setDefaultTimeout(10_000);
  await native.route('**/*', route => {
    const url = new URL(route.request().url());
    return url.origin === target.origin ? route.continue() : route.abort();
  });
  const nativePage = await native.newPage();
  await nativePage.goto(origin + '/login');
  const nativeToken = await nativePage.locator('input[name="csrf"]').inputValue();
  assert.ok(nativeToken);
  assert.equal((await native.cookies()).find(c => c.name === '__Host-csrf').value, nativeToken);
  await nativePage.locator('input[name="email"]').fill('native-csrf-missing@example.invalid');
  await nativePage.locator('input[name="password"]').fill('wrong-password');
  // Invalid credentials reach the real handler at 200; CSRF rejection is 403.
  await submitForm(nativePage, '/login', 200);
  await nativePage.locator('#email-error[role="alert"]').waitFor();
  assert.equal(await nativePage.locator('#email-error').innerText(), 'Email or password is incorrect.');
  await native.close();

  // Fetch itself retains custom headers when following a same-origin 303.
  // Assert the hook does not add tokens to independently initiated safe reads.
  for (const request of requests.filter(r => !r.redirectedFrom() && /^(GET|HEAD|OPTIONS)$/.test(r.method()))) {
    assert.equal(request.headers()['x-csrf-token'], undefined,
      'safe requests must not carry the CSRF header');
  }
  const cookie = (await context.cookies()).find(c => c.name === '__Host-csrf');
  assert.ok(cookie && cookie.httpOnly && cookie.secure);
  assert.equal(cookie.sameSite, 'Lax');
  await context.close();
  console.log('Real Axum CSRF browser flows passed: boosted/native forms, explicit headers, rejection/recovery, tabs/history, full login/logout navigation and first-cookie race recovery.');
})().catch(error => {
  console.error(error);
  process.exitCode = 1;
}).finally(async () => {
  await browser?.close();
  clearTimeout(deadline);
});
