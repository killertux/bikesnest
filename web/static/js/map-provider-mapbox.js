/* Provider-neutral map contract backed by the vendored Mapbox runtime. */
(function () {
  "use strict";

  var cfg = document.body ? document.body.dataset : {};
  var styleUrl = cfg.mapStyleUrl || "https://tiles.openfreemap.org/styles/liberty";
  var accessToken = cfg.mapAccessToken || "";

  function position(value) {
    return [value.lon, value.lat];
  }

  function MapboxAdapter(el, options) {
    if (accessToken) window.mapboxgl.accessToken = accessToken;
    this.raw = new window.mapboxgl.Map({
      container: el,
      style: styleUrl,
      center: position(options.center),
      zoom: options.zoom,
    });
    if (options.navigation !== false) {
      this.raw.addControl(new window.mapboxgl.NavigationControl());
    }
  }

  MapboxAdapter.prototype.onLoad = function (handler) {
    this.raw.on("load", handler);
  };
  MapboxAdapter.prototype.onMoveEnd = function (handler) {
    this.raw.on("moveend", handler);
  };
  MapboxAdapter.prototype.onClick = function (handler) {
    this.raw.on("click", function (event) {
      handler({ lat: event.lngLat.lat, lon: event.lngLat.lng });
    });
  };
  MapboxAdapter.prototype.addMarker = function (options) {
    var marker = new window.mapboxgl.Marker({
      element: options.element,
      draggable: !!options.draggable,
      anchor: options.anchor || "center",
    }).setLngLat(position(options.position));
    if (options.popup) {
      marker.setPopup(
        new window.mapboxgl.Popup({ offset: 16, closeButton: true })
          .setDOMContent(options.popup)
      );
    }
    marker.addTo(this.raw);
    if (options.onDragEnd) {
      marker.on("dragend", function () {
        var at = marker.getLngLat();
        options.onDragEnd({ lat: at.lat, lon: at.lng });
      });
    }
    return {
      remove: function () { marker.remove(); },
      getElement: function () { return marker.getElement(); },
      togglePopup: function () { marker.togglePopup(); },
      setPosition: function (at) { marker.setLngLat(position(at)); },
      getPosition: function () {
        var at = marker.getLngLat();
        return { lat: at.lat, lon: at.lng };
      },
    };
  };
  MapboxAdapter.prototype.getBounds = function () {
    var bounds = this.raw.getBounds();
    return {
      west: bounds.getWest(),
      south: bounds.getSouth(),
      east: bounds.getEast(),
      north: bounds.getNorth(),
    };
  };
  MapboxAdapter.prototype.getZoom = function () { return this.raw.getZoom(); };
  MapboxAdapter.prototype.easeTo = function (options) {
    this.raw.easeTo({ center: position(options.center), zoom: options.zoom });
  };
  MapboxAdapter.prototype.flyTo = function (options) {
    this.raw.flyTo({ center: position(options.center), zoom: options.zoom });
  };
  MapboxAdapter.prototype.jumpTo = function (options) {
    this.raw.jumpTo({ center: position(options.center), zoom: options.zoom });
  };
  MapboxAdapter.prototype.fitBounds = function (bounds, options) {
    this.raw.fitBounds(
      [[bounds[0], bounds[1]], [bounds[2], bounds[3]]],
      options || {}
    );
  };
  MapboxAdapter.prototype.resize = function () { this.raw.resize(); };

  window.BikesNestMapProvider = {
    name: "mapbox",
    ready: function () {
      return window.mapboxgl
        ? Promise.resolve()
        : Promise.reject(new Error("mapbox_unavailable"));
    },
    createMap: function (el, options) {
      return new MapboxAdapter(el, options);
    },
  };
})();
