/* Persistent navigation lifecycle. Loaded once from the document head. */
(function () {
  "use strict";
  if (window.BikesNestMaps) return;
  // This is the nonce of the original trusted document. Never copy a nonce
  // from a swapped map manifest or a request header.
  var documentNonce = document.currentScript && document.currentScript.nonce || "";
  var assets = new Map();
  var maps = new Map();
  var observers = new Map();
  var pending;
  var assetTimeoutMs = 10000;

  function allowedAsset(node, rawUrl) {
    var url;
    try { url = new URL(rawUrl, document.baseURI); } catch (_) { return false; }
    if (url.origin !== location.origin || url.search || url.hash) return false;
    var path = url.pathname.replace(/^\/static\/h\/[0-9a-f]{10}\//, "/static/");
    if (node.tagName === "SCRIPT") {
      return /^\/static\/(vendor\/(maplibre-gl|mapbox-gl)\.js|js\/(map-provider-(maplibre|mapbox|google)|search|details-map|pin-picker)\.js)$/.test(path);
    }
    return /^\/static\/(vendor\/(maplibre-gl|mapbox-gl)\.css|css\/map\.css)$/.test(path);
  }

  function load(node) {
    var url = node.src || node.href;
    if (!allowedAsset(node, url)) return Promise.reject(new Error("map_asset_refused"));
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
      var timer = setTimeout(function () {
        assets.delete(url);
        el.remove();
        reject(new Error("map_asset_timeout"));
      }, assetTimeoutMs);
      if (node.tagName === "SCRIPT") {
        el.src = url;
        el.async = false;
        if (documentNonce) el.nonce = documentNonce;
      }
      else { el.rel = "stylesheet"; el.href = url; }
      el.onload = function () { clearTimeout(timer); resolve(); };
      el.onerror = function () {
        clearTimeout(timer);
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
    if (manifest._bnLoading) return;
    if (manifest._bnLoaded) {
      document.dispatchEvent(new CustomEvent("bikesnest:maps-ready"));
      return;
    }
    manifest._bnLoading = true;
    setMapStatus("loading");
    // Serialize overlapping swaps; a later page must still get its own consumer.
    pending = (pending || Promise.resolve()).catch(function () {}).then(function () {
      if (!manifest.isConnected) return;
      return Array.from(manifest.content.querySelectorAll('link, script')).reduce(function (chain, node) {
        return chain.then(function () { return load(node); });
      }, Promise.resolve()).then(function () {
        manifest._bnLoaded = true;
        if (manifest.isConnected) document.dispatchEvent(new CustomEvent("bikesnest:maps-ready"));
      });
    }).catch(function () {
      if (manifest.isConnected) setMapStatus("failed");
    }).then(function () { manifest._bnLoading = false; });
  }

  function setMapStatus(state, mapEl) {
    var statuses = mapEl && mapEl.parentElement ? mapEl.parentElement.querySelectorAll(":scope > [data-map-status]") : document.querySelectorAll("[data-map-status]");
    statuses.forEach(function (el) {
      var message = state === "failed" ? el.dataset.failed : el.dataset.loading;
      el.textContent = message || "";
      el.hidden = state === "ready";
      if (state === "failed") {
        var retry = document.createElement("button");
        retry.type = "button";
        retry.className = "mt-2 block rounded-lg border border-border px-3 py-2 font-medium text-fg";
        retry.dataset.mapRetry = "";
        retry.textContent = el.dataset.retry || "";
        el.appendChild(retry);
      }
    });
  }

  function syncDocument(data) {
    var body = document.body;
    var lang = data.lang || body.dataset.documentLang;
    if (lang) document.documentElement.lang = lang;
    if (data.title || body.dataset.documentTitle) document.title = data.title || body.dataset.documentTitle;
    body.dataset.documentLang = data.lang || "";
    body.dataset.documentTitle = data.title || "";
    body.dataset.documentCanonical = data.canonical || "";
    body.dataset.documentDescription = data.description || "";
    [
      ["link[rel='canonical']", "href", data.canonical || body.dataset.documentCanonical],
      ["meta[name='description']", "content", data.description || body.dataset.documentDescription],
      ["meta[property='og:title']", "content", data.title || body.dataset.documentTitle],
      ["meta[property='og:description']", "content", data.description || body.dataset.documentDescription],
      ["meta[property='og:url']", "content", data.canonical || body.dataset.documentCanonical],
    ].forEach(function (entry) {
      var node = document.head.querySelector(entry[0]);
      if (!entry[2]) { if (node) node.remove(); return; }
      if (!node) {
        node = document.createElement(entry[0].indexOf("link") === 0 ? "link" : "meta");
        if (entry[0].indexOf("canonical") !== -1) node.setAttribute("rel", "canonical");
        else if (entry[0].indexOf("name=") !== -1) node.name = "description";
        else node.setAttribute("property", entry[0].match(/property='([^']+)'/)[1]);
        document.head.appendChild(node);
      }
      node.setAttribute(entry[1], entry[2]);
    });
  }

  function focusFormError(formError) {
    // Unlike ordinary document navigation, a rejected form needs its actual
    // correction visible — especially when the invalid control is inside a
    // native disclosure below the fold on a phone. Center it before focusing
    // so a sticky header cannot obscure the input.
    formError.scrollIntoView({ block: "center" });
    formError.focus({ preventScroll: true });
  }

  function focusNewPage() {
    var heading = document.querySelector("main h1");
    if (!heading) return;
    heading.setAttribute("tabindex", "-1");
    heading.focus({ preventScroll: true });
    var announcement = document.getElementById("page-change-announcement");
    if (announcement) announcement.textContent = announcement.dataset.pageChanged + ": " + heading.textContent.trim();
  }

  function retryMaps() {
    maps.forEach(function (map, el) {
      if (!el.isConnected) return;
      map.destroy();
      maps.delete(el);
      var observer = observers.get(el);
      if (observer) observer.disconnect();
      observers.delete(el);
      delete el._bnMap;
      if (el._bn) { el._bn.map = null; el._bn.initializing = false; }
      while (el.firstChild) el.removeChild(el.firstChild);
    });
    setMapStatus("loading");
    initMaps();
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
    report: function (state, el) { setMapStatus(state, el); },
  };
  // Do not morph page-local Alpine state or SDK-owned DOM into a different
  // page. Explicit fragment swaps retain their original behavior.
  document.addEventListener("htmx:before:swap", function (event) {
    var ctx = event.detail.ctx;
    var fullDocument = event.detail.tasks.some(function (task) { return task.target === document.body; });
    if (fullDocument && ctx && ctx.text) {
      var response = new DOMParser().parseFromString(ctx.text, "text/html");
      var responseMeta = response.querySelector("[data-document-meta]");
      if (responseMeta) ctx.bikesnestDocumentMeta = {
        lang: responseMeta.dataset.lang,
        title: responseMeta.dataset.title,
        canonical: responseMeta.dataset.canonical,
        description: responseMeta.dataset.description,
      };
    }
    event.detail.tasks.forEach(function (task) {
      // Responses never need executable scripts: persistent document scripts
      // and the explicit map manifest own all application code. Keep inert JSON.
      task.fragment.querySelectorAll('script:not([type="application/json"])')
        .forEach(function (script) { script.remove(); });
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
  document.addEventListener("htmx:after:swap", function (event) {
    initMaps();
    var meta = event.detail.ctx && event.detail.ctx.bikesnestDocumentMeta;
    if (meta) {
      syncDocument(meta);
      var formError = document.querySelector('[data-form-error-focus]');
      if (formError && event.detail.ctx.target === document.body) {
        // Keep the correction on this request context. htmx carries the same
        // ctx through finally:swap, after its boosted `show:top` scroll.
        event.detail.ctx.bikesnestFormErrorFocus = formError;
      } else {
        focusNewPage();
      }
    }
  });
  document.addEventListener("htmx:finally:swap", function (event) {
    var ctx = event.detail.ctx;
    if (!ctx) return;
    var formError = ctx.bikesnestFormErrorFocus;
    delete ctx.bikesnestFormErrorFocus;
    if (
      ctx.target === document.body &&
      formError &&
      formError.isConnected &&
      document.querySelector('[data-form-error-focus]') === formError
    ) {
      focusFormError(formError);
    }
  });
  document.addEventListener("click", function (event) {
    if (event.target.closest("[data-map-retry]")) retryMaps();
  });
  window.addEventListener("pageshow", initMaps);
  window.addEventListener("online", initMaps);
  initMaps();
})();
