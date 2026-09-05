use super::*;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use bikesnest_infrastructure::MapConfig;
use i18n::Locale;

fn map() -> MapConfig {
    MapConfig::open_free_map()
}

#[test]
fn google_map_layout_exposes_only_the_browser_credential() {
    let layout = PageLayout::new(
        &MapConfig::Google {
            browser_api_key: "browser-key".to_string(),
            map_id: "map-id".to_string(),
        },
        "Map".to_string(),
        "search",
    );
    assert!(layout.uses_google_maps());
    assert!(!layout.uses_maplibre());
    assert_eq!(layout.google_maps_api_key, "browser-key");
    assert_eq!(layout.google_map_id, "map-id");
    assert!(layout.map_style_url.is_empty());
}

#[test]
fn mapbox_style_preconnects_to_the_https_api() {
    let layout = PageLayout::new(
        &MapConfig::Mapbox {
            style_url: "mapbox://styles/example/streets".to_string(),
            access_token: "public-token".to_string(),
        },
        "Map".to_string(),
        "search",
    );
    assert!(layout.uses_mapbox());
    assert!(!layout.uses_maplibre());
    assert_eq!(layout.tile_origin(), "https://api.mapbox.com");
}

async fn body_of(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    String::from_utf8_lossy(&bytes).to_string()
}

/// The 500 branch `search` / `parking_details` take when the read side
/// fails: a fragment request must not receive a whole document.
#[tokio::test]
async fn error_response_renders_a_fragment_for_a_fragment_request() {
    let mut headers = HeaderMap::new();
    headers.insert(htmx::HX_REQUEST, HeaderValue::from_static("true"));
    let tr = i18n::Translator::new(Locale::En);
    let resp = error_response(
        &headers,
        &map(),
        &Auth::default(),
        tr,
        StatusCode::INTERNAL_SERVER_ERROR,
        tr.t("error.500.body").to_string(),
    );
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        resp.headers()
            .get_all(axum::http::header::VARY)
            .iter()
            .count(),
        1
    );
    let body = body_of(resp).await;
    assert!(body.contains(r#"role="alert""#), "{body}");
    assert!(!body.contains("<html"), "not a document: {body}");
}

#[tokio::test]
async fn error_response_renders_the_page_for_a_document_request() {
    let tr = i18n::Translator::new(Locale::En);
    let resp = error_response(
        &HeaderMap::new(),
        &map(),
        &Auth::default(),
        tr,
        StatusCode::INTERNAL_SERVER_ERROR,
        tr.t("error.500.body").to_string(),
    );
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = body_of(resp).await;
    assert!(body.contains("<!DOCTYPE"), "a whole document: {body}");
    assert!(body.contains("500"), "the status is on the page");
    assert!(body.contains("Something went wrong"), "the 500 title/body");
}

/// A boosted navigation carries `HX-Request` but swaps `<body>`.
#[tokio::test]
async fn error_response_gives_a_boosted_request_the_page() {
    let mut headers = HeaderMap::new();
    headers.insert(htmx::HX_REQUEST, HeaderValue::from_static("true"));
    headers.insert(htmx::HX_BOOSTED, HeaderValue::from_static("true"));
    let tr = i18n::Translator::new(Locale::En);
    let resp = error_response(
        &headers,
        &map(),
        &Auth::default(),
        tr,
        StatusCode::NOT_FOUND,
        tr.t("error.404.body").to_string(),
    );
    let body = body_of(resp).await;
    assert!(body.contains("<!DOCTYPE"), "a whole document: {body}");
}
