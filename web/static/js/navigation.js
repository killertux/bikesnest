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
  var moduleFailed = false;
  var assetTimeoutMs = 10000;

  function allowedAsset(node, rawUrl) {
    var url;
    try { url = new URL(rawUrl, document.baseURI); } catch (_) { return false; }
    if (url.origin !== location.origin || url.search || url.hash) return false;
    var path = url.pathname.replace(/^\/static\/h\/[0-9a-f]{10}\//, "/static/");
    if (node.tagName === "SCRIPT") {
      return /^\/static\/(vendor\/mapbox-gl\.js|js\/maplibre-loader\.mjs|js\/(map-provider-(maplibre|mapbox|google)|search|details-map|pin-picker)\.js)$/.test(path);
    }
    return /^\/static\/(vendor\/(maplibre-gl|mapbox-gl)\.css|css\/map\.css)$/.test(path);
  }

  function load(node) {
    var url = node.src || node.href;
    if (!allowedAsset(node, url)) return Promise.reject(new Error("map_asset_refused"));
    // A hard-loaded map page already has these styles in its head.
    if (!assets.has(url) && node.tagName === "LINK") {
      var existing = Array.from(document.querySelectorAll('head link[rel="stylesheet"]'))
        .find(function (link) { return link.href === url; });
      if (existing && existing.sheet) return Promise.resolve();
      if (existing) existing.remove();
    }
    if (!assets.has(url)) assets.set(url, fetchAsset(node, url));
    // The timeout bounds how long the page shows "loading", not the download:
    // the element stays in flight, a Retry waits on that same element instead
    // of inserting a duplicate, and a late arrival restarts the map itself.
    var download = assets.get(url);
    return new Promise(function (resolve, reject) {
      var timer = setTimeout(function () {
        download.timedOut = true;
        reject(new Error("map_asset_timeout"));
      }, assetTimeoutMs);
      download.then(
        function () { clearTimeout(timer); resolve(); },
        function (error) { clearTimeout(timer); reject(error); }
      );
    });
  }

  function fetchAsset(node, url) {
    var download = new Promise(function (resolve, reject) {
      var el = document.createElement(node.tagName.toLowerCase());
      if (node.tagName === "SCRIPT") {
        el.src = url;
        el.async = false;
        if (node.type) el.type = node.type;
        if (documentNonce) el.nonce = documentNonce;
      }
      else { el.rel = "stylesheet"; el.href = url; }
      el.onload = function () { resolve(); };
      el.onerror = function () {
        assets.delete(url);
        el.remove();
        // The browser's module map remembers a failed module graph for the
        // document's lifetime, so inserting the same module again can never
        // succeed: only a reload recovers it.
        if (node.type === "module") moduleFailed = true;
        reject(new Error("map_asset_unavailable"));
      };
      document.head.appendChild(el);
    });
    download.then(function () {
      // Arrived after the page already reported the failure: carry on.
      if (download.timedOut) initMaps();
    }, function () {});
    return download;
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
    if (moduleFailed) {
      location.reload();
      return;
    }
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

  // A request that never got an answer (offline, dropped connection, timeout)
  // would otherwise fail silently: htmx 4 reports it as `htmx:error` with no
  // `ctx.response`. An answered request — any status — has its own swapped
  // message, and a request aborted by `hx-sync` replacement is not a failure.
  var failedRequest = null;

  function requestNotice() {
    var region = document.getElementById("request-error");
    if (!region) return null;
    return {
      region: region,
      box: region.querySelector("[data-request-error-box]"),
      message: region.querySelector("[data-request-error-message]"),
      retry: region.querySelector("[data-request-retry]"),
      dismiss: region.querySelector("[data-request-dismiss]"),
    };
  }

  function hideRequestError() {
    failedRequest = null;
    var notice = requestNotice();
    if (!notice || !notice.box) return;
    notice.box.hidden = true;
    notice.message.textContent = "";
  }

  function showRequestError(ctx) {
    var notice = requestNotice();
    if (!notice || !notice.box) return;
    var active = document.activeElement;
    var focusLost = !active || active === document.body || !active.isConnected;
    // Only a read is retried: a change may have reached the server even
    // though its answer did not, so resending it is the visitor's call.
    var isRead = /^GET$/i.test(ctx.request.method || "");
    failedRequest = {
      read: isRead,
      url: ctx.request.action,
      source: ctx.sourceElement,
      target: ctx.target,
    };
    notice.retry.hidden = !isRead;
    notice.box.hidden = false;
    // Live regions announce changed text, not a box that becomes visible.
    notice.message.textContent = "";
    setTimeout(function () { notice.message.textContent = notice.region.dataset.message || ""; }, 50);
    if (focusLost) (isRead ? notice.retry : notice.dismiss).focus();
  }

  function requestTimedOut(ctx) {
    var timeout = ctx.request.timeout != null ? htmx.parseInterval(ctx.request.timeout) : htmx.config.defaultTimeout;
    return !!timeout && !!ctx.bikesnestStartedAt && Date.now() - ctx.bikesnestStartedAt >= timeout - 50;
  }

  function retryRequest() {
    var failed = failedRequest;
    var notice = requestNotice();
    var hadFocus = notice && notice.box.contains(document.activeElement);
    hideRequestError();
    if (!failed || !failed.read) return;
    var source = failed.source && failed.source.isConnected ? failed.source : null;
    var target = failed.target && failed.target.isConnected ? failed.target : null;
    if (!target || target === document.body || !source) {
      // A page navigation (or a fragment whose page is gone): navigate for real.
      if (new URL(failed.url, location.href).href === location.href) location.reload();
      else location.assign(failed.url);
      return;
    }
    if (hadFocus) source.focus();
    Promise.resolve(htmx.ajax("GET", failed.url, { source: source, target: target })).catch(function () {});
  }

  function dismissRequestError() {
    var failed = failedRequest;
    var notice = requestNotice();
    var hadFocus = notice && notice.box.contains(document.activeElement);
    hideRequestError();
    var source = failed && failed.source;
    if (hadFocus && source && source.isConnected) source.focus();
  }

  document.addEventListener("htmx:before:request", function (event) {
    if (event.detail.ctx) event.detail.ctx.bikesnestStartedAt = Date.now();
  });
  document.addEventListener("htmx:error", function (event) {
    var ctx = event.detail.ctx;
    var error = event.detail.error;
    if (!ctx || !ctx.request || ctx.response) return;
    if (error && error.name === "AbortError" && !requestTimedOut(ctx)) return;
    showRequestError(ctx);
  });
  document.addEventListener("htmx:after:request", function () {
    var notice = requestNotice();
    if (notice && notice.box && !notice.box.hidden && !notice.box.contains(document.activeElement)) hideRequestError();
  });

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
    else if (event.target.closest("[data-request-retry]")) retryRequest();
    else if (event.target.closest("[data-request-dismiss]")) dismissRequestError();
  });
  window.addEventListener("pageshow", initMaps);
  window.addEventListener("online", initMaps);
  initMaps();
})();
