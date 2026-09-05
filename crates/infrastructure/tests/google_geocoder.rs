//! Opt-in live smoke tests for Google Maps Platform.
//!
//! Set `GOOGLE_MAPS_TEST_API_KEY` on a Google Cloud project with Geocoding API
//! and Places API (New) enabled. The normal workspace suite skips these calls.

use bikesnest_application::Geocoder;
use bikesnest_infrastructure::GoogleGeocoder;

fn geocoder() -> Option<GoogleGeocoder> {
    std::env::var("GOOGLE_MAPS_TEST_API_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .map(GoogleGeocoder::new)
}

#[tokio::test]
async fn geocodes_a_real_curitiba_address() {
    let Some(geocoder) = geocoder() else {
        return;
    };
    let hit = geocoder
        .geocode("Rua XV de Novembro, Curitiba, Paraná, Brasil")
        .await
        .expect("Google request succeeds")
        .expect("the address resolves");
    assert!((-26.0..=-25.0).contains(&hit.point.lat()));
    assert!((-50.0..=-48.0).contains(&hit.point.lon()));
}

#[tokio::test]
async fn suggests_and_resolves_a_curitiba_address() {
    let Some(geocoder) = geocoder() else {
        return;
    };
    let session = "550e8400-e29b-41d4-a716-446655440000";
    let suggestions = geocoder
        .suggest("Rua XV de Nov", 10, Some(session), "pt-BR")
        .await
        .expect("Google autocomplete succeeds");
    let selected = suggestions
        .first()
        .expect("Google returns a prediction for Rua XV");
    let reference = selected.reference.as_deref().expect("a Google Place ID");
    let hit = geocoder
        .resolve_suggestion(reference, Some(session))
        .await
        .expect("Google Place Details succeeds")
        .expect("the selected place has coordinates");
    assert!((-26.0..=-25.0).contains(&hit.point.lat()));
    assert!((-50.0..=-48.0).contains(&hit.point.lon()));
}
