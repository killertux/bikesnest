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
  search: ['search', '<div id="map" style="height:300px"></div><script id="search-data" type="application/json">{"items":[],"total":0}</script>'],
  details: ['parking_details', '<div id="map-single" data-lat="-25" data-lon="-49"></div>'],
  new: ['parking_new', '<input id="lat"><input id="lon"><div id="pin-map" data-default-lat="-25" data-default-lon="-49"></div>'],
  edit: ['parking_edit', '<input id="lat" value="-25"><input id="lon" value="-49"><div id="pin-map"></div>'],
  proposals: ['moderation_proposals', '<div class="proposal-map" data-lat="-25" data-lon="-49" data-current-lat="-26" data-current-lon="-50"></div><div class="proposal-map" data-lat="-27" data-lon="-48"></div>'],
};

// Use the real page asset declarations; only server-side business data and
// third-party map services are replaced. No database or external API is used.
function renderAssets(html, provider) {
  return html.replace(/{% if layout.uses_maplibre\(\) %}([\s\S]*?){% endif %}/g, (_, block) => {
    const branches = block.split(/{% else if layout.uses_(?:mapbox|google_maps)\(\) %}/);
    return branches[{ maplibre: 0, mapbox: 1, google: 2 }[provider]] || '';
  }).replace(/{{ layout.asset\("([^"]+)"\) }}/g, '/static/$1');
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
  return '<!DOCTYPE html><html><head><title>' + kind + '</title>' +
    '<link rel="stylesheet" href="/static/css/app.css">' + scripts +
    '</head><body hx-boost:inherited="true" data-map-provider="' + provider + '">' +
    header + button + menu + '<a id="menu-link" href="/details/' + provider + '">details</a></div></header>' +
    '<div x-data="accountMenu"><button id="account-toggle" @click="toggle">account</button><div id="account-menu" x-show="open" x-cloak>account items</div></div>' +
    '<main id="content"><h1>' + kind + '</h1>' + content + '</main>' +
    Object.keys({ plain: 1, ...consumers }).map(k => '<a id="go-' + k + '" href="/' + k + '/' + provider + '">' + k + '</a>').join(' ') +
    assets + '</body></html>';
}

const providerStub = `
if (document.body.dataset.mapProvider !== 'google' && !window.testSdk) throw Error('sdk_not_ready');
window.providerLoads = (window.providerLoads || 0) + 1;
window.created = window.created || 0;
window.destroyed = window.destroyed || 0;
window.BikesNestMapProvider = {
 ready: function () { return Promise.resolve(); },
 createMap: function(el) {
  window.created++;
  el.appendChild(document.createElement('canvas'));
  var map = {
   onLoad: function(fn) { setTimeout(function() { if(el.isConnected) fn(); }, 0); },
   onClick: function(fn) { map.click = fn; }, onMoveEnd: function() {},
   addMarker: function(opts) { return { remove: function(){}, setPosition: function(){}, getElement: function(){ return opts.element; } }; },
   resize: function(){}, fitBounds: function(){}, jumpTo: function(){},
   flyTo: function(){}, easeTo: function(){}, getBounds: function(){ return null; }, getZoom: function(){return 14;},
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
        if (/map-provider-.*\.js$/.test(path)) {
          await new Promise(r => setTimeout(r, 150));
          res.setHeader('Content-Type', 'text/javascript');
          return res.end(providerStub);
        }
        if (/vendor\/map(lib|box).*\.js$/.test(path)) {
          await new Promise(r => setTimeout(r, 250));
          res.setHeader('Content-Type', 'text/javascript');
          return res.end('window.testSdk = true;');
        }
        res.setHeader('Content-Type', path.endsWith('.js') ? 'text/javascript' : 'text/css');
        return res.end(await source('web/' + path));
      }
      const [, kind = 'plain', provider = 'google'] = url.pathname.split('/');
      res.setHeader('Content-Type', 'text/html');
      res.end(await pageHtml(kind, provider));
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
