'use strict';

const assert = require('node:assert/strict');
const fs = require('node:fs');
const os = require('node:os');
const path = require('node:path');
const { chromium } = require('playwright');

async function main() {
  const fixtures = JSON.parse(fs.readFileSync(0, 'utf8'));
  assert.equal(fixtures.length, 16);
  // Keep only screenshots, in an exclusively created artifact directory.
  const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'b14-email-visual-'));
  const browser = await chromium.launch({ headless: true });
  let checked = 0;
  try {
    for (const [mode, width, colorScheme] of [
      ['mobile', 390, 'light'],
      ['desktop', 1280, 'light'],
      ['dark', 390, 'dark'],
    ]) {
      const context = await browser.newContext({
        viewport: { width, height: 844 }, colorScheme, serviceWorkers: 'block',
      });
      const requests = [];
      await context.route('**/*', route => {
        requests.push(route.request().url());
        return route.abort();
      });
      try {
        for (const fixture of fixtures) {
          const page = await context.newPage();
          page.setDefaultTimeout(5000);
          await page.setContent(fixture.html, { waitUntil: 'load' });
          assert.equal(await page.locator('html').getAttribute('lang'), fixture.lang);
          assert.equal(await page.locator('main').count(), 1);
          assert.equal(await page.locator('h1').innerText(), 'BikesNest');
          assert.equal(await page.locator('img, script, iframe, link, object, embed').count(), 0);
          const links = page.locator('a');
          assert.equal(await links.count(), 2);
          for (const anchor of await links.all()) {
            assert.equal(await anchor.getAttribute('href'), fixture.link);
          }
          assert.equal(await links.last().innerText(), fixture.link);
          assert.ok((await links.first().innerText()).trim().length > 3);
          assert.equal(await page.locator('h2').innerText(), await links.first().innerText());
          assert.ok((await links.first().boundingBox()).height >= 44, 'CTA must meet the audit touch-height target');
          await links.first().focus();
          assert.equal(await links.first().evaluate(el => el === document.activeElement), true);
          const text = await page.locator('body').innerText();
          assert.ok(text.length > 100);
          assert.ok(!/⟨i18n\?⟩|never-render|\{(?:app|link|expires)\}/.test(text));
          assert.ok(fixture.text.includes(fixture.link));
          if (fixture.expiry) {
            assert.ok(text.includes(fixture.expiry));
            assert.ok(fixture.text.includes(fixture.expiry));
          } else {
            assert.ok(!text.includes('2030-01-02'));
            assert.ok(!/expires at|expira em|24 hours|24 horas|1 hour|1 hora/i.test(text));
          }
          assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true, fixture.name);
          assert.deepEqual(requests, [], 'email unexpectedly attempted an external resource');
          if (fixture.name.endsWith('verify-expiry') || fixture.name.endsWith('email_changed-no-expiry')) {
            // Focusing a CTA can scroll even a short document. Capture from
            // the top, after layout has settled, so the brand isn't cropped.
            await page.evaluate(async () => {
              window.scrollTo(0, 0);
              await new Promise(resolve => requestAnimationFrame(() => requestAnimationFrame(resolve)));
            });
            assert.equal(await page.evaluate(() => window.scrollY), 0);
            assert.equal(await page.locator('h1').isVisible(), true);
            const heading = await page.locator('h1').boundingBox();
            assert.ok(heading.y >= 0 && heading.width > 0 && heading.height > 0);
            await page.screenshot({ path: path.join(artifacts, `${mode}-${fixture.name}.png`), fullPage: true });
          }
          checked += 1;
          await page.close();
        }
      } finally {
        await context.close();
      }
    }
  } finally {
    await browser.close();
  }
  console.log(`PASS: ${checked} offline document/viewport checks. Screenshots: ${artifacts}`);
}

main().catch(error => { console.error(error); process.exitCode = 1; });
