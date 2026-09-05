//! `/search` — the P2 results page: query mapping, the per-IP geocode
//! budget, and rendering results as a whole page or an htmx fragment.

use axum::extract::{Query, State};
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
use super::errors::internal_error;

/// Query parameters of `/search` (P2). Only mapping — validation and
/// business rules live in the application layer.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct SearchParams {
    /// Plain `String` with a default: Askama templates cannot destructure
    /// `Option<String>` directly, so string params always exist (empty = unset).
    #[serde(default)]
    pub q: String,
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub radius: Option<u32>,
    #[serde(default)]
    pub cost: String,
    /// `type` URL parameter (renamed: `type` is a Rust keyword).
    #[serde(rename = "type", default)]
    pub parking_type: String,
    #[serde(default)]
    pub security: String,
    #[serde(default)]
    pub open_now: String,
    #[serde(default)]
    pub sort: String,
    pub cursor: Option<String>,
    /// Browse mode: `west,south,east,north` in WGS84 degrees, as the map
    /// writes it. Honoured only when there is no destination to search around
    /// — see [`search`].
    #[serde(default)]
    pub bbox: String,
}

impl SearchParams {
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

/// P2 — search results (full page, or HTMX fragment when requested).
pub(crate) async fn search(
    State(state): State<AppState>,
    locale: Locale,
    headers: HeaderMap,
    ClientIp(ip): ClientIp,
    auth: Auth,
    params: Query<SearchParams>,
) -> Response {
    let tr = Translator::new(locale);
    // Only a request htmx will swap into a real target may get the bare results
    // list: a boosted navigation and a back/forward history replay both target
    // `<body>`, and would make the fragment the entire document.
    let is_htmx = is_fragment_request(&headers);
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

/// P2, browse mode — everything inside the map's current viewport.
///
/// The complement of a radius search: no destination, no geocode, no sort and
/// no pages. A box that cannot be honoured is the viewer's URL, not a server
/// fault, so both refusals are a 400 carrying the same styled results notice
/// every other bad-input case uses.
async fn browse(
    state: &AppState,
    tr: Translator,
    auth: &Auth,
    params: &Query<SearchParams>,
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
    params: &Query<SearchParams>,
    results: ResultsData,
    is_htmx: bool,
    status: StatusCode,
) -> Response {
    let can_contribute = auth.user.as_ref().is_some_and(|u| u.is_verified);
    if is_htmx {
        let vm = SearchResultsVm {
            tr,
            results,
            form: params.0.clone(),
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
            form: params.0.clone(),
            security_options: view::security_options(tr, Some(&params.security)),
            type_options: view::type_options(tr, Some(&params.parking_type)),
            oob: false,
            is_authenticated: auth.authenticated(),
            can_contribute,
        };
        vary_fragment(render(vm, status))
    }
}
