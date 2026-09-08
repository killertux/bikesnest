const { test } = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { resolve } = require('node:path');
const { runInNewContext } = require('node:vm');

function load(name, globals = {}) {
  const scripts = [];
  const window = { ...globals };
  const document = {
    body: { dataset: {} },
    createElement: () => ({ remove() { this.removed = true; } }),
    head: { appendChild(script) { scripts.push(script); } },
  };
  runInNewContext(readFileSync(resolve(__dirname, '../../web/static/js/map-provider-' + name + '.js'), 'utf8'), { window, document });
  return { window, scripts, provider: window.BikesNestMapProvider };
}

test('Google SDK readiness is shared and a failed request can retry', async () => {
  const { window, provider, scripts } = load('google');
  const first = provider.ready();
  assert.equal(provider.ready(), first);
  assert.equal(scripts.length, 1);
  scripts[0].onerror();
  await assert.rejects(first, /unavailable/);
  assert.equal(scripts[0].removed, true);
  const retry = provider.ready();
  assert.notEqual(retry, first);
  assert.equal(scripts.length, 2);
  window.google = { maps: { Map() {} } };
  window.__bikesnestGoogleMapsReady();
  await retry;
  await provider.ready();
  assert.equal(scripts.length, 2);
});

test('Google map disposal releases live markers, popups and SDK listeners', () => {
  const cleared = [];
  let closed = 0;
  const google = {
    maps: {
      Map: class {},
      marker: { AdvancedMarkerElement: class { constructor(options) { Object.assign(this, options); } } },
      InfoWindow: class { close() { closed++; } },
      event: { clearInstanceListeners(value) { cleared.push(value); } },
    },
  };
  const { provider } = load('google', { google });
  const map = provider.createMap({}, { center: { lon: 1, lat: 2 }, zoom: 14 });
  const raw = map.raw;
  const options = { element: { title: 'pin' }, position: { lon: 1, lat: 2 }, popup: {} };
  const removed = map.addMarker(options);
  removed.remove();
  assert.equal(map.markers.size, 0);
  map.addMarker(options);
  const [entry] = map.markers;
  map.destroy();
  assert.equal(entry.marker.map, null);
  assert.equal(closed, 2);
  assert.equal(map.markers.size, 0);
  assert.equal(cleared.includes(raw), true);
  assert.equal(map.raw, null);
});

for (const name of ['mapbox', 'maplibre']) {
  test(name + ' map disposal releases the SDK instance', async () => {
    let removed = 0;
    const sdk = {
      Map: class { remove() { removed++; } },
    };
    const { provider } = load(name, { [name + 'gl']: sdk });
    await provider.ready();
    const map = provider.createMap({}, { center: { lon: 1, lat: 2 }, zoom: 14, navigation: false });
    map.destroy();
    assert.equal(removed, 1);
  });
}
