/* The add/edit map picker: one draggable pin whose position writes the form's
 * real lat/lon inputs.
 *
 * Progressive enhancement, so the contract is one-directional and small: the
 * inputs are the submission (they live in the form's "Advanced" details and
 * post fine with this file absent); this file only offers a nicer way to fill
 * them. Communication with the Alpine controls is element-scoped, on #pin-map:
 *
 *   bikesnest:pin-set (in)  — move the pin here (geolocation / geocode result)
 *   bikesnest:pin     (out) — the pin moved (drag or map click)
 *
 * Scoping the listener to the element rather than window means a boosted
 * navigation that swaps in a fresh #pin-map leaves no stale handler holding a
 * map that is no longer on the page.
 *
 * CSP-safe: no inline script, no eval; the style URL/token arrive on <body>
 * data attributes exactly as search.js and details-map.js read them. */
(function () {
  "use strict";

  var bodyCfg = document.body ? document.body.dataset : {};
  var STYLE_URL = bodyCfg.mapStyleUrl || "https://tiles.openfreemap.org/styles/liberty";
  var ACCESS_TOKEN = bodyCfg.mapAccessToken || "";

  function num(value) {
    var n = parseFloat(value);
    return isFinite(n) ? n : null;
  }

  function init() {
    var el = document.getElementById("pin-map");
    if (!el || !window.maplibregl || el.dataset.initialized) return;
    el.dataset.initialized = "1";

    var latInput = document.getElementById(el.dataset.latInput || "lat");
    var lonInput = document.getElementById(el.dataset.lonInput || "lon");

    /* Centre, in order: whatever the form already holds (a re-render after a
     * validation failure, or the spot's own position when editing), then the
     * server-rendered default (the city centroid). The default only centres
     * the map — it is never written into the inputs, so an untouched picker
     * cannot silently file a spot at the middle of town. */
    var lat = num(latInput && latInput.value);
    var lon = num(lonInput && lonInput.value);
    if (lat === null || lon === null) {
      lat = num(el.dataset.lat);
      lon = num(el.dataset.lon);
    }
    var picked = lat !== null && lon !== null;
    var centerLat = picked ? lat : num(el.dataset.defaultLat);
    var centerLon = picked ? lon : num(el.dataset.defaultLon);
    if (centerLat === null || centerLon === null) {
      centerLat = -25.4284;
      centerLon = -49.2733;
    }

    if (ACCESS_TOKEN) maplibregl.accessToken = ACCESS_TOKEN;
    var map = new maplibregl.Map({
      container: el,
      style: STYLE_URL,
      center: [centerLon, centerLat],
      zoom: picked ? 17 : 14,
    });
    map.addControl(new maplibregl.NavigationControl());

    var markerEl = document.createElement("div");
    markerEl.className = "marker marker-pin";
    var marker = new maplibregl.Marker({ element: markerEl, draggable: true })
      .setLngLat([centerLon, centerLat])
      .addTo(map);

    function publish(lat, lon) {
      if (latInput) latInput.value = lat.toFixed(6);
      if (lonInput) lonInput.value = lon.toFixed(6);
      el.dispatchEvent(
        new CustomEvent("bikesnest:pin", {
          bubbles: true,
          detail: { lat: lat, lon: lon },
        })
      );
    }

    marker.on("dragend", function () {
      var at = marker.getLngLat();
      publish(at.lat, at.lng);
    });
    map.on("click", function (e) {
      marker.setLngLat(e.lngLat);
      publish(e.lngLat.lat, e.lngLat.lng);
    });

    el.addEventListener("bikesnest:pin-set", function (e) {
      var detail = (e && e.detail) || {};
      var toLat = num(detail.lat);
      var toLon = num(detail.lon);
      if (toLat === null || toLon === null) return;
      marker.setLngLat([toLon, toLat]);
      map.jumpTo({ center: [toLon, toLat], zoom: 17 });
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
