/* Provider-neutral map contract backed by Google Maps JavaScript API. */
(function () {
  "use strict";

  var cfg = document.body ? document.body.dataset : {};
  var apiKey = cfg.googleMapsApiKey || "";
  var mapId = cfg.googleMapId || "";
  var loading;

  function loadGoogleMaps() {
    if (window.google && window.google.maps && window.google.maps.Map) {
      return Promise.resolve();
    }
    if (loading) return loading;
    loading = new Promise(function (resolve, reject) {
      var callback = "__bikesnestGoogleMapsReady";
      window[callback] = function () {
        delete window[callback];
        resolve();
      };
      var script = document.createElement("script");
      script.async = true;
      script.src = "https://maps.googleapis.com/maps/api/js?key=" +
        encodeURIComponent(apiKey) + "&loading=async&v=weekly&libraries=marker&callback=" + callback;
      script.onerror = function () {
        delete window[callback];
        loading = null;
        script.remove();
        reject(new Error("Google Maps unavailable"));
      };
      document.head.appendChild(script);
    });
    return loading;
  }

  function latLng(value) {
    return { lat: value.lat, lng: value.lon };
  }

  function GoogleAdapter(el, options) {
    this.loaded = false;
    this.markers = new Set();
    this.raw = new window.google.maps.Map(el, {
      center: latLng(options.center),
      zoom: options.zoom,
      mapId: mapId,
      clickableIcons: false,
      fullscreenControl: false,
      mapTypeControl: false,
      streetViewControl: false,
      zoomControl: options.navigation !== false,
    });
  }

  GoogleAdapter.prototype.onLoad = function (handler) {
    var self = this;
    window.google.maps.event.addListenerOnce(this.raw, "idle", function () {
      self.loaded = true;
      handler();
    });
  };
  GoogleAdapter.prototype.onMoveEnd = function (handler) {
    var self = this;
    this.raw.addListener("idle", function () {
      if (self.loaded) handler();
    });
  };
  GoogleAdapter.prototype.onClick = function (handler) {
    this.raw.addListener("click", function (event) {
      if (!event.latLng) return;
      handler({ lat: event.latLng.lat(), lon: event.latLng.lng() });
    });
  };
  GoogleAdapter.prototype.addMarker = function (options) {
    var marker = new window.google.maps.marker.AdvancedMarkerElement({
      map: this.raw,
      position: latLng(options.position),
      title: options.element.title || options.element.getAttribute("aria-label") || "",
      content: options.element,
      gmpClickable: true,
      gmpDraggable: !!options.draggable,
    });
    var info = null;
    var open = false;
    if (options.popup) {
      info = new window.google.maps.InfoWindow({ content: options.popup });
    }
    var entry = { marker: marker, info: info };
    var markers = this.markers;
    markers.add(entry);
    if (options.onDragEnd) {
      marker.addListener("dragend", function () {
        var at = marker.position;
        var lat = typeof at.lat === "function" ? at.lat() : at.lat;
        var lon = typeof at.lng === "function" ? at.lng() : at.lng;
        options.onDragEnd({ lat: lat, lon: lon });
      });
    }
    return {
      remove: function () {
        marker.map = null;
        if (info) info.close();
        window.google.maps.event.clearInstanceListeners(marker);
        markers.delete(entry);
      },
      getElement: function () { return options.element; },
      togglePopup: function () {
        if (!info) return;
        if (open) info.close();
        else info.open({ map: marker.map, anchor: marker });
        open = !open;
      },
      setPosition: function (at) { marker.position = latLng(at); },
      getPosition: function () {
        var at = marker.position;
        return {
          lat: typeof at.lat === "function" ? at.lat() : at.lat,
          lon: typeof at.lng === "function" ? at.lng() : at.lng,
        };
      },
    };
  };
  GoogleAdapter.prototype.getBounds = function () {
    var bounds = this.raw.getBounds();
    if (!bounds) return null;
    var sw = bounds.getSouthWest();
    var ne = bounds.getNorthEast();
    return { west: sw.lng(), south: sw.lat(), east: ne.lng(), north: ne.lat() };
  };
  GoogleAdapter.prototype.getZoom = function () { return this.raw.getZoom() || 0; };
  GoogleAdapter.prototype.easeTo = function (options) {
    this.raw.panTo(latLng(options.center));
    this.raw.setZoom(options.zoom);
  };
  GoogleAdapter.prototype.flyTo = GoogleAdapter.prototype.easeTo;
  GoogleAdapter.prototype.jumpTo = function (options) {
    this.raw.setCenter(latLng(options.center));
    this.raw.setZoom(options.zoom);
  };
  GoogleAdapter.prototype.fitBounds = function (bounds, options) {
    var target = new window.google.maps.LatLngBounds(
      { lat: bounds[1], lng: bounds[0] },
      { lat: bounds[3], lng: bounds[2] }
    );
    this.raw.fitBounds(target, options && options.padding ? options.padding : 0);
    if (options && options.maxZoom != null) {
      var map = this.raw;
      window.google.maps.event.addListenerOnce(map, "idle", function () {
        if ((map.getZoom() || 0) > options.maxZoom) map.setZoom(options.maxZoom);
      });
    }
  };
  GoogleAdapter.prototype.resize = function () {
    window.google.maps.event.trigger(this.raw, "resize");
  };

  GoogleAdapter.prototype.destroy = function () {
    this.markers.forEach(function (entry) {
      entry.marker.map = null;
      if (entry.info) entry.info.close();
      window.google.maps.event.clearInstanceListeners(entry.marker);
    });
    this.markers.clear();
    window.google.maps.event.clearInstanceListeners(this.raw);
    this.raw = null;
  };

  window.BikesNestMapProvider = {
    name: "google",
    ready: loadGoogleMaps,
    createMap: function (el, options) {
      return new GoogleAdapter(el, options);
    },
  };
})();
