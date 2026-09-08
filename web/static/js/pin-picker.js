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
 * CSP-safe: no inline script or dynamic markup. */
(function () {
  "use strict";

  function num(value) {
    var n = parseFloat(value);
    return isFinite(n) ? n : null;
  }

  function initReady() {
    var el = document.getElementById("pin-map");
    var provider = window.BikesNestMapProvider;
    if (!el || !provider || el._bnMap) return;

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

    var map = provider.createMap(el, {
      center: { lon: centerLon, lat: centerLat },
      zoom: picked ? 17 : 14,
      navigation: true,
    });
    window.BikesNestMaps.track(el, map);

    var markerEl = document.createElement("div");
    markerEl.className = "marker marker-pin";
    var marker = map.addMarker({
      element: markerEl,
      position: { lon: centerLon, lat: centerLat },
      draggable: true,
      onDragEnd: function (at) { publish(at.lat, at.lon); },
    });

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

    map.onClick(function (at) {
      marker.setPosition(at);
      publish(at.lat, at.lon);
    });

    el.addEventListener("bikesnest:pin-set", function (e) {
      var detail = (e && e.detail) || {};
      var toLat = num(detail.lat);
      var toLon = num(detail.lon);
      if (toLat === null || toLon === null) return;
      marker.setPosition({ lon: toLon, lat: toLat });
      map.jumpTo({ center: { lon: toLon, lat: toLat }, zoom: 17 });
    });
  }

  function init() {
    var provider = window.BikesNestMapProvider;
    if (!provider) return;
    provider.ready().then(initReady).catch(function () {
      /* Manual latitude/longitude fields remain available. */
    });
  }

  document.addEventListener("bikesnest:maps-ready", init);
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
