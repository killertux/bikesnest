// MapLibre GL JS v6 is ESM-only. Keep its module graph at stable same-origin
// URLs so the main module and worker can resolve their shared sibling module
// even when this small entry point is served through the content-hashed route.
import * as maplibregl from "/static/vendor/maplibre-gl.mjs";

maplibregl.setWorkerUrl("/static/vendor/maplibre-gl-worker.mjs");
window.maplibregl = maplibregl;
