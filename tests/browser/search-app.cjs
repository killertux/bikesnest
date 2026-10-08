// Invoked only by search_browser_test.rs against its real loopback Axum app.
const assert = require('node:assert/strict');
const { chromium } = require('playwright');
const mutation = require('./mutation.cjs').scriptMutation('search-stale-response');

const origin = process.env.BIKESNEST_SEARCH_TEST_ORIGIN;
const target = new URL(origin);
assert.equal(target.protocol, 'http:');
assert.equal(target.hostname, '127.0.0.1');

function deferred() {
  let resolve;
  const promise = new Promise(done => { resolve = done; });
  return { promise, resolve };
}

function responseGate(name, matches) {
  return { name, matches, responseReady: deferred(), release: deferred(), completed: deferred(), used: false };
}

async function armHtmxEvent(page, eventName) {
  const marker = await page.evaluate(name => {
    window.__bikesnestBrowserEventMarker = (window.__bikesnestBrowserEventMarker || 0) + 1;
    const next = window.__bikesnestBrowserEventMarker;
    document.addEventListener(name, () => {
      window.__bikesnestBrowserEventReached = next;
    }, { once: true });
    return next;
  }, eventName);
  return () => page.waitForFunction(value => window.__bikesnestBrowserEventReached === value, marker);
}

async function armHtmxRequestFinally(page, expectedTypes) {
  const marker = await page.evaluate(types => {
    window.__bikesnestBrowserEventMarker = (window.__bikesnestBrowserEventMarker || 0) + 1;
    const next = window.__bikesnestBrowserEventMarker;
    const handler = event => {
      const url = new URL(event.detail.ctx.request.action, location.href);
      if (url.searchParams.getAll('type').join(',') === types.join(',')) {
        window.__bikesnestBrowserEventReached = next;
        document.removeEventListener('htmx:finally:request', handler);
      }
    };
    document.addEventListener('htmx:finally:request', handler);
    return next;
  }, expectedTypes);
  return () => page.waitForFunction(value => window.__bikesnestBrowserEventReached === value, marker);
}

async function installResponseBodyGate(page, expectedTypes) {
  await page.evaluate(types => {
    const originalFetch = window.fetch.bind(window);
    window.__bikesnestReleaseResponseBody = null;
    window.__bikesnestResponseBodyWaiting = false;
    window.fetch = async function (...args) {
      const response = await originalFetch(...args);
      const url = new URL(response.url);
      if (url.pathname === '/search' && url.searchParams.getAll('type').join(',') === types.join(',')) {
        window.fetch = originalFetch;
        const readText = response.text.bind(response);
        response.text = async function () {
          window.__bikesnestResponseBodyWaiting = true;
          await new Promise(resolve => { window.__bikesnestReleaseResponseBody = resolve; });
          return readText();
        };
      }
      return response;
    };
  }, expectedTypes);
}

async function fixtureResultNames(page) {
  return page.locator('#results [role="listitem"]').evaluateAll(items =>
    items.map(item => item.innerText.match(/Search browser[^\n]*/)?.[0]).filter(Boolean)
  );
}
const deadline = setTimeout(() => {
  console.error('Real-app search browser regression exceeded 90 seconds');
  process.exit(1);
}, 90_000);

(async () => {
  const browser = await chromium.launch({ headless: true });
  try {
    const context = await browser.newContext({ locale: 'en-US' });
    context.setDefaultTimeout(10_000);
    const page = await context.newPage();
    const requests = [];
    const mapModuleRequests = [];
    const mapModuleFailures = [];
    const pageErrors = [];
    const gates = [];
    page.on('pageerror', error => pageErrors.push(error.message));
    page.on('request', request => {
      const url = new URL(request.url());
      if (url.origin === target.origin && /maplibre-(?:loader|gl(?:-shared|-worker)?)\.mjs$/.test(url.pathname)) {
        mapModuleRequests.push(url.pathname);
      }
    });
    page.on('requestfailed', request => {
      const url = new URL(request.url());
      if (url.origin === target.origin && /\.mjs$/.test(url.pathname)) {
        mapModuleFailures.push(`${url.pathname}: ${request.failure()?.errorText || 'failed'}`);
      }
    });
    await context.route('**/*', async route => {
      const request = route.request();
      const url = new URL(request.url());
      if (url.origin !== target.origin) return route.abort();
      if (await mutation.intercept(route)) return;
      if (request.method() !== 'GET' || url.pathname !== '/search' || !request.headers()['hx-request']) {
        return route.continue();
      }
      requests.push(url);
      const gate = gates.find(candidate => !candidate.used && candidate.matches(url));
      try {
        const response = await route.fetch();
        if (gate) {
          gate.used = true;
          gate.responseReady.resolve();
          await gate.release.promise;
        }
        await route.fulfill({ response });
      } catch (error) {
        const failure = request.failure();
        if (failure && failure.errorText === 'net::ERR_ABORTED') return;
        throw error;
      } finally {
        if (gate) gate.completed.resolve();
      }
    });

    await page.goto(origin + '/search?lat=-33.930000&lon=-70.630000&radius=1000');
    await page.locator('#search-filter-panel').waitFor();
    await page.locator('[aria-controls="search-map-panel"]').click();
    await page.locator('[data-map-status]').waitFor({ state: 'hidden' });
    assert.equal(await page.evaluate(() => window.maplibregl.accessToken), undefined,
      'the MapLibre/OpenFreeMap path remains token-free');
    assert.ok(mapModuleRequests.some(path => /^\/static\/h\/[0-9a-f]{10}\/js\/maplibre-loader\.mjs$/.test(path)),
      'the real router serves the small loader through its immutable hashed URL');
    for (const path of [
      '/static/vendor/maplibre-gl.mjs',
      '/static/vendor/maplibre-gl-shared.mjs',
      '/static/vendor/maplibre-gl-worker.mjs',
    ]) {
      assert.ok(mapModuleRequests.includes(path), `the coherent MapLibre module graph requests ${path}`);
    }
    assert.deepEqual(mapModuleFailures, []);
    assert.deepEqual(pageErrors, []);
    mutation.assertApplied();
    const rack = page.locator('#search-filter-form input[name="type"][value="rack"]');
    if (!await rack.isVisible()) await page.getByRole('button', { name: 'Filters' }).click();
    await rack.waitFor({ state: 'visible' });

    // A sort action produces one sort-only GET. The immediately following
    // filters must compose its still-unswapped value into their own request.
    const sortGate = responseGate('sort', url =>
      url.searchParams.get('sort') === 'distance' && url.searchParams.getAll('type').length === 0
    );
    const rackGate = responseGate('rack', url =>
      url.searchParams.getAll('type').join(',') === 'rack' && url.searchParams.getAll('security').length === 0
    );
    const indoorGate = responseGate('indoor', url =>
      url.searchParams.getAll('type').join(',') === 'rack,indoor' && url.searchParams.getAll('security').length === 0
    );
    const latestGate = responseGate('latest security', url =>
      url.searchParams.getAll('type').join(',') === 'rack,indoor' &&
      url.searchParams.getAll('security').join(',') === 'cctv,well_lit'
    );
    gates.push(sortGate, rackGate, indoorGate, latestGate);
    await page.locator('#search-sort-form select[name="sort"]').selectOption('distance');
    await sortGate.responseReady.promise;
    if (!await rack.isVisible()) {
      await page.getByRole('button', { name: 'Filters' }).click();
      await rack.waitFor({ state: 'visible' });
    }
    const waitForSortFinally = await armHtmxRequestFinally(page, []);
    await rack.check();
    await Promise.all([rackGate.responseReady.promise, waitForSortFinally()]);
    await page.locator('#search-filter-form input[name="type"][value="indoor"]').check();
    await indoorGate.responseReady.promise;
    const waitForIndoorFinally = await armHtmxRequestFinally(page, ['rack', 'indoor']);
    await page.locator('#search-filter-form input[name="security"][value="cctv"]').check();
    await waitForIndoorFinally();
    await page.locator('#search-filter-form input[name="security"][value="well_lit"]').check();
    await latestGate.responseReady.promise;

    // Let an aborted older request finish first. In htmx 4 this exercises the
    // queue-owner `finally` race while the newest response is still held.
    sortGate.release.resolve();
    await sortGate.completed.promise;

    const waitForLatestSettle = await armHtmxEvent(page, 'htmx:after:settle');
    latestGate.release.resolve();
    await Promise.all([latestGate.completed.promise, waitForLatestSettle()]);

    // Deliver the remaining older real responses after the newest response
    // has committed. Neither may alter its main or out-of-band state.
    const waitForRackFinally = await armHtmxRequestFinally(page, ['rack']);
    indoorGate.release.resolve();
    rackGate.release.resolve();
    await Promise.all([
      rackGate.completed.promise,
      indoorGate.completed.promise,
      waitForRackFinally(),
    ]);

    const sortOnly = requests.filter(url =>
      url.searchParams.get('sort') === 'distance' &&
      url.searchParams.getAll('type').length === 0 &&
      url.searchParams.getAll('security').length === 0
    );
    assert.equal(sortOnly.length, 1, 'one sort intent issues one sort-only GET');
    const latest = requests.at(-1);
    assert.equal(latest.searchParams.get('sort'), 'distance');
    assert.deepEqual(latest.searchParams.getAll('type'), ['rack', 'indoor']);
    assert.deepEqual(latest.searchParams.getAll('security'), ['cctv', 'well_lit']);
    const latestNames = await fixtureResultNames(page);
    assert.equal(latestNames.length, 20);
    assert.ok(latestNames.includes('Search browser rack'));
    assert.ok(!latestNames.includes('Search browser indoor'));
    assert.ok(!latestNames.includes('Search browser locker'));
    const committed = new URL(page.url());
    assert.equal(committed.searchParams.get('sort'), 'distance');
    assert.deepEqual(committed.searchParams.getAll('type'), ['rack', 'indoor']);
    assert.deepEqual(committed.searchParams.getAll('security'), ['cctv', 'well_lit']);
    assert.equal(await page.locator('#search-sort-form select[name="sort"]').inputValue(), 'distance');
    assert.equal(await page.locator('#search-filter-state input[name="sort"]').inputValue(), 'distance');
    assert.deepEqual(await page.locator('#search-sort-state input[name="security"]').evaluateAll(inputs => inputs.map(input => input.value)), ['cctv,well_lit']);
    assert.ok(await page.locator('#search-filter-form input[name="type"][value="rack"]').isChecked());
    assert.ok(await page.locator('#search-filter-form input[name="type"][value="indoor"]').isChecked());
    const latestClear = new URL(await page.locator('[data-search-clear]').first().getAttribute('href'), origin);
    assert.equal(latestClear.searchParams.get('lat'), '-33.93');
    assert.equal(latestClear.searchParams.has('type'), false);
    assert.deepEqual(new URL(await page.getByRole('link', { name: 'Next page' }).getAttribute('href'), origin).searchParams.getAll('security'), ['cctv,well_lit']);

    // Hold a real filter response, then clear. The late response includes OOB
    // controls, so this catches regressions where it repopulates cleared state.
    const clearGate = responseGate('pending filter', url =>
      url.searchParams.getAll('type').includes('parking_facility')
    );
    gates.push(clearGate);
    await page.locator('#search-filter-form input[name="type"][value="parking_facility"]').check({ force: true });
    await clearGate.responseReady.promise;
    await page.getByRole('link', { name: 'Clear all' }).click();
    await page.waitForFunction(() => location.search === '?lat=-33.93&lon=-70.63');
    clearGate.release.resolve();
    await clearGate.completed.promise;
    assert.equal(await page.locator('#search-sort-form select[name="sort"]').inputValue(), 'recommended');
    assert.equal(await page.locator('#search-filter-form input[name="type"]:checked').count(), 0);
    assert.equal(await page.locator('#search-filter-form input[name="security"]:checked').count(), 0);
    assert.equal(await page.locator('#search-filter-state input[name="type"]').count(), 0);
    assert.equal(await page.locator('#search-filter-state input[name="security"]').count(), 0);
    assert.ok((await page.locator('#results').innerText()).includes('Search browser locker'));
    // Filters replace the current history entry instead of pushing one per
    // checkbox; the pager still pushes, which gives the history restoration
    // below an entry to return to.
    const waitForPagerSettle = await armHtmxEvent(page, 'htmx:after:settle');
    await page.getByRole('link', { name: 'Next page' }).click();
    await waitForPagerSettle();
    const historyLength = await page.evaluate(() => history.length);
    const waitForRackSettle = await armHtmxEvent(page, 'htmx:after:settle');
    await page.locator('#search-filter-form input[name="type"][value="rack"]').check();
    await waitForRackSettle();
    assert.equal(await page.evaluate(() => history.length), historyLength,
      'a filter change replaces the history entry');

    // Hold an older fragment across a full history body restoration. Its
    // disconnected source will make htmx retarget lifecycle events to the
    // document; releasing it must not touch the restored page's OOB or URL.
    await installResponseBodyGate(page, ['rack', 'indoor']);
    const waitForHistoryRequestFinally = await armHtmxRequestFinally(page, ['rack', 'indoor']);
    await page.locator('#search-filter-form input[name="type"][value="indoor"]').check();
    await page.waitForFunction(() => window.__bikesnestResponseBodyWaiting === true);

    // htmx history restores a full page: selected controls and the browser URL
    // must reflect the prior latest intent rather than a stale OOB fragment.
    await page.goBack();
    await page.waitForFunction(() =>
      location.search === '?lat=-33.93&lon=-70.63' &&
      document.querySelectorAll('#search-filter-form input[name="type"]:checked').length === 0
    );
    await page.evaluate(() => window.__bikesnestReleaseResponseBody());
    await waitForHistoryRequestFinally();
    assert.equal(await page.locator('#search-sort-form select[name="sort"]').inputValue(), 'recommended');
    assert.equal(await page.locator('#search-filter-form input[name="type"]:checked').count(), 0);
    assert.equal(await page.locator('#search-filter-form input[name="security"]:checked').count(), 0);
    assert.equal(await page.locator('#search-filter-state input[name="type"]').count(), 0);
    assert.equal(await page.locator('#search-filter-state input[name="security"]').count(), 0);
    assert.ok((await page.locator('#results').innerText()).includes('Search browser locker'));
    assert.equal(new URL(page.url()).searchParams.has('type'), false);
    await page.getByRole('button', { name: 'Filters' }).click();
    const waitForRestoredFollowup = await armHtmxEvent(page, 'htmx:after:settle');
    await page.locator('#search-filter-form input[name="type"][value="rack"]').check();
    await waitForRestoredFollowup();
    assert.deepEqual(new URL(page.url()).searchParams.getAll('type'), ['rack']);
    assert.equal(await page.locator('#search-filter-form input[name="security"]:checked').count(), 0,
      'a follow-up interaction must remain in restored state');

    // Commit a new destination through the persistent htmx form. The clear
    // link still has its startup attributes until clearSearchState rebuilds it
    // from the fragment's current OOB mirrors.
    await page.locator('#q').fill('Changed destination');
    const waitForDestinationSettle = await armHtmxEvent(page, 'htmx:after:settle');
    await page.locator('#search-form').evaluate(form => form.requestSubmit());
    await waitForDestinationSettle();
    assert.equal(new URL(page.url()).searchParams.get('q'), 'Changed destination');
    assert.equal(new URL(page.url()).searchParams.has('lat'), false);
    assert.equal(await page.locator('#search-query-state input[name="type"]').inputValue(), 'rack');

    const waitForPersistentClear = await armHtmxEvent(page, 'htmx:after:settle');
    await page.locator('#search-filter-panel [data-search-clear]').click();
    await waitForPersistentClear();
    assert.equal(new URL(page.url()).searchParams.get('q'), 'Changed destination');
    assert.deepEqual([...new URL(page.url()).searchParams.keys()], ['q']);
    assert.equal(await page.locator('#search-filter-form input[name="type"]:checked').count(), 0);
    assert.equal(await page.locator('#search-filter-state input[name="type"]').count(), 0);

    // The empty-result fragment has a second clear action. Exercise it after
    // applying new controls and prove it keeps the fragment-committed place.
    const waitForEmptySort = await armHtmxEvent(page, 'htmx:after:settle');
    await page.locator('#search-sort-form select[name="sort"]').selectOption('distance');
    await waitForEmptySort();
    const waitForEmptyFilter = await armHtmxEvent(page, 'htmx:after:settle');
    await page.locator('#search-filter-form input[name="type"][value="parking_facility"]').check();
    await waitForEmptyFilter();
    const emptyClear = page.locator('#results [data-search-clear]');
    await emptyClear.waitFor();
    const waitForEmptyClear = await armHtmxEvent(page, 'htmx:after:settle');
    await emptyClear.click();
    await waitForEmptyClear();
    assert.equal(new URL(page.url()).searchParams.get('q'), 'Changed destination');
    assert.deepEqual([...new URL(page.url()).searchParams.keys()], ['q']);
    assert.equal(await page.locator('#search-sort-form select[name="sort"]').inputValue(), 'recommended');
    assert.equal(await page.locator('#search-filter-form input[name="type"]:checked').count(), 0);
    assert.equal(await page.locator('#search-filter-form input[name="security"]:checked').count(), 0);
    assert.equal(await page.locator('#search-filter-state input[name="type"]').count(), 0);
    await page.locator('#search-filter-form input[name="type"][value="rack"]').check();
    await page.waitForFunction(() => new URL(location.href).searchParams.get('type') === 'rack');
    assert.equal(new URL(page.url()).searchParams.get('q'), 'Changed destination');
    assert.equal(await page.locator('#search-filter-form input[name="security"]:checked').count(), 0);

    // The real next-page link and htmx history cache restore the last committed
    // URL and controls, including removal of the cursor when navigating back.
    await page.goto(origin + '/search?lat=-33.930000&lon=-70.630000&radius=1000');
    const nextPage = page.getByRole('link', { name: 'Next page' });
    await nextPage.waitFor();
    await nextPage.click();
    await page.waitForFunction(() => new URL(location.href).searchParams.has('cursor'));
    assert.equal(await page.locator('#search-filter-form input[name="type"]:checked').count(), 0);
    await page.goBack();
    await page.waitForFunction(() =>
      !new URL(location.href).searchParams.has('cursor') &&
      document.querySelectorAll('#search-filter-form input[name="type"]:checked').length === 0
    );
    await page.getByRole('link', { name: 'Next page' }).waitFor();

    // Native fallback: operate and submit the rendered forms with JavaScript
    // disabled, rather than loading a hand-built query string.
    const native = await browser.newContext({ javaScriptEnabled: false, locale: 'en-US' });
    await native.route('**/*', route => {
      const url = new URL(route.request().url());
      return url.origin === target.origin ? route.continue() : route.abort();
    });
    const nativePage = await native.newPage();
    await nativePage.goto(origin + '/search?lat=-33.930000&lon=-70.630000&radius=1000');
    await nativePage.locator('#search-filter-form input[name="type"][value="rack"]').check();
    await nativePage.locator('#search-filter-form input[name="type"][value="indoor"]').check();
    await nativePage.locator('#search-filter-form input[name="security"][value="cctv"]').check();
    await nativePage.locator('#search-filter-form input[name="security"][value="well_lit"]').check();
    await nativePage.locator('#search-filter-form noscript button').click();
    await nativePage.waitForLoadState();
    let nativeUrl = new URL(nativePage.url());
    assert.deepEqual(nativeUrl.searchParams.getAll('type'), ['rack', 'indoor']);
    assert.deepEqual(nativeUrl.searchParams.getAll('security'), ['cctv', 'well_lit']);
    assert.ok(await nativePage.locator('#search-filter-form input[name="type"][value="rack"]').isChecked());
    assert.ok(await nativePage.locator('#search-filter-form input[name="type"][value="indoor"]').isChecked());
    assert.ok((await fixtureResultNames(nativePage)).includes('Search browser rack'));
    assert.ok(!(await fixtureResultNames(nativePage)).includes('Search browser indoor'));

    await nativePage.locator('#search-sort-form select[name="sort"]').selectOption('distance');
    await nativePage.locator('#search-sort-form noscript button').click();
    await nativePage.waitForLoadState();
    nativeUrl = new URL(nativePage.url());
    assert.equal(nativeUrl.searchParams.get('sort'), 'distance');
    assert.deepEqual(nativeUrl.searchParams.getAll('type'), ['rack,indoor']);
    assert.deepEqual(nativeUrl.searchParams.getAll('security'), ['cctv,well_lit']);
    assert.equal(await nativePage.locator('#search-sort-form select[name="sort"]').inputValue(), 'distance');

    await nativePage.locator('#search-filter-panel [data-search-clear]').click();
    await nativePage.waitForLoadState();
    nativeUrl = new URL(nativePage.url());
    assert.equal(nativeUrl.searchParams.has('type'), false);
    assert.equal(nativeUrl.searchParams.has('security'), false);
    assert.equal(nativeUrl.searchParams.has('sort'), false);
    const nativeNext = nativePage.getByRole('link', { name: 'Next page' });
    await nativeNext.click();
    await nativePage.waitForLoadState();
    assert.ok(new URL(nativePage.url()).searchParams.has('cursor'));
    await nativePage.goBack();
    await nativePage.waitForLoadState();
    assert.equal(new URL(nativePage.url()).searchParams.has('cursor'), false);
    await nativePage.getByRole('link', { name: 'Next page' }).waitFor();
    await native.close();
    await context.close();
    console.log('Real Axum search browser flow passed: repeated filters, one sort GET, delayed latest intent, clear, history and native fallback.');
  } finally {
    await browser.close();
    clearTimeout(deadline);
  }
})().catch(error => {
  console.error(error);
  process.exitCode = 1;
});
