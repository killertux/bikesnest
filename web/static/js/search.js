/* Search map rendering + card↔marker sync over the server-rendered
 * results. The server stays the source of truth: results arrive as HTML; this
 * file only mirrors them into markers.
 *
 * Written to be idempotent because it runs again after every swap: whole-body
 * swaps (base.html sets `hx-boost:inherited` on <body>, so links and forms are
 * boosted) and the `/search` results-fragment swap. The map is created once and
 * reused; markers are re-rendered from the current #search-data on every
 * (re)load.
 *
 * The map is built *lazily*, on the first moment its container has a size. A
 * MapLibre map constructed inside a `display:none` panel measures 0×0 and
 * renders blank or garbled once revealed, which is exactly what the mobile
 * "show map" toggle used to do. Two independent triggers cover the reveal, so
 * neither has to be reliable on its own:
 *
 *   #map --bikesnest:map-toggle-- (in)  the panel opened/closed (from app.js)
 *   #map --ResizeObserver------- (in)  the panel got a size, however that came
 *   #map --bikesnest:map-moved--- (out) the viewer panned/zoomed; here is the box
 *
 * Everything the map draws comes from the server's #search-data island and is
 * written with DOM APIs (`textContent`, `createElement`) — never `innerHTML` —
 * so a location's name cannot smuggle markup into a marker or a popup.
 * CSP-safe: no inline script, no eval; the style URL/token arrive on <body>
 * data attributes exactly as details-map.js and pin-picker.js read them. */
(function () {
  "use strict";

  var CENTER_FALLBACK = { lon: -49.2733, lat: -25.4284 };

  // Per-page-view state, keyed off the #map element so a fresh map (after a
  // boosted navigation swaps in a new #map) starts clean.
  function state(mapEl) {
    if (!mapEl._bn) mapEl._bn = {
      map: null,
      markers: {},
      clusters: [],
      dest: null,
      ignoreMove: 0,
      initializing: false,
    };
    return mapEl._bn;
  }

  function readData() {
    var dataEl = document.getElementById("search-data");
    if (!dataEl) return null;
    try {
      return JSON.parse(dataEl.textContent);
    } catch (e) {
      return null;
    }
  }

  /* Server-rendered translations, read from #map's data-* attributes (set by
   * search.html via i18n) so the map has no hard-coded English. */
  function readLabels(mapEl) {
    var ds = (mapEl && mapEl.dataset) || {};
    return {
      destination: ds.labelDestination || "Destination",
      onMap: ds.labelOnMap || "{n} on map",
      spot: ds.labelSpot || "Spot {n}",
      details: ds.labelDetails || "View details",
      cluster: ds.labelCluster || "{n} spots here",
    };
  }

  /* One frame later. Called as `window.requestAnimationFrame(fn)` rather than
   * through a detached reference: the browser's rAF throws when it is invoked
   * with the wrong `this`. */
  function nextFrame(fn) {
    if (window.requestAnimationFrame) window.requestAnimationFrame(fn);
    else window.setTimeout(fn, 16);
  }

  // An element inside a `display:none` ancestor has no boxes at all — which is
  // the state a map must not be constructed in.
  function isVisible(el) {
    return !!(el && (el.offsetWidth || el.offsetHeight || el.getClientRects().length));
  }

  function select(id) {
    document.querySelectorAll("[data-parking-id]").forEach(function (card) {
      card.classList.toggle("selected", Number(card.dataset.parkingId) === id);
    });
    var mapEl = document.getElementById("map");
    if (mapEl && mapEl._bn) {
      Object.keys(mapEl._bn.markers).forEach(function (mid) {
        mapEl._bn.markers[mid].getElement().classList.toggle("selected", Number(mid) === id);
      });
    }
    var card = document.querySelector('[data-parking-id="' + id + '"]');
    if (card) card.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }

  /* A result marker: the number the card shows, as a real control — reachable
   * by Tab, announced as a button, and answering Enter/Space like one. */
  function resultMarkerEl(item, labels) {
    var el = document.createElement("div");
    el.className = "marker marker-numbered";
    el.setAttribute("role", "button");
    el.setAttribute("tabindex", "0");
    el.setAttribute("aria-label", labels.spot.replace("{n}", item.n) + ": " + item.name);
    el.textContent = String(item.n);
    return el;
  }

  /* The popup's body, built node by node: name, distance, cost, and the link
   * onward to the details page. Never `innerHTML` — every value here is
   * user-supplied content the server only JSON-escaped. */
  function popupContent(item, labels) {
    var wrap = document.createElement("div");
    wrap.className = "map-popup";
    var name = document.createElement("p");
    name.className = "map-popup-name";
    name.textContent = item.name;
    wrap.appendChild(name);
    var meta = [item.distance_label, item.cost_label].filter(Boolean).join(" · ");
    if (meta) {
      var metaEl = document.createElement("p");
      metaEl.className = "map-popup-meta";
      metaEl.textContent = meta;
      wrap.appendChild(metaEl);
    }
    if (item.href) {
      var link = document.createElement("a");
      link.className = "map-popup-link";
      link.href = item.href;
      link.textContent = labels.details;
      wrap.appendChild(link);
    }
    return wrap;
  }

  /* A cluster marker: how many locations fell in this grid cell. Clicking it
   * zooms towards the cell, which ends in a `moveend` — so the "search this
   * area" button offers the smaller box the viewer just asked for. */
  function clusterMarkerEl(cluster, labels) {
    var el = document.createElement("div");
    el.className = "marker marker-cluster";
    el.setAttribute("role", "button");
    el.setAttribute("tabindex", "0");
    el.setAttribute("aria-label", labels.cluster.replace("{n}", cluster.count));
    el.textContent = String(cluster.count);
    return el;
  }

  function clearMarkers(st) {
    Object.keys(st.markers).forEach(function (id) {
      st.markers[id].remove();
    });
    st.markers = {};
    st.clusters.forEach(function (m) {
      m.remove();
    });
    st.clusters = [];
    if (st.dest) {
      st.dest.remove();
      st.dest = null;
    }
  }

  function renderMarkers(map, st, data, labels) {
    clearMarkers(st);
    if (data.origin && data.origin.lat != null) {
      var destEl = document.createElement("div");
      destEl.className = "marker marker-destination";
      destEl.title = data.origin.label || labels.destination;
      st.dest = map.addMarker({
        element: destEl,
        position: { lon: data.origin.lon, lat: data.origin.lat },
        anchor: "bottom",
      });
    }
    (data.items || []).forEach(function (item) {
      var el = resultMarkerEl(item, labels);
      var marker = map.addMarker({
        element: el,
        position: { lon: item.lon, lat: item.lat },
        // The adapters pass this DOM node to setDOMContent, preserving the
        // text-only construction above across every map provider.
        popup: popupContent(item, labels),
      });
      el.addEventListener("click", function () {
        select(item.id);
      });
      el.addEventListener("keydown", function (e) {
        if (e.key !== "Enter" && e.key !== " " && e.key !== "Spacebar") return;
        e.preventDefault();
        marker.togglePopup();
        select(item.id);
      });
      st.markers[item.id] = marker;
    });
    (data.clusters || []).forEach(function (cluster) {
      var el = clusterMarkerEl(cluster, labels);
      var marker = map.addMarker({
        element: el,
        position: { lon: cluster.lon, lat: cluster.lat },
      });
      function zoomIn() {
        map.easeTo({
          center: { lon: cluster.lon, lat: cluster.lat },
          zoom: map.getZoom() + 2,
        });
      }
      el.addEventListener("click", zoomIn);
      el.addEventListener("keydown", function (e) {
        if (e.key !== "Enter" && e.key !== " " && e.key !== "Spacebar") return;
        e.preventDefault();
        zoomIn();
      });
      st.clusters.push(marker);
    });
  }

  /* Tell the page which box the map is looking at. app.js turns this into the
   * "search this area" button and the browse form's hidden `bbox`. */
  function publishBounds(mapEl, map) {
    var b = map.getBounds();
    if (!b) return;
    var bbox = [
      b.west.toFixed(5),
      b.south.toFixed(5),
      b.east.toFixed(5),
      b.north.toFixed(5),
    ].join(",");
    mapEl.dispatchEvent(
      new CustomEvent("bikesnest:map-moved", { bubbles: true, detail: { bbox: bbox } })
    );
  }

  function initReady() {
    var data = readData();
    if (!data) return;
    var mapEl = document.getElementById("map");
    var provider = window.BikesNestMapProvider;
    if (!mapEl || !provider) return;

    var st = state(mapEl);
    var labels = readLabels(mapEl);

    /* A hidden panel is not a place to build a map: remember nothing, observe
     * the container, and come back when it has a size. */
    observe(mapEl);
    if (!isVisible(mapEl)) return;

    var center = CENTER_FALLBACK;
    var zoom = 13;
    if (data.origin && data.origin.lat != null) {
      center = { lon: data.origin.lon, lat: data.origin.lat };
      zoom = 14;
    } else if (data.items && data.items.length) {
      center = { lon: data.items[0].lon, lat: data.items[0].lat };
    }
    // Browse mode: the box the server answered for is the view, whatever is
    // inside it.
    var bbox = data.bbox && data.bbox.length === 4 ? data.bbox : null;

    if (!st.map) {
      st.map = provider.createMap(mapEl, { center: center, zoom: zoom, navigation: true });
      var recenter = document.getElementById("recenter");
      if (recenter) {
        recenter.addEventListener("click", function () {
          st.ignoreMove++;
          st.map.flyTo({ center: center, zoom: zoom });
        });
      }
      // Only *the viewer's* moves offer a new area to search: the camera moves
      // this file makes itself (framing a fresh result set, recentring) are
      // swallowed, or the button would appear on every page load.
      st.map.onMoveEnd(function () {
        if (st.ignoreMove > 0) {
          st.ignoreMove--;
          return;
        }
        publishBounds(mapEl, st.map);
      });
      st.map.onLoad(function () {
        renderMarkers(st.map, st, readData() || data, labels);
        if (bbox) {
          st.ignoreMove++;
          st.map.fitBounds(bbox, { animate: false, padding: 24 });
        }
      });
    } else {
      st.ignoreMove++;
      if (bbox) {
        st.map.fitBounds(bbox, { animate: false, padding: 24 });
      } else {
        st.map.jumpTo({ center: center, zoom: zoom });
      }
      renderMarkers(st.map, st, data, labels);
      st.map.resize();
    }

    var countEl = document.getElementById("map-count");
    if (countEl) {
      var n = data.total != null ? data.total : (data.items || []).length;
      countEl.textContent = labels.onMap.replace("{n}", n);
    }
  }

  function init() {
    var mapEl = document.getElementById("map");
    var provider = window.BikesNestMapProvider;
    if (!mapEl || !provider) return;
    var st = state(mapEl);
    observe(mapEl);
    if (!isVisible(mapEl)) return;
    if (st.map) {
      initReady();
      return;
    }
    if (st.initializing) return;
    st.initializing = true;
    provider.ready().then(function () {
      st.initializing = false;
      initReady();
    }).catch(function () {
      st.initializing = false;
    });
  }

  /* Belt and braces for the reveal: a container that gains a size either has a
   * map to resize or is ready for one to be built. Covers every way a panel
   * can open — the Alpine toggle, a breakpoint change, a parent's animation —
   * without this file knowing about any of them. */
  function observe(mapEl) {
    if (mapEl._bnObserved || !window.ResizeObserver) return;
    mapEl._bnObserved = true;
    var ro = new ResizeObserver(function () {
      if (!isVisible(mapEl)) return;
      var st = state(mapEl);
      if (st.map) st.map.resize();
      else init();
    });
    ro.observe(mapEl);
  }

  // Bind delegated + HTMX listeners exactly once. A boosted navigation swaps
  // <body> and re-runs this file, so the guard is what stops the handlers from
  // stacking up. These sit on `document`, which no swap replaces, and they
  // re-resolve #map on every call — so none of them can hold a stale map.
  if (!window.__bnSearchBound) {
    window.__bnSearchBound = true;
    document.addEventListener("click", function (e) {
      var card = e.target.closest("[data-parking-id]");
      if (card) select(Number(card.dataset.parkingId));
    });
    // Re-render markers after an HTMX results-fragment swap (htmx 4 event).
    document.addEventListener("htmx:after:swap", function () {
      setTimeout(init, 0);
    });
    /* The panel was shown or hidden. `x-show` flips `display` as part of the
     * same task, so the size is only real on the next frame — hence the
     * requestAnimationFrame before building or resizing. */
    document.addEventListener("bikesnest:map-toggle", function (e) {
      if (!e.detail || !e.detail.open) return;
      nextFrame(function () {
        var mapEl = document.getElementById("map");
        if (!mapEl) return;
        var st = state(mapEl);
        if (st.map) st.map.resize();
        init();
      });
    });
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init);
  } else {
    init();
  }
})();
