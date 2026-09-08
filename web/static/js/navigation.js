/* Persistent navigation lifecycle. Loaded once from the document head. */
(function () {
  "use strict";
  if (window.BikesNestMaps) return;
  var assets = new Map();
  var maps = new Map();
  var observers = new Map();
  var pending;

  function load(node) {
    var url = node.src || node.href;
    if (assets.has(url)) return assets.get(url);
    // A hard-loaded map page already has these styles in its head.
    if (node.tagName === "LINK") {
      var existing = Array.from(document.querySelectorAll('head link[rel="stylesheet"]'))
        .find(function (link) { return link.href === url; });
      if (existing && existing.sheet) return Promise.resolve();
      if (existing) existing.remove();
    }
    var promise = new Promise(function (resolve, reject) {
      var el = document.createElement(node.tagName.toLowerCase());
      if (node.tagName === "SCRIPT") { el.src = url; el.async = false; }
      else { el.rel = "stylesheet"; el.href = url; }
      el.onload = resolve;
      el.onerror = function () {
        assets.delete(url);
        el.remove();
        reject(new Error("map_asset_unavailable"));
      };
      document.head.appendChild(el);
    });
    assets.set(url, promise);
    return promise;
  }

  function initMaps() {
    var manifest = document.querySelector("template[data-map-assets]");
    if (!manifest) return;
    // Serialize overlapping swaps; a later page must still get its own consumer.
    pending = (pending || Promise.resolve()).catch(function () {}).then(function () {
      if (!manifest.isConnected) return;
      return Array.from(manifest.content.querySelectorAll('link, script')).reduce(function (chain, node) {
        return chain.then(function () { return load(node); });
      }, Promise.resolve()).then(function () {
        if (manifest.isConnected) document.dispatchEvent(new CustomEvent("bikesnest:maps-ready"));
      });
    }).catch(function () { /* A later navigation or online event retries failed assets. */ });
  }

  function disposeRemovedMaps() {
    observers.forEach(function (observer, el) {
      if (el.isConnected) return;
      observer.disconnect();
      observers.delete(el);
    });
    maps.forEach(function (map, el) {
      if (el.isConnected) return;
      map.destroy();
      maps.delete(el);
    });
  }

  window.BikesNestMaps = {
    observe: function (el, observer) { observers.set(el, observer); },
    track: function (el, map) {
      maps.set(el, map);
      el._bnMap = map;
      if (!observers.has(el) && window.ResizeObserver) {
        var observer = new ResizeObserver(function () {
          if (el.isConnected && el.offsetWidth && el.offsetHeight) map.resize();
        });
        observer.observe(el);
        observers.set(el, observer);
      }
    },
  };
  // Do not morph page-local Alpine state or SDK-owned DOM into a different
  // page. Explicit fragment swaps retain their original behavior.
  document.addEventListener("htmx:before:swap", function (event) {
    event.detail.tasks.forEach(function (task) {
      if (task.target === document.body && task.swapSpec.style === "outerMorph") {
        task.swapSpec.style = "outerSync";
      }
    });
  });
  // During fragment morphs Alpine owns display; htmx must not erase x-show's
  // hidden state when the server sends an element without an inline style.
  htmx.registerExtension("bikesnest-visibility", {
    htmx_before_morph_attr: function (el, detail) {
      if (detail.attrName === "style" && el.hasAttribute("x-show") && el._x_isShown !== undefined) {
        var style = document.createElement("div").style;
        style.cssText = detail.newValue || "";
        if (!el._x_isShown) style.setProperty("display", "none");
        else style.removeProperty("display");
        el.style.cssText = style.cssText;
        return false;
      }
    },
  });
  new MutationObserver(disposeRemovedMaps).observe(document.body, { childList: true, subtree: true });
  document.addEventListener("htmx:after:swap", initMaps);
  window.addEventListener("pageshow", initMaps);
  window.addEventListener("online", initMaps);
  initMaps();
})();
