const assert = require('node:assert/strict');
const { chromium } = require('playwright');

const origin = process.env.BIKESNEST_PROFILE_TEST_ORIGIN;
const id = process.env.BIKESNEST_PROFILE_TEST_ID;
const timeout = setTimeout(() => {
  console.error('Real profile browser regression exceeded 45 seconds');
  process.exit(1);
}, 45_000);

async function positions(page) {
  const boxes = await Promise.all([
    page.locator('[data-od-id="parking-key-facts"]').boundingBox(),
    page.locator('[data-od-id="security-features"]').boundingBox(),
    page.locator('[data-od-id="parking-map"]').boundingBox(),
    page.locator('[data-od-id="parking-reviews"]').boundingBox(),
  ]);
  assert.ok(boxes.every(Boolean));
  assert.ok(boxes[0].y < boxes[1].y && boxes[1].y < boxes[2].y && boxes[2].y < boxes[3].y);
  return boxes.map(box => Math.round(box.y));
}

async function captureTop(page, path) {
  await page.evaluate(() => {
    window.scrollTo(0, 0);
    return new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  });
  await page.screenshot({ path, fullPage: true });
}

(async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const context = await browser.newContext({ locale: 'en-US', viewport: { width: 1280, height: 900 } });
    await context.route(origin + '/b12-test-style.json', route => route.fulfill({
      contentType: 'application/json',
      body: JSON.stringify({ version: 8, sources: {}, layers: [] }),
    }));
    await context.route('http://media.test.invalid/**', route => route.fulfill({
      contentType: 'image/svg+xml',
      body: '<svg xmlns="http://www.w3.org/2000/svg" width="800" height="450"><rect width="100%" height="100%" fill="#d7e9df"/><text x="40" y="80" font-size="40">B12 test photo</text></svg>',
    }));
    const page = await context.newPage();
    await page.goto(origin + '/parking/' + id);
    await page.locator('[data-od-id="parking-key-facts"]').waitFor();
    assert.match(await page.locator('a[href*="travelmode=bicycling"]').getAttribute('href'), /api=1.*travelmode=bicycling/);
    assert.equal(await page.locator('#map-single').count(), 1);
    assert.equal(await page.locator('.parking-gallery-grid > button').count(), 1);
    const desktop = await positions(page);
    await captureTop(page, '/tmp/b12-profile-desktop.png');
    await page.setViewportSize({ width: 390, height: 844 });
    const mobile = await positions(page);
    await captureTop(page, '/tmp/b12-profile-mobile.png');
    await page.locator('.parking-gallery summary').click();
    assert.equal(await page.locator('.parking-gallery details > div button').count(), 1);

    const native = await browser.newContext({ javaScriptEnabled: false, locale: 'en-US', viewport: { width: 390, height: 844 } });
    const nativePage = await native.newPage();
    await nativePage.goto(origin + '/parking/' + id);
    const gallerySummary = nativePage.locator('.parking-gallery summary');
    await gallerySummary.focus();
    await gallerySummary.press('Enter');
    assert.equal(await nativePage.locator('.parking-gallery details').getAttribute('open'), '');
    const history = nativePage.locator('a[href="/parking/' + id + '?tab=history"]');
    await history.focus();
    await Promise.all([
      nativePage.waitForURL('**?tab=history'),
      nativePage.keyboard.press('Enter'),
    ]);
    assert.equal(new URL(nativePage.url()).searchParams.get('tab'), 'history');
    assert.equal(await nativePage.locator('a[aria-current="page"]').innerText(), 'History');
    await native.close();
    await context.close();
    console.log('B12 real profile positions (desktop/mobile):', desktop, mobile);
  } finally {
    await browser.close();
    clearTimeout(timeout);
  }
})().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
