//! Small JSON endpoints the progressive-enhancement layer calls.
//!
//! `GET /api/address-suggestions` powers the public address comboboxes, while
//! `GET /api/geocode` remains the verified-contributor fallback used by the
//! edit-page map picker. Both share the same per-IP provider budget.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bikesnest_application::Geocoder;

use crate::auth::Auth;
use crate::client_ip::ClientIp;
use crate::i18n::Locale;
use crate::state::AppState;

use super::search::geocode_within_budget;

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct GeocodeQuery {
    #[serde(default)]
    q: String,
    #[serde(default)]
    session: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ResolveSuggestionQuery {
    #[serde(default)]
    id: String,
    #[serde(default)]
    session: Option<String>,
}

const MIN_SUGGESTION_QUERY_CHARS: usize = 3;
const MAX_SUGGESTION_QUERY_CHARS: usize = 200;
const MAX_ADDRESS_SUGGESTIONS: usize = 10;

/// `GET /api/address-suggestions?q=…` returns up to ten provider-ranked hits.
///
/// This endpoint is public because the public parking search is public. It is
/// still bounded by a minimum query length, a maximum query length and the
/// same per-IP budget used by submitted searches.
pub(crate) async fn address_suggestions_api(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    locale: Locale,
    Query(q): Query<GeocodeQuery>,
) -> Response {
    let query = q.q.trim();
    let query_len = query.chars().count();
    if query_len < MIN_SUGGESTION_QUERY_CHARS {
        return json(StatusCode::OK, "[]".to_string());
    }
    if query_len > MAX_SUGGESTION_QUERY_CHARS {
        return json(StatusCode::BAD_REQUEST, "[]".to_string());
    }
    if !geocode_within_budget(&state, &ip).await {
        return json(StatusCode::TOO_MANY_REQUESTS, "[]".to_string());
    }

    let session = match valid_session_token(q.session.as_deref()) {
        Ok(session) => session,
        Err(()) => return json(StatusCode::BAD_REQUEST, "[]".to_string()),
    };
    match state
        .geocoder
        .suggest(query, MAX_ADDRESS_SUGGESTIONS, session, locale.html_lang())
        .await
    {
        Ok(hits) => json(
            StatusCode::OK,
            serde_json::Value::Array(
                hits.into_iter()
                    .take(MAX_ADDRESS_SUGGESTIONS)
                    .map(|hit| {
                        serde_json::json!({
                            "lat": hit.point.as_ref().map(|point| point.lat()),
                            "lon": hit.point.as_ref().map(|point| point.lon()),
                            "label": hit.label,
                            "reference": hit.reference,
                        })
                    })
                    .collect(),
            )
            .to_string(),
        ),
        Err(_) => json(StatusCode::SERVICE_UNAVAILABLE, "[]".to_string()),
    }
}

/// Resolve the opaque reference for a selected autocomplete prediction.
/// Mapbox/fake predictions already carry coordinates and never call this;
/// Google resolves one Place ID at the end of its billing session.
pub(crate) async fn resolve_address_suggestion_api(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    Query(q): Query<ResolveSuggestionQuery>,
) -> Response {
    let reference = q.id.trim();
    if reference.is_empty() || reference.chars().count() > 256 {
        return json(StatusCode::BAD_REQUEST, "{}".to_string());
    }
    let session = match valid_session_token(q.session.as_deref()) {
        Ok(session) => session,
        Err(()) => return json(StatusCode::BAD_REQUEST, "{}".to_string()),
    };
    if !geocode_within_budget(&state, &ip).await {
        return json(StatusCode::TOO_MANY_REQUESTS, "{}".to_string());
    }
    match state.geocoder.resolve_suggestion(reference, session).await {
        Ok(Some(hit)) => json(
            StatusCode::OK,
            serde_json::json!({
                "lat": hit.point.lat(),
                "lon": hit.point.lon(),
                "label": hit.label,
            })
            .to_string(),
        ),
        Ok(None) => json(StatusCode::NOT_FOUND, "{}".to_string()),
        Err(_) => json(StatusCode::SERVICE_UNAVAILABLE, "{}".to_string()),
    }
}

fn valid_session_token(value: Option<&str>) -> Result<Option<&str>, ()> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    (value.len() <= 36
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
    .then_some(Some(value))
    .ok_or(())
}

/// `GET /api/geocode?q=…` → `{"lat":…,"lon":…,"label":"…"}`.
///
/// `404 {}` when nothing matches, `429 {}` when this network has spent its
/// geocode budget, `503 {}` when the provider is unreachable. The picker
/// treats every non-200 the same way: keep the pin where it is and say so.
pub(crate) async fn geocode_api(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    auth: Auth,
    Query(q): Query<GeocodeQuery>,
) -> Response {
    if let Err(resp) = auth.require_verified() {
        return resp;
    }
    let query = q.q.trim();
    if query.is_empty() {
        return json(StatusCode::NOT_FOUND, "{}".to_string());
    }

    // A cached answer is free, so it must not be charged — same rule as the
    // search page's budget check.
    let cached = state.geocoder.peek(query);
    if cached.is_none() && !geocode_within_budget(&state, &ip).await {
        return json(StatusCode::TOO_MANY_REQUESTS, "{}".to_string());
    }

    let hit = match cached {
        Some(hit) => Some(hit),
        None => match state.geocoder.geocode(query).await {
            Ok(hit) => hit,
            Err(_) => return json(StatusCode::SERVICE_UNAVAILABLE, "{}".to_string()),
        },
    };
    match hit {
        Some(hit) => json(
            StatusCode::OK,
            serde_json::json!({
                "lat": hit.point.lat(),
                "lon": hit.point.lon(),
                "label": hit.label,
            })
            .to_string(),
        ),
        None => json(StatusCode::NOT_FOUND, "{}".to_string()),
    }
}

fn json(status: StatusCode, body: String) -> Response {
    // Answers are per-budget; a shared HTTP cache must never replay one
    // caller's result (or 429) to another.
    (
        status,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "application/json; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::valid_session_token;

    #[test]
    fn autocomplete_session_tokens_are_url_safe_and_bounded() {
        assert_eq!(valid_session_token(None), Ok(None));
        assert_eq!(
            valid_session_token(Some("550e8400-e29b-41d4-a716-446655440000")),
            Ok(Some("550e8400-e29b-41d4-a716-446655440000"))
        );
        assert_eq!(valid_session_token(Some("contains space")), Err(()));
        assert_eq!(valid_session_token(Some(&"a".repeat(37))), Err(()));
    }
}
