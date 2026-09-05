//! Live integration tests for the Mapbox geocoder.
//!
//! The network-dependent test is **gated on `MAPBOX_TEST_TOKEN`**: when it is
//! unset the test is skipped (logged, still passes), so the default `cargo test`
//! run stays green with no credentials. Run it live with:
//!
//! ```bash
//! MAPBOX_TEST_TOKEN=pk.xxxx cargo test -p bikesnest-infrastructure --test mapbox_geocoder -- --nocapture
//! ```

use bikesnest_application::Geocoder;
use bikesnest_infrastructure::MapboxGeocoder;

fn token() -> Option<String> {
    std::env::var("MAPBOX_TEST_TOKEN")
        .ok()
        .filter(|t| !t.is_empty())
}

/// The blank-query guard runs before any network call, so it is not gated.
#[tokio::test]
async fn blank_query_returns_none_without_network() {
    let geo = MapboxGeocoder::new("any-token");
    assert!(geo.geocode("   ").await.expect("no error").is_none());
}

#[tokio::test]
async fn geocodes_a_real_place() {
    let Some(token) = token() else {
        eprintln!("MAPBOX_TEST_TOKEN not set; skipping live Mapbox test");
        return;
    };
    let geo = MapboxGeocoder::new(token);
    let hit = geo
        .geocode("Rua XV de Novembro, Curitiba")
        .await
        .expect("geocode should succeed");
    let hit = hit.expect("expected at least one feature");
    // Curitiba, Brazil (~-25.43, -49.27). Allow a loose tolerance; the assert is
    // about the point being near Curitiba, not exact.
    assert!(
        (hit.point.lat() - -25.43).abs() < 1.0 && (hit.point.lon() - -49.27).abs() < 1.0,
        "expected a point near Curitiba, got {:?}",
        hit.point
    );
    assert!(!hit.label.is_empty(), "label should be populated");
}

#[tokio::test]
async fn suggests_multiple_curitiba_addresses() {
    let Some(token) = token() else {
        eprintln!("MAPBOX_TEST_TOKEN not set; skipping live Mapbox test");
        return;
    };
    let geo = MapboxGeocoder::new(token);
    let hits = geo
        .suggest("Rua XV de", 10, None, "pt-BR")
        .await
        .expect("suggestions should succeed");
    assert!(hits.len() > 1, "expected more than one suggestion");
    assert!(hits.len() <= 10, "provider limit must be respected");
    assert!(
        hits.iter().all(|hit| !hit.label.is_empty()),
        "every suggestion needs a visible label"
    );
}
