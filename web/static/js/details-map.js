/* Single-location maps rendered from `data-*` attributes.
 *
 * Two shapes share this file:
 *   - `#map-single` (P3 details): one marker at data-lat/data-lon.
 *   - `.proposal-map` (M4 proposal queue): the same, plus an optional "before"
 *     marker at data-current-lat/data-current-lon so a move proposal shows
 *     where the pin is now and where it would go. When both are present the
 *     view is fitted to contain them.
 *
 * Everything comes from attributes because CSP forbids inline script.
 */
(function () {
  "use strict";
  var bodyCfg = document.body ? document.body.dataset : {};
  var STYLE_URL = bodyCfg.mapStyleUrl || "https://tiles.openfreemap.org/styles/liberty";
  var ACCESS_TOKEN = bodyCfg.mapAccessToken || "";

  function addMarker(map, lon, lat, className, label) {
    var el = document.createElement("div");
    el.className = className;
    if (label) el.title = label;
    var marker = new maplibregl.Marker(el).setLngLat([lon, lat]);
    marker.addTo(map);
    return marker;
  }

  function num(value) {
    var n = parseFloat(value);
    return isFinite(n) ? n : null;
  }

  function initOne(el) {
    if (!el || !window.maplibregl || el.dataset.initialized) return;
    var lat = num(el.dataset.lat);
    var lon = num(el.dataset.lon);
    if (lat === null || lon === null) return;
    el.dataset.initialized = "1";

    // The optional "before" point. Absent for the details page, and absent for
    // a location that had no coordinates before the proposal.
    var fromLat = num(el.dataset.currentLat);
    var fromLon = num(el.dataset.currentLon);
    var hasFrom = fromLat !== null && fromLon !== null;

    if (ACCESS_TOKEN) maplibregl.accessToken = ACCESS_TOKEN;
    var map = new maplibregl.Map({
      container: el,
      style: STYLE_URL,
      center: [lon, lat],
      zoom: 17,
    });
    map.addControl(new maplibregl.NavigationControl());
    map.on("load", function () {
      if (hasFrom) {
        addMarker(map, fromLon, fromLat, "marker marker-before", el.dataset.currentLabel);
      }
      addMarker(map, lon, lat, "marker", el.dataset.proposedLabel || el.dataset.name);
      if (hasFrom && (fromLat !== lat || fromLon !== lon)) {
        map.fitBounds(
          [
            [Math.min(fromLon, lon), Math.min(fromLat, lat)],
            [Math.max(fromLon, lon), Math.max(fromLat, lat)],
          ],
          { padding: 48, maxZoom: 17, duration: 0 }
        );
      }
    });
  }

  function init() {
    initOne(document.getElementById("map-single"));
    var pairs = document.querySelectorAll(".proposal-map");
    for (var i = 0; i < pairs.length; i++) initOne(pairs[i]);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
