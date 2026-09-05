/* Single-location maps rendered from `data-*` attributes.
 *
 * Two shapes share this file:
 *   - `#map-single` on details pages: one marker at data-lat/data-lon.
 *   - `.proposal-map` in the proposal queue: the same, plus an optional "before"
 *     marker at data-current-lat/data-current-lon so a move proposal shows
 *     where the pin is now and where it would go. When both are present the
 *     view is fitted to contain them.
 *
 * Everything comes from attributes because CSP forbids inline script.
 */
(function () {
  "use strict";
  function addMarker(map, lon, lat, className, label) {
    var el = document.createElement("div");
    el.className = className;
    if (label) el.title = label;
    return map.addMarker({ element: el, position: { lon: lon, lat: lat } });
  }

  function num(value) {
    var n = parseFloat(value);
    return isFinite(n) ? n : null;
  }

  function initOne(el) {
    var provider = window.BikesNestMapProvider;
    if (!el || !provider || el.dataset.initialized) return;
    var lat = num(el.dataset.lat);
    var lon = num(el.dataset.lon);
    if (lat === null || lon === null) return;
    el.dataset.initialized = "1";

    // The optional "before" point. Absent for the details page, and absent for
    // a location that had no coordinates before the proposal.
    var fromLat = num(el.dataset.currentLat);
    var fromLon = num(el.dataset.currentLon);
    var hasFrom = fromLat !== null && fromLon !== null;

    var map = provider.createMap(el, {
      center: { lon: lon, lat: lat },
      zoom: 17,
      navigation: true,
    });
    map.onLoad(function () {
      if (hasFrom) {
        addMarker(map, fromLon, fromLat, "marker marker-before", el.dataset.currentLabel);
      }
      addMarker(map, lon, lat, "marker", el.dataset.proposedLabel || el.dataset.name);
      if (hasFrom && (fromLat !== lat || fromLon !== lon)) {
        map.fitBounds(
          [
            Math.min(fromLon, lon),
            Math.min(fromLat, lat),
            Math.max(fromLon, lon),
            Math.max(fromLat, lat),
          ],
          { padding: 48, maxZoom: 17, duration: 0 }
        );
      }
    });
  }

  function init() {
    var provider = window.BikesNestMapProvider;
    if (!provider) return;
    provider.ready().then(function () {
      initOne(document.getElementById("map-single"));
      var pairs = document.querySelectorAll(".proposal-map");
      for (var i = 0; i < pairs.length; i++) initOne(pairs[i]);
    }).catch(function () { /* The rest of the page remains usable. */ });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
