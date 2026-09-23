const { test, before, after } = require('node:test');
const assert = require('node:assert/strict');
const { readFile } = require('node:fs/promises');
const { resolve } = require('node:path');
const { createServer } = require('node:http');
const { chromium } = require('playwright');

const root = resolve(__dirname, '../..');
const source = (path) => readFile(resolve(root, path), 'utf8');
let browser, server, origin;
const consumers = {
  search: ['search', '<div id="map" style="height:300px"></div><div data-map-status data-loading="Loading map" data-failed="Map failed" data-retry="Retry map">Loading map</div><script id="search-data" type="application/json">{"items":[],"total":0}</script>'],
  details: ['parking_details', '<div id="map-single" data-lat="-25" data-lon="-49"></div><div data-map-status data-loading="Loading map" data-failed="Map failed" data-retry="Retry map">Loading map</div>'],
  new: ['parking_new', '<input id="lat"><input id="lon"><div id="pin-map" data-default-lat="-25" data-default-lon="-49"></div><div data-map-status data-loading="Loading map" data-failed="Map failed" data-retry="Retry map">Loading map</div>'],
  edit: ['parking_edit', '<input id="lat" value="-25"><input id="lon" value="-49"><div id="pin-map"></div><div data-map-status data-loading="Loading map" data-failed="Map failed" data-retry="Retry map">Loading map</div>'],
  proposals: ['moderation_proposals', '<div class="proposal-map" data-lat="-25" data-lon="-49" data-current-lat="-26" data-current-lon="-50"></div><div class="proposal-map" data-lat="-27" data-lon="-48"></div>'],
};

// Use the real page asset declarations; only server-side business data and
// third-party map services are replaced. No database or external API is used.
function renderAssets(html, provider) {
  return html.replace(/{% if layout.uses_maplibre\(\) %}([\s\S]*?){% endif %}/g, (_, block) => {
    const branches = block.split(/{% else if layout.uses_(?:mapbox|google_maps)\(\) %}/);
    return branches[{ maplibre: 0, mapbox: 1, google: 2 }[provider]] || '';
  }).replace(/{{ layout.asset\("([^"]+)"\) }}/g, '/static/$1')
    .replace(/{{ layout.csp_nonce }}/g, 'test-nonce');
}

async function pageHtml(kind, provider) {
  const base = await source('templates/layouts/base.html');
  const scripts = renderAssets(base.split('</head>')[0].match(/<script[^>]+><\/script>/g).join('\n'), provider);
  const header = base.match(/<header id="top"[\s\S]*?>/)[0];
  const menu = base.match(/<div id="mobile-menu"[^>]*>/)[0];
  const button = base.match(/<button\s+@click="toggle" :aria-expanded="open"[\s\S]*?<\/button>/)[0]
    .replace(/{{.*?}}/g, 'menu');
  let content = '', assets = '';
  if (consumers[kind]) {
    const [template, markup] = consumers[kind];
    content = markup;
    const html = await source('templates/pages/' + template + '.html');
    assets = renderAssets(html.split('{% block scripts %}')[1].split('{% endblock %}')[0], provider);
  }
  if (kind === 'attack') {
    content = '<script>window.injectedScriptRan=true</script>' +
      '<img src=x onerror="window.injectedHandlerRan=true">' +
      '<template data-map-assets><script src="https://evil.invalid/payload.js"></script></template>';
  }
  if (kind === 'gps-home') {
    content = '<div id="gps" x-data="homeHero" data-location-loading="loading" data-location-unavailable="unavailable" data-location-timeout="timeout" data-location-denied="denied">' +
      '<input id="q"><button id="locate" @click="locate" :disabled="locating">locate</button><p x-text="locationMessage"></p></div>';
  }
  if (kind === 'gps-search') {
    content = '<div id="gps" x-data="searchFilters" data-location-loading="loading" data-location-unavailable="unavailable" data-location-timeout="timeout" data-location-denied="denied">' +
      '<input id="q"><button id="locate" @click="locate" :disabled="locating">locate</button><button id="fallback" @click="focusDestination">fallback</button><p x-text="locationMessage"></p></div>';
  }
  if (kind === 'gps-pin') {
    content = '<div id="gps" x-data="pinPicker" data-empty="empty" data-locate-failed="failed" data-location-loading="loading" data-location-unavailable="unavailable" data-location-timeout="timeout" data-location-denied="denied">' +
      '<input id="address"><input id="lat"><input id="lon"><div data-lat-input="lat" data-lon-input="lon"></div><button id="locate" @click="useLocation" :disabled="locating">locate</button><p x-text="message"></p></div>';
  }
  if (kind === 'form-start') {
    content = '<form id="rejected-form" action="/form-error/' + provider + '" method="post">' +
      '<button type="submit">Submit invalid form</button></form>';
  }
  if (kind === 'form-error') {
    content = '<div style="height:1100px"></div><details id="error-group" open>' +
      '<summary>Price details</summary><label>Price<input id="invalid-price" data-form-error-focus aria-invalid="true"></label></details>' +
      '<div id="unrelated-fragment"></div>';
  }
  return '<!DOCTYPE html><html><head><title>' + kind + '</title>' +
    '<link rel="stylesheet" href="/static/css/app.css">' + scripts +
    '</head><body hx-boost:inherited="true" data-document-lang="pt-BR" data-document-title="' + kind + '" data-document-canonical="/' + kind + '" data-document-description="' + kind + ' description" data-map-provider="' + provider + '">' +
    header + button + menu + '<a id="menu-link" href="/details/' + provider + '">details</a></div></header>' +
    '<div x-data="accountMenu"><button id="account-toggle" @click="toggle">account</button><div id="account-menu" x-show="open" x-cloak>account items</div></div><p id="page-change-announcement" aria-live="polite" data-page-changed="Page changed"></p><div hidden data-document-meta data-lang="pt-BR" data-title="' + kind + '" data-canonical="/' + kind + '" data-description="' + kind + ' description"></div>' +
    '<main id="content"><h1>' + kind + '</h1>' + content + '</main>' +
    Object.keys({ plain: 1, ...consumers }).map(k => '<a id="go-' + k + '" href="/' + k + '/' + provider + '">' + k + '</a>').join(' ') +
    assets + '</body></html>';
}

const providerStub = `
if (document.body.dataset.mapProvider !== 'google' && !window.testSdk) throw Error('sdk_not_ready');
window.providerLoads = (window.providerLoads || 0) + 1;
window.created = window.created || 0;
window.destroyed = window.destroyed || 0;
window.resizeCalls = window.resizeCalls || [];
window.flyCalls = window.flyCalls || 0;
window.jumpCalls = window.jumpCalls || [];
window.BikesNestMapProvider = {
 ready: function () { return Promise.resolve(); },
 createMap: function(el) {
  window.created++;
  var mapId = window.created;
  el.appendChild(document.createElement('canvas'));
  var map = {
   onLoad: function(fn) { setTimeout(function() { if(el.isConnected) fn(); }, 0); },
   onClick: function(fn) { map.click = fn; }, onMoveEnd: function() {}, onError: function(fn) { map.error = fn; },
   addMarker: function(opts) { return { remove: function(){}, setPosition: function(){}, getElement: function(){ return opts.element; } }; },
   resize: function(){ window.resizeCalls.push(mapId); }, fitBounds: function(){}, jumpTo: function(){ window.jumpCalls.push(mapId); },
   flyTo: function(){ window.flyCalls++; }, easeTo: function(){}, getBounds: function(){ return null; }, getZoom: function(){return 14;},
   destroy: function(){ window.destroyed++; }
  };
  return map;
 }
};`;

before(async () => {
  server = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://localhost');
      if (url.pathname.startsWith('/static/')) {
        const path = url.pathname.slice(1);
        if (path.endsWith('js/maplibre-loader.mjs')) {
          await new Promise(r => setTimeout(r, 250));
          res.setHeader('Content-Type', 'text/javascript');
          return res.end('window.testSdk = true; window.maplibregl = {};');
        }
        if (/map-provider-.*\.js$/.test(path)) {
          if (url.searchParams.has('actual')) {
            res.setHeader('Content-Type', 'text/javascript');
            return res.end(await source('web/' + path));
          }
          await new Promise(r => setTimeout(r, 150));
          res.setHeader('Content-Type', 'text/javascript');
          return res.end(providerStub);
        }
        if (/vendor\/maplibre-gl(?:-shared|-worker)?\.mjs$/.test(path)) {
          res.setHeader('Content-Type', 'text/javascript');
          return res.end(await source('web/' + path));
        }
        if (/vendor\/mapbox-gl\.js$/.test(path)) {
          if (url.searchParams.has('actual')) {
            res.setHeader('Content-Type', 'text/javascript');
            return res.end(await source('web/' + path));
          }
          await new Promise(r => setTimeout(r, 250));
          res.setHeader('Content-Type', 'text/javascript');
          return res.end('window.testSdk = true;');
        }
        res.setHeader('Content-Type', path.endsWith('.js') ? 'text/javascript' : 'text/css');
        return res.end(await source('web/' + path));
      }
      if (url.pathname === '/test-map-style-empty.json' || url.pathname === '/test-map-style-failed.json') {
        const failed = url.pathname.endsWith('failed.json');
        res.setHeader('Content-Type', 'application/json');
        return res.end(JSON.stringify(failed
          ? {version: 8, sources: {broken: {type: 'vector', tiles: ['/missing/{z}/{x}/{y}.pbf']}}, layers: [{id: 'broken', type: 'circle', source: 'broken', 'source-layer': 'missing'}]}
          : {version: 8, sources: {}, layers: []}));
      }
      const [, kind = 'plain', provider = 'google'] = url.pathname.split('/');
      const mapboxConnect = (kind === 'sdk' || kind === 'sdk-fail') && provider === 'mapbox'
        ? ' https://api.mapbox.com https://events.mapbox.com'
        : '';
      res.setHeader('Content-Type', 'text/html');
      res.setHeader('Content-Security-Policy', "script-src 'nonce-test-nonce' 'strict-dynamic'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'" + mapboxConnect + "; worker-src 'self' blob:; object-src 'none'");
      if (kind === 'sdk' || kind === 'sdk-fail') {
        const global = provider === 'mapbox' ? 'mapboxgl' : 'maplibregl';
        const style = kind === 'sdk-fail'
          ? '{version:8,sources:{broken:{type:"vector",tiles:["/missing/{z}/{x}/{y}.pbf"]}},layers:[{id:"broken",type:"circle",source:"broken","source-layer":"missing"}]}'
          : '{version:8,sources:{},layers:[]}';
        if (provider === 'maplibre') {
          return res.end('<!doctype html><div id="map" style="width:200px;height:200px"></div>' +
            '<script type="module" nonce="test-nonce">import * as sdk from "/static/vendor/maplibre-gl.mjs";' +
            'sdk.setWorkerUrl("/static/vendor/maplibre-gl-worker.mjs");' +
            'window.actualMap=new sdk.Map({container:"map",style:' + style + ',center:[0,0],zoom:1,attributionControl:false});' +
            'window.actualMap.on("load",function(){window.sdkReady=true});window.actualMap.on("error",function(){window.sdkFailed=true})</script>');
        }
        return res.end('<!doctype html><body data-map-style-url="/test-map-style-' + (kind === 'sdk-fail' ? 'failed' : 'empty') + '.json"' +
          ' data-map-access-token="browser-mapbox-test-token"><div id="map" style="width:200px;height:200px"></div>' +
          '<script nonce="test-nonce" src="/static/vendor/mapbox-gl.js?actual=1"></script>' +
          '<script nonce="test-nonce" src="/static/js/map-provider-mapbox.js?actual=1"></script>' +
          '<script nonce="test-nonce">var adapted=window.BikesNestMapProvider.createMap(document.getElementById("map"),{center:{lat:0,lon:0},zoom:1,navigation:false});' +
          'window.actualMap=adapted.raw;window.sdkToken=' + global + '.accessToken;' +
          'window.actualMap.on("load",function(){window.sdkReady=true});window.actualMap.on("error",function(){window.sdkFailed=true})</script>');
      }
      let html = (await pageHtml(kind === 'english' ? 'plain' : kind, provider))
        .replace(/<script(?=\s+defer)/g, '<script nonce="test-nonce"');
      if (kind === 'english') {
        html = html.replace('<html>', '<html lang="en">').replace('<title>plain</title>', '<title>English page</title>')
          .replaceAll('data-document-lang="pt-BR"', 'data-document-lang="en"')
          .replaceAll('data-lang="pt-BR"', 'data-lang="en"')
          .replaceAll('data-document-title="plain"', 'data-document-title="English page"')
          .replaceAll('data-title="plain"', 'data-title="English page"')
          .replaceAll('data-document-canonical="/plain"', 'data-document-canonical="/english"')
          .replaceAll('data-canonical="/plain"', 'data-canonical="/english"')
          .replaceAll('data-document-description="plain description"', 'data-document-description="English description"')
          .replaceAll('data-description="plain description"', 'data-description="English description"')
          .replace('<h1>plain</h1>', '<h1>English page</h1>');
      }
      if (kind === 'empty-meta') {
        html = html.replaceAll('data-document-canonical="/empty-meta"', 'data-document-canonical=""')
          .replaceAll('data-canonical="/empty-meta"', 'data-canonical=""')
          .replaceAll('data-document-description="empty-meta description"', 'data-document-description=""')
          .replaceAll('data-description="empty-meta description"', 'data-description=""');
      }
      res.end(html);
    } catch (error) {
      res.statusCode = 500;
      res.end(String(error));
    }
  });
  await new Promise(r => server.listen(0, '127.0.0.1', r));
  origin = 'http://127.0.0.1:' + server.address().port;
  browser = await chromium.launch({ headless: true });
});
after(async () => {
  await browser?.close();
  await new Promise(r => server.close(r));
});

async function navigate(page, kind) {
  await page.locator('#go-' + kind).click();
  await page.waitForFunction(k => document.querySelector('h1')?.textContent === k, kind);
}
async function assertMaps(page, kind) {
  const count = kind === 'proposals' ? 2 : 1;
  await page.waitForFunction(n => document.querySelectorAll('canvas').length === n, count);
  assert.equal(await page.evaluate(() => window.created - window.destroyed), count);
}

test('mobile menu stays closed across navigation, clicks and back/forward', async () => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
  const errors = [];
  page.on('pageerror', e => errors.push(e.message));
  await page.goto(origin + '/plain/google');
  for (let i = 0; i < 3; i++) {
    await navigate(page, 'details');
    assert.equal(await page.locator('#mobile-menu').isVisible(), false);
    await navigate(page, 'plain');
  }
  await page.locator('[aria-controls="mobile-menu"]').click();
  await page.locator('#mobile-menu').waitFor({ state: 'visible' });
  await page.locator('#menu-link').click();
  await page.waitForFunction(() => document.querySelector('h1').textContent === 'details');
  assert.equal(await page.locator('#mobile-menu').isVisible(), false);
  await page.goBack();
  await page.waitForFunction(() => document.querySelector('h1').textContent === 'plain');
  assert.equal(await page.locator('#mobile-menu').isVisible(), false);
  await page.goForward();
  await page.waitForFunction(() => document.querySelector('h1').textContent === 'details');
  assert.equal(await page.locator('#mobile-menu').isVisible(), false);
  await page.locator('#account-toggle').click();
  await page.locator('#account-menu').waitFor({ state: 'visible' });
  await navigate(page, 'plain');
  assert.equal(await page.locator('#account-menu').isVisible(), false);
  assert.deepEqual(errors, []);
  await page.close();
});

for (const provider of ['google', 'mapbox', 'maplibre']) {
  test(provider + ': all map consumers load after non-map navigation with delayed dependencies', async () => {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', e => errors.push(e.message));
    await page.goto(origin + '/plain/' + provider);
    assert.equal(await page.evaluate(() => !!window.BikesNestMapProvider), false);
    for (const kind of Object.keys(consumers)) {
      await navigate(page, kind);
      await assertMaps(page, kind);
      assert.equal(await page.locator('head link[href="/static/css/map.css"]').count(), 1);
      if (provider !== 'google') {
        assert.equal(await page.locator('head link[href="/static/vendor/' + provider + '-gl.css"]').count(), 1);
      }
      if (kind === 'new' || kind === 'edit') {
        await page.evaluate(() => document.querySelector('#pin-map')._bnMap.click({lat: 12, lon: 34}));
        assert.equal(await page.locator('#lat').inputValue(), '12.000000');
      }
      await navigate(page, 'plain');
      await page.waitForFunction(() => window.created === window.destroyed);
      await page.goBack();
      await page.waitForFunction(k => document.querySelector('h1').textContent === k, kind);
      await assertMaps(page, kind);
      await navigate(page, 'plain');
    }
    assert.equal(await page.evaluate(() => window.providerLoads), 1);
    assert.deepEqual(errors, []);
    await page.close();
  });
}

test('failed provider download can retry on a later navigation', async () => {
  const page = await browser.newPage();
  let fail = true;
  await page.route('**/map-provider-google.js', route => fail ? route.abort() : route.continue());
  await page.goto(origin + '/details/google');
  await page.waitForEvent('requestfailed', { predicate: r => r.url().endsWith('map-provider-google.js') });
  fail = false;
  await navigate(page, 'plain');
  await navigate(page, 'details');
  await assertMaps(page, 'details');
  await page.close();
});

test('stalled map asset reaches finite failure and retries on the same page', async () => {
  const page = await browser.newPage();
  let release;
  const stall = route => new Promise(resolve => {
    release = async () => { try { await route.continue(); } catch (_) { /* timed-out script removal aborted it */ } resolve(); };
  });
  await page.route('**/map-provider-google.js', stall);
  await page.goto(origin + '/details/google', { waitUntil: 'domcontentloaded' });
  await page.getByRole('button', { name: 'Retry map' }).waitFor({ timeout: 12000 });
  await page.unroute('**/map-provider-google.js', stall);
  await release();
  await page.getByRole('button', { name: 'Retry map' }).evaluate(button => button.click());
  await assertMaps(page, 'details');
  await page.close();
});

test('failed SDK and renderer errors retry in place without duplicate maps', async () => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
  let fail = true;
  await page.route('**/map-provider-google.js', route => fail ? route.abort() : route.continue());
  await page.goto(origin + '/details/google');
  await page.getByRole('button', { name: 'Retry map' }).waitFor();
  fail = false;
  await page.getByRole('button', { name: 'Retry map' }).evaluate(button => button.click());
  await assertMaps(page, 'details');
  assert.equal(await page.locator('canvas').count(), 1);
  await page.evaluate(() => document.querySelector('#map-single')._bnMap.error());
  await page.getByRole('button', { name: 'Retry map' }).waitFor();
  await page.getByRole('button', { name: 'Retry map' }).evaluate(button => button.click());
  await page.waitForFunction(() => window.created === 2 && window.destroyed === 1);
  await page.locator('[data-map-status]').waitFor({ state: 'hidden' });
  assert.equal(await page.locator('canvas').count(), 1);
  await page.close();
});

test('map retry preserves same-page form state and pin coordinates', async () => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
  await page.goto(origin + '/new/google');
  await assertMaps(page, 'new');
  await page.locator('#lat').fill('12.345678');
  await page.locator('#lon').fill('23.456789');
  await page.evaluate(() => {
    window.oldPinMap = document.querySelector('#pin-map')._bnMap;
    window.oldPinMap.error();
  });
  await page.getByRole('button', { name: 'Retry map' }).evaluate(button => button.click());
  await page.waitForFunction(() => window.created === 2 && window.destroyed === 1);
  await page.locator('[data-map-status]').waitFor({ state: 'hidden' });
  assert.equal(await page.locator('#lat').inputValue(), '12.345678');
  assert.equal(await page.locator('#lon').inputValue(), '23.456789');
  assert.equal(await page.locator('canvas').count(), 1);
  await page.evaluate(() => window.oldPinMap.error());
  assert.equal(await page.getByRole('button', { name: 'Retry map' }).count(), 0);
  await page.evaluate(() => document.querySelector('#pin-map')._bnMap.error());
  await page.getByRole('button', { name: 'Retry map' }).evaluate(button => button.click());
  await page.waitForFunction(() => window.created === 3 && window.destroyed === 2);
  assert.equal(await page.locator('canvas').count(), 1);
  await page.evaluate(() => {
    window.oldPinMap.click({ lat: 99, lon: 88 });
    document.querySelector('#pin-map').dispatchEvent(new CustomEvent('bikesnest:pin-set', { detail: { lat: 33, lon: 44 } }));
  });
  assert.equal(await page.locator('#lat').inputValue(), '12.345678');
  assert.deepEqual(await page.evaluate(() => window.jumpCalls), [3]);
  await page.evaluate(() => { document.querySelector('#pin-map').style.width = '321px'; });
  await page.waitForFunction(() => window.resizeCalls.includes(3));
  assert.equal(await page.evaluate(() => window.resizeCalls.at(-1)), 3);
  await page.close();
});

test('search map retries preserve filters without stacking recenter listeners', async () => {
  const page = await browser.newPage();
  await page.goto(origin + '/search/google');
  await assertMaps(page, 'search');
  await page.evaluate(() => {
    const filter = document.createElement('input'); filter.id = 'kept-filter'; filter.value = 'covered';
    document.querySelector('main').appendChild(filter);
    const recenter = document.createElement('button'); recenter.id = 'recenter'; recenter.textContent = 'recenter';
    document.querySelector('#map').before(recenter);
    // Re-run readiness once so the persistent button gets its single listener.
    document.dispatchEvent(new CustomEvent('bikesnest:maps-ready'));
  });
  for (let attempt = 0; attempt < 2; attempt++) {
    await page.evaluate(() => document.querySelector('#map')._bnMap.error());
    await page.getByRole('button', { name: 'Retry map' }).evaluate(button => button.click());
    await page.waitForFunction(expected => window.created === expected, attempt + 2);
    await page.locator('[data-map-status]').waitFor({ state: 'hidden' });
  }
  await page.locator('#recenter').click();
  assert.equal(await page.evaluate(() => window.flyCalls), 1);
  assert.equal(await page.locator('#kept-filter').inputValue(), 'covered');
  assert.equal(await page.locator('canvas').count(), 1);
  await page.close();
});

test('boosted and history page swaps synchronize document metadata and heading focus', async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 844 } });
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(origin + '/plain/google');
  await page.evaluate(() => {
    const link = document.createElement('a');
    link.href = '/english/google'; link.id = 'english-link'; link.textContent = 'English';
    document.body.appendChild(link);
    htmx.process(link);
  });
  await page.locator('#english-link').click();
  await page.waitForFunction(() => document.title === 'English page', null, { timeout: 2000 });
  await page.waitForTimeout(100);
  assert.deepEqual(errors, []);
  assert.equal(await page.evaluate(() => document.documentElement.lang), 'en');
  assert.equal(await page.evaluate(() => document.body.dataset.documentCanonical), '/english');
  assert.equal(await page.evaluate(() => document.querySelector("link[rel='canonical']")?.getAttribute('href')), '/english');
  assert.equal(await page.evaluate(() => document.activeElement.textContent), 'English page');
  assert.match(await page.locator('#page-change-announcement').textContent(), /English page/);
  await page.goBack();
  await page.waitForFunction(() => document.querySelector('h1')?.textContent === 'plain', null, { timeout: 2000 });
  await page.waitForFunction(() => document.title === 'plain' && document.documentElement.lang === 'pt-BR');
  assert.equal(await page.evaluate(() => document.body.dataset.documentCanonical), '/plain');
  assert.equal(await page.evaluate(() => document.querySelector("link[rel='canonical']")?.getAttribute('href')), '/plain');
  assert.equal(await page.evaluate(() => document.activeElement.textContent), 'plain');
  await page.evaluate(() => {
    const link = document.createElement('a');
    link.href = '/empty-meta/google'; link.id = 'empty-meta-link'; link.textContent = 'Empty metadata';
    document.body.appendChild(link); htmx.process(link);
  });
  await page.locator('#empty-meta-link').click();
  await page.waitForFunction(() => document.querySelector('h1')?.textContent === 'empty-meta');
  assert.equal(await page.locator("link[rel='canonical']").count(), 0);
  assert.equal(await page.locator("meta[name='description']").count(), 0);
  await page.close();
});

test('rejected full-body form focus survives boosted scroll and belongs to its request', async () => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
  await page.goto(origin + '/form-start/google');
  await page.getByRole('button', { name: 'Submit invalid form' }).click();
  await page.waitForFunction(() => document.activeElement?.id === 'invalid-price');
  const box = await page.locator('#invalid-price').boundingBox();
  assert.ok(box.y >= 0 && box.y + box.height <= 844, 'marked correction must remain visible after boosted show=top');
  assert.equal(await page.locator('#error-group').getAttribute('open'), '');

  await page.route('**/unrelated-focus-fragment', route => route.fulfill({
    contentType: 'text/html', body: '<p id="fragment-result">fragment settled</p>',
  }));
  await page.evaluate(() => htmx.ajax('GET', '/unrelated-focus-fragment', {
    target: '#unrelated-fragment', swap: 'innerHTML',
  }));
  await page.locator('#fragment-result').waitFor();
  assert.equal(await page.evaluate(() => document.activeElement?.id), 'invalid-price',
    'an unrelated fragment must not claim full-body error focus');

  await page.locator('#go-plain').click();
  await page.waitForFunction(() => document.querySelector('h1')?.textContent === 'plain');
  assert.equal(await page.evaluate(() => document.activeElement?.textContent), 'plain',
    'a later error-free navigation owns normal heading focus');
  await page.close();
});

for (const kind of ['gps-home', 'gps-search', 'gps-pin']) {
  test(kind + ': location outcomes are announced and detached callbacks are ignored', async () => {
    const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
    await page.addInitScript(() => {
      window.locationCallbacks = [];
      Object.defineProperty(navigator, 'geolocation', { configurable: true, value: {
        getCurrentPosition: (success, failure, options) => window.locationCallbacks.push({ success, failure, options }),
      }});
    });
    await page.goto(origin + '/' + kind + '/google');
    for (const outcome of [{ code: 1, text: 'denied' }, { code: 2, text: 'unavailable' }, { code: 3, text: 'timeout' }]) {
      await page.locator('#locate').click();
      assert.equal(await page.locator('#gps p').textContent(), 'loading');
      assert.equal(await page.evaluate(() => window.locationCallbacks.at(-1).options.timeout), 10000);
      await page.evaluate(code => window.locationCallbacks.at(-1).failure({ code }), outcome.code);
      await page.waitForFunction(text => document.querySelector('#gps p').textContent === text, outcome.text);
    }
    if (kind === 'gps-search') {
      await page.locator('#fallback').click();
      assert.equal(await page.evaluate(() => document.activeElement.id), 'q');
    } else {
      assert.equal(await page.locator(kind === 'gps-pin' ? '#address' : '#q').count(), 1);
    }
    await page.locator('#locate').click();
    await navigate(page, 'plain');
    await page.evaluate(() => window.locationCallbacks.at(-1).success({ coords: { latitude: 1, longitude: 2 } }));
    await page.waitForFunction(() => document.querySelector('h1').textContent === 'plain');
    assert.match(page.url(), /\/plain\/google$/);
    await page.close();
  });
}

test('pinned htmx network-restores without a localStorage history snapshot', async () => {
  const page = await browser.newPage();
  await page.goto(origin + '/plain/google');
  await navigate(page, 'details');
  assert.equal(await page.evaluate(() => localStorage.getItem('htmx-history-cache')), null);
  const [restoreRequest] = await Promise.all([
    page.waitForRequest(request => request.url() === origin + '/plain/google'),
    page.goBack(),
  ]);
  assert.equal(restoreRequest.url(), origin + '/plain/google');
  await page.waitForURL('**/plain/google');
  await page.waitForFunction(() => document.querySelector('h1').textContent === 'plain');
  assert.equal(await page.evaluate(() => localStorage.getItem('htmx-history-cache')), null);
  await page.close();
});

test('enforced candidate runs trusted scripts but rejects swapped script gadgets', async () => {
  const page = await browser.newPage();
  let externalRequested = false;
  await page.route('https://evil.invalid/**', route => {
    externalRequested = true;
    return route.abort();
  });
  await page.goto(origin + '/plain/google');
  assert.equal(await page.evaluate(() => typeof htmx), 'object');
  await page.evaluate(() => htmx.ajax('GET', '/attack/google', { target: '#content' }));
  await page.waitForFunction(() => document.querySelector('h1')?.textContent === 'attack');
  assert.equal(await page.evaluate(() => window.injectedScriptRan), undefined);
  assert.equal(await page.evaluate(() => window.injectedHandlerRan), undefined);
  assert.equal(externalRequested, false);
  await page.close();
});

for (const provider of ['maplibre', 'mapbox']) {
  test(provider + ': actual vendored SDK constructs and loads a local empty map under CSP', async () => {
    const page = await browser.newPage();
    const violations = [];
    const workerRequests = [];
    const mapboxTokenRequests = [];
    if (provider === 'mapbox') {
      // Fulfil SDK telemetry/session requests locally: prove the configured
      // browser token reaches Mapbox's request boundary without contacting it.
      await page.route(/https:\/\/(?:api|events)\.mapbox\.com\/.*/, route => {
        mapboxTokenRequests.push(route.request().url());
        return route.fulfill({ status: 204, body: '' });
      });
    }
    page.on('console', message => {
      if (message.text().includes('Content Security Policy')) violations.push(message.text());
    });
    page.on('request', request => {
      if (request.url().includes('maplibre-gl-worker.mjs')) workerRequests.push(request.url());
    });
    await page.goto(origin + '/sdk/' + provider);
    await page.waitForFunction(() => window.sdkReady === true);
    assert.equal(await page.evaluate(() => !!window.actualMap.getCanvas()), true);
    if (provider === 'maplibre') assert.equal(workerRequests.length > 0, true);
    if (provider === 'mapbox') {
      assert.equal(await page.evaluate(() => window.sdkToken), 'browser-mapbox-test-token');
      await page.waitForFunction(() => performance.getEntriesByType('resource').some(entry =>
        entry.name.includes('mapbox.com/')));
      assert.equal(mapboxTokenRequests.length > 0, true);
      assert.equal(mapboxTokenRequests.every(url => new URL(url).searchParams.get('access_token') === 'browser-mapbox-test-token'), true);
    }
    assert.deepEqual(violations, []);
    await page.close();
  });
  test(provider + ': actual vendored SDK surfaces a failed local tile request', async () => {
    const page = await browser.newPage();
    if (provider === 'mapbox') {
      await page.route(/https:\/\/(?:api|events)\.mapbox\.com\/.*/, route =>
        route.fulfill({ status: 204, body: '' }));
    }
    await page.goto(origin + '/sdk-fail/' + provider);
    await page.waitForFunction(() => window.sdkFailed === true);
    assert.equal(await page.evaluate(() => !!window.actualMap.getCanvas()), true);
    await page.close();
  });
}

test('fragment morphs retain Alpine visibility and remain interactive', async () => {
  const page = await browser.newPage();
  await page.goto(origin + '/plain/google');
  await page.route('**/fragment', route => route.fulfill({
    contentType: 'text/html',
    body: '<div id="account-menu" x-show="open" x-cloak>updated items</div>',
  }));
  const update = () => page.evaluate(() => htmx.ajax('GET', '/fragment', {
    target: '#account-menu', swap: 'outerMorph',
  }));
  await update();
  assert.equal(await page.locator('#account-menu').isVisible(), false);
  await page.locator('#account-toggle').click();
  await page.locator('#account-menu').waitFor({state: 'visible'});
  await update();
  assert.equal(await page.locator('#account-menu').isVisible(), true);
  await page.locator('#account-toggle').click();
  await page.locator('#account-menu').waitFor({state: 'hidden'});
  await page.close();
});

test('search results swaps keep the live map and refresh its data', async () => {
  const page = await browser.newPage();
  await page.goto(origin + '/search/google');
  await assertMaps(page, 'search');
  await page.evaluate(() => {
    const results = document.createElement('div');
    results.id = 'results';
    document.querySelector('main').appendChild(results);
    window.originalMap = document.querySelector('#map')._bnMap;
  });
  await page.route('**/results', route => route.fulfill({
    contentType: 'text/html',
    body: '<script id="search-data" type="application/json" hx-swap-oob="true">{"items":[],"total":7}</script><p id="map-count"></p>',
  }));
  await page.evaluate(() => htmx.ajax('GET', '/results', {target: '#results', swap: 'innerHTML'}));
  await page.waitForFunction(() => document.querySelector('#map-count').textContent.includes('7'));
  assert.equal(await page.evaluate(() => document.querySelector('#map')._bnMap === window.originalMap), true);
  assert.equal(await page.evaluate(() => window.created), 1);
  await page.close();
});

test('a hidden search map initializes on reveal without another navigation', async () => {
  const page = await browser.newPage({ viewport: { width: 390, height: 844 } });
  await page.route('**/search/google', async route => {
    const html = await pageHtml('search', 'google');
    await route.fulfill({ contentType: 'text/html', body: html.replace('height:300px', 'height:300px;display:none') });
  });
  await page.goto(origin + '/search/google');
  await page.waitForFunction(() => window.__bnSearchBound);
  assert.equal(await page.locator('canvas').count(), 0);
  await page.evaluate(() => { document.querySelector('#map').style.display = ''; });
  await assertMaps(page, 'search');
  await navigate(page, 'plain');
  await page.waitForFunction(() => window.created === window.destroyed);
  await page.close();
});

test('leaving before dependencies finish never initializes a detached map', async () => {
  const page = await browser.newPage();
  await page.goto(origin + '/plain/maplibre');
  await navigate(page, 'details');
  await navigate(page, 'plain');
  await page.waitForFunction(() => window.BikesNestMapProvider);
  assert.equal(await page.evaluate(() => window.created), 0);
  await navigate(page, 'edit');
  await assertMaps(page, 'edit');
  await page.close();
});
