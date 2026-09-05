use std::path::PathBuf;

fn workspace_file(path: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::read_to_string(root.join(path))
        .unwrap_or_else(|err| panic!("failed to read {path}: {err}"))
}

#[test]
fn alpine_compat_is_vendored_and_loaded_before_alpine() {
    let package = workspace_file("package.json");
    assert!(package.contains("dist/ext/hx-alpine-compat.min.js"));
    assert!(package.contains("web/static/vendor/hx-alpine-compat.js"));

    let extension = workspace_file("web/static/vendor/hx-alpine-compat.js");
    assert!(extension.contains("registerExtension(\"alpine-compat\""));

    let base = workspace_file("templates/layouts/base.html");
    let htmx = base.find("vendor/htmx.js").expect("htmx script");
    let compat = base
        .find("vendor/hx-alpine-compat.js")
        .expect("Alpine compatibility extension script");
    let alpine = base
        .find("vendor/alpine.min.js")
        .expect("deferred Alpine script");
    assert!(htmx < compat, "the extension must load after htmx");
    assert!(compat < alpine, "the extension must load before Alpine");
}

#[test]
fn map_adapters_expose_the_same_contract() {
    let maplibre = workspace_file("web/static/js/map-provider-maplibre.js");
    let mapbox = workspace_file("web/static/js/map-provider-mapbox.js");
    let google = workspace_file("web/static/js/map-provider-google.js");
    for method in [
        "onLoad",
        "onMoveEnd",
        "onClick",
        "addMarker",
        "getBounds",
        "getZoom",
        "easeTo",
        "flyTo",
        "jumpTo",
        "fitBounds",
        "resize",
    ] {
        assert!(maplibre.contains(method), "MapLibre adapter lacks {method}");
        assert!(mapbox.contains(method), "Mapbox adapter lacks {method}");
        assert!(google.contains(method), "Google adapter lacks {method}");
    }

    for consumer in ["search.js", "details-map.js", "pin-picker.js"] {
        let source = workspace_file(&format!("web/static/js/{consumer}"));
        assert!(source.contains("BikesNestMapProvider"));
        assert!(!source.contains("maplibregl."));
        assert!(!source.contains("google.maps."));
    }
}

#[test]
fn address_autocomplete_is_debounced_and_google_attribution_is_rendered() {
    let app = workspace_file("web/static/js/app.js");
    assert!(app.contains("setTimeout(function ()"));
    assert!(app.contains("}, 800);"));
    assert!(app.contains("items.slice(0, 10)"));

    for template in ["home.html", "search.html", "parking_new.html"] {
        let source = workspace_file(&format!("templates/pages/{template}"));
        assert!(source.contains("x-data=\"addressAutocomplete\""));
        assert!(source.contains("powered_by_google_on_white.png"));
    }
}
