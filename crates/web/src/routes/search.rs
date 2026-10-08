//! `/search` results: query mapping, the per-IP geocode
//! budget, and rendering results as a whole page or an htmx fragment.

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bikesnest_application::SearchInput;

use crate::auth::Auth;
use crate::client_ip::ClientIp;
use crate::htmx::{is_fragment_request, vary_fragment};
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view::{self, ResultsData};
use crate::{PageLayout, SearchPageVm, SearchResultsVm};

use super::common::render;
use super::errors::{error_page, internal_error};

/// Query parameters of `/search`. Only mapping — validation and business rules
/// live in the application layer.
#[derive(Debug, Default, Clone)]
pub struct SearchParams {
    /// Plain `String` with a default: Askama templates cannot destructure
    /// `Option<String>` directly, so string params always exist (empty = unset).
    pub q: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub radius: Option<u32>,
    pub cost: String,
    /// Normalized `type` URL filter values.
    pub parking_type: String,
    pub security: String,
    pub open_now: String,
    pub sort: String,
    pub cursor: Option<String>,
    /// Browse mode: `west,south,east,north` in WGS84 degrees, as the map
    /// writes it. Honoured only when there is no destination to search around
    /// — see [`search`].
    pub bbox: String,
}

impl SearchParams {
    /// Parse the query string with a deliberately narrow repeated-value
    /// contract. HTML checkbox groups submit one key per selected value, but
    /// every other search input is scalar and remains strict about duplicates.
    ///
    /// The application service already accepts comma-delimited filter values,
    /// so repeated `type` and `security` values are normalized to that existing
    /// representation. We do not generalize this behavior to unrelated keys:
    /// accepting a duplicate coordinate, sort, or cursor would hide a malformed
    /// request and make its meaning depend on parameter order.
    pub(crate) fn parse(raw: &str) -> Result<Self, ()> {
        let mut params = Self::default();
        let mut types = Vec::new();
        let mut security = Vec::new();
        let mut scalar_keys = Vec::new();

        for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
            let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = decode_query_component(raw_key)?;
            let value = decode_query_component(raw_value)?;
            match key.as_str() {
                "q" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.q = value;
                }
                "lat" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.lat = Some(value.parse().map_err(|_| ())?);
                }
                "lon" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.lon = Some(value.parse().map_err(|_| ())?);
                }
                "radius" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.radius = Some(value.parse().map_err(|_| ())?);
                }
                "cost" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.cost = value;
                }
                "type" => extend_filter_values(&mut types, value),
                "security" => extend_filter_values(&mut security, value),
                "open_now" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.open_now = value;
                }
                "sort" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.sort = value;
                }
                "cursor" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.cursor = Some(value);
                }
                "bbox" => {
                    mark_scalar(&mut scalar_keys, &key)?;
                    params.bbox = value;
                }
                // Serde's old query extractor ignored unknown query keys; keep
                // that compatibility while keeping known scalar keys strict.
                _ => {}
            }
        }
        params.parking_type = types.join(",");
        params.security = security.join(",");
        Ok(params)
    }

    fn to_input(&self) -> SearchInput {
        SearchInput {
            query: (!self.q.is_empty()).then(|| self.q.clone()),
            lat: self.lat,
            lon: self.lon,
            radius_m: self.radius,
            cost: (!self.cost.is_empty()).then(|| self.cost.clone()),
            types: (!self.parking_type.is_empty()).then(|| self.parking_type.clone()),
            security: (!self.security.is_empty()).then(|| self.security.clone()),
            open_now: self.open_now == "true",
            sort: (!self.sort.is_empty()).then(|| self.sort.clone()),
            page_size: None,
            cursor: self.cursor.clone(),
            bbox: (!self.bbox.is_empty()).then(|| self.bbox.clone()),
        }
    }

    /// Is this a request to browse a map area rather than search around a
    /// destination? A box only counts when nothing better is on offer:
    /// explicit coordinates and a free-text query both name a destination, and
    /// a destination is what the viewer actually asked for.
    fn is_browse(&self) -> bool {
        !self.bbox.trim().is_empty()
            && self.q.trim().is_empty()
            && !(self.lat.is_some() && self.lon.is_some())
    }

    /// Query string without the cursor (for building the next-page link).
    ///
    /// `bbox` is deliberately absent: browse mode has no next page, and a box
    /// that reached a *paginated* answer was ignored by [`Self::is_browse`]
    /// anyway — carrying it into the link would suggest otherwise.
    fn query_string(&self) -> String {
        let mut parts = Vec::new();
        if !self.q.is_empty() {
            parts.push(format!("q={}", urlencode(&self.q)));
        }
        if let Some(lat) = self.lat {
            parts.push(format!("lat={lat}"));
        }
        if let Some(lon) = self.lon {
            parts.push(format!("lon={lon}"));
        }
        if let Some(r) = self.radius {
            parts.push(format!("radius={r}"));
        }
        if !self.cost.is_empty() {
            parts.push(format!("cost={}", urlencode(&self.cost)));
        }
        if !self.parking_type.is_empty() {
            parts.push(format!("type={}", urlencode(&self.parking_type)));
        }
        if !self.security.is_empty() {
            parts.push(format!("security={}", urlencode(&self.security)));
        }
        if self.open_now == "true" {
            parts.push("open_now=true".to_string());
        }
        if !self.sort.is_empty() {
            parts.push(format!("sort={}", urlencode(&self.sort)));
        }
        parts.join("&")
    }

    /// Clear filters without dropping the place currently being searched.
    /// This is intentionally separate from `query_string`, which includes
    /// filters and sort for pagination.
    pub(crate) fn clear_filters_url(&self) -> String {
        let mut parts = Vec::new();
        if !self.q.is_empty() {
            parts.push(format!("q={}", urlencode(&self.q)));
        }
        if let Some(lat) = self.lat {
            parts.push(format!("lat={lat}"));
        }
        if let Some(lon) = self.lon {
            parts.push(format!("lon={lon}"));
        }
        if !self.bbox.is_empty() {
            parts.push(format!("bbox={}", urlencode(&self.bbox)));
        }
        if parts.is_empty() {
            "/search".to_string()
        } else {
            format!("/search?{}", parts.join("&"))
        }
    }
}

fn extend_filter_values(values: &mut Vec<String>, value: String) {
    for code in value
        .split(',')
        .map(str::trim)
        .filter(|code| !code.is_empty())
    {
        if !values.iter().any(|existing| existing == code) {
            values.push(code.to_string());
        }
    }
}

fn mark_scalar(seen: &mut Vec<String>, key: &str) -> Result<(), ()> {
    if seen.iter().any(|seen_key| seen_key == key) {
        return Err(());
    }
    seen.push(key.to_string());
    Ok(())
}

fn decode_query_component(raw: &str) -> Result<String, ()> {
    let mut bytes = Vec::with_capacity(raw.len());
    let raw = raw.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'+' => bytes.push(b' '),
            b'%' if i + 2 < raw.len() => {
                let high = hex_value(raw[i + 1]).ok_or(())?;
                let low = hex_value(raw[i + 2]).ok_or(())?;
                bytes.push(high << 4 | low);
                i += 2;
            }
            b'%' => return Err(()),
            byte => bytes.push(byte),
        }
        i += 1;
    }
    String::from_utf8(bytes).map_err(|_| ())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// A results page carrying nothing but a notice — no destination, no
/// geocoder, or no budget left to call one.
pub(crate) fn results_notice(tr: Translator, key: &str) -> ResultsData {
    ResultsData {
        destination_label: None,
        total_label: String::new(),
        items: Vec::new(),
        cursor_url: None,
        error: Some(tr.t(key).to_string()),
        map_json: serde_json::json!({ "origin": null, "items": [] }).to_string(),
        browse: false,
        refine_hint: None,
    }
}

/// Would serving this search cost a geocode the provider actually bills?
///
/// No, when the request carries coordinates (they win over the query),
/// when there is no query to resolve, or when the in-process cache already
/// holds the answer. A cached destination is free, so it must not count
/// against anyone's budget.
pub(crate) fn geocode_is_billable(state: &AppState, input: &SearchInput) -> bool {
    if input.lat.is_some() && input.lon.is_some() {
        return false;
    }
    match input.query.as_deref().map(str::trim) {
        Some(q) if !q.is_empty() => state.geocoder.peek(q).is_none(),
        _ => false,
    }
}

/// Is this network still inside its per-IP geocode budget?
///
/// A limiter error counts as *over* budget: `fail_open` (the default) is
/// applied inside the limiter, so an error reaching here means the operator
/// asked to refuse rather than let calls through unmetered.
pub(crate) async fn geocode_within_budget(state: &AppState, ip: &str) -> bool {
    let limits = state.geocode_limits;
    matches!(
        state
            .rate_limiter
            .check(&format!("geocode:ip:{ip}"), limits.per_ip, limits.window)
            .await,
        Ok(true)
    )
}

/// Search results (full page, or HTMX fragment when requested).
pub(crate) async fn search(
    State(state): State<AppState>,
    locale: Locale,
    headers: HeaderMap,
    ClientIp(ip): ClientIp,
    auth: Auth,
    RawQuery(raw_query): RawQuery,
) -> Response {
    let tr = Translator::new(locale);
    // Only a request htmx will swap into a real target may get the bare results
    // list: a boosted navigation and a back/forward history replay both target
    // `<body>`, and would make the fragment the entire document.
    let is_htmx = is_fragment_request(&headers);
    let params = match SearchParams::parse(raw_query.as_deref().unwrap_or("")) {
        Ok(params) => params,
        // Preserve strict malformed-query handling without disclosing parser
        // details, using the same localized full/fragment error presentation
        // as the rest of the web surface.
        Err(()) => {
            return error_page(
                &headers,
                &state.map,
                &auth,
                tr,
                StatusCode::BAD_REQUEST,
                "search.invalid",
            );
        }
    };
    let input = params.to_input();
    let query_string = params.query_string();

    // Browse mode short-circuits everything below: a bounding box needs no
    // geocode, so it can neither cost a provider call nor spend a budget.
    if params.is_browse() {
        return browse(&state, tr, &auth, &params, &headers, is_htmx).await;
    }

    // One view of `/search?q=…` can be one billable geocode, so a page that
    // has to resolve free text is metered per IP before the use case runs.
    if geocode_is_billable(&state, &input) && !geocode_within_budget(&state, &ip).await {
        return render_search(
            &state,
            tr,
            &auth,
            &params,
            results_notice(tr, "search.geocode_limited"),
            is_htmx,
            StatusCode::TOO_MANY_REQUESTS,
        );
    }

    let results = match state.search.execute(input).await {
        Ok((page, hit)) => {
            let label = hit
                .as_ref()
                .map(|h| h.label.clone())
                .or_else(|| (!params.q.trim().is_empty()).then(|| params.q.clone()));
            view::build_results(
                tr,
                &page,
                hit.as_ref(),
                label,
                query_string,
                chrono::Utc::now(),
                &state.freshness.thresholds,
                &*state.storage,
            )
            .await
        }
        Err(bikesnest_application::SearchError::MissingDestination) => {
            results_notice(tr, "search.missing")
        }
        // Geocoder outage / rate-limit / bad token → graceful "can't reach the
        // geocoder" page, not a 500 (a hosted provider is a soft dependency).
        Err(bikesnest_application::SearchError::Geocode(_)) => {
            results_notice(tr, "search.geocode_unavailable")
        }
        Err(_) => return internal_error(&headers, &state.map, &auth, tr),
    };

    render_search(&state, tr, &auth, &params, results, is_htmx, StatusCode::OK)
}

/// Browse mode — everything inside the map's current viewport.
///
/// The complement of a radius search: no destination, no geocode, no sort and
/// no pages. A box that cannot be honoured is the viewer's URL, not a server
/// fault, so both refusals are a 400 carrying the same styled results notice
/// every other bad-input case uses.
async fn browse(
    state: &AppState,
    tr: Translator,
    auth: &Auth,
    params: &SearchParams,
    headers: &HeaderMap,
    is_htmx: bool,
) -> Response {
    let notice = |key: &str| {
        render_search(
            state,
            tr,
            auth,
            params,
            results_notice(tr, key),
            is_htmx,
            StatusCode::BAD_REQUEST,
        )
    };
    match state.search.browse(&params.to_input()).await {
        Ok((bounds, page)) => {
            let results = view::build_browse_results(
                tr,
                &bounds,
                &page,
                chrono::Utc::now(),
                &state.freshness.thresholds,
                &*state.storage,
            )
            .await;
            render_search(state, tr, auth, params, results, is_htmx, StatusCode::OK)
        }
        Err(bikesnest_application::SearchError::InvalidBounds) => notice("search.browse.invalid"),
        Err(bikesnest_application::SearchError::BoundsNotPaginated) => {
            notice("search.browse.no_pages")
        }
        Err(_) => internal_error(headers, &state.map, auth, tr),
    }
}

/// Render a results page: the bare list for an htmx swap, the full document
/// otherwise. Both spellings live at the same URL, chosen by the `HX-*`
/// headers — hence the `Vary`, or a cache hands one to the wrong request.
pub(crate) fn render_search(
    state: &AppState,
    tr: Translator,
    auth: &Auth,
    params: &SearchParams,
    results: ResultsData,
    is_htmx: bool,
    status: StatusCode,
) -> Response {
    let can_contribute = auth.user.as_ref().is_some_and(|u| u.is_verified);
    if is_htmx {
        let vm = SearchResultsVm {
            tr,
            results,
            form: params.clone(),
            oob: true,
            is_authenticated: auth.authenticated(),
            can_contribute,
        };
        vary_fragment(render(vm, status))
    } else {
        let vm = SearchPageVm {
            layout: PageLayout::for_request(
                tr.t("search.title").to_string(),
                "search",
                auth,
                &state.map,
            )
            .canonical(format!("{}/search", state.base_url.trim_end_matches('/')))
            .description(tr.t("search.title").to_string()),
            tr,
            results,
            form: params.clone(),
            security_options: view::security_options(tr, Some(&params.security)),
            type_options: view::type_options(tr, Some(&params.parking_type)),
            oob: false,
            is_authenticated: auth.authenticated(),
            can_contribute,
        };
        vary_fragment(render(vm, status))
    }
}

#[cfg(test)]
mod tests {
    use super::SearchParams;

    #[test]
    fn repeated_filter_keys_normalize_with_comma_links_and_deduplicate_values() {
        let params = SearchParams::parse(
            "type=rack&type=indoor,locker&type=rack&security=cctv&security=lighting,cctv",
        )
        .expect("checkbox values are valid");

        assert_eq!(params.parking_type, "rack,indoor,locker");
        assert_eq!(params.security, "cctv,lighting");
    }

    #[test]
    fn duplicate_or_malformed_scalar_values_remain_invalid() {
        for raw in [
            "sort=distance&sort=rating",
            "lat=-25.4&lat=-25.5",
            "radius=far",
            "q=%GG",
        ] {
            assert!(SearchParams::parse(raw).is_err(), "must reject {raw}");
        }
    }

    #[test]
    fn query_components_follow_form_urlencoding_for_unicode_and_encoded_commas() {
        let params = SearchParams::parse(
            "q=Esta%C3%A7%C3%A3o+Central&type=parking%5Ffacility%2Cindoor&security=cctv",
        )
        .unwrap();
        assert_eq!(params.q, "Estação Central");
        assert_eq!(params.parking_type, "parking_facility,indoor");
    }

    #[test]
    fn clearing_filters_retains_the_current_destination_context() {
        let params = SearchParams::parse(
            "q=Rua+das+Bicicletas&lat=-25.4&lon=-49.2&bbox=1,2,3,4&type=rack&sort=distance",
        )
        .unwrap();
        assert_eq!(
            params.clear_filters_url(),
            "/search?q=Rua+das+Bicicletas&lat=-25.4&lon=-49.2&bbox=1%2C2%2C3%2C4"
        );
    }
}
