//! Helpers shared by more than one slice: rendering a template into a
//! response, answering an htmx fragment endpoint, and the small parsing
//! utilities the paginated lists share.

use askama::Template;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use bikesnest_domain::LocaleCode;
use bikesnest_infrastructure::MapConfig;

use crate::auth::Auth;
use crate::htmx::{fragment_or_redirect, is_fragment_request};
use crate::i18n::{Locale, Translator};

/// One htmx fragment endpoint's answer, for *any* caller.
///
/// htmx 4 puts `HX-Request` on boosted navigations and history replays too, and
/// both swap `<body>` — so only a real fragment request may be answered with a
/// partial. Everything else (no-JS submit, boosted form post, history replay)
/// gets a whole document: a 303 to the page that now shows the new state on
/// success (post/redirect/get), and the styled error page — at the same status —
/// on failure. htmx 4 swaps 4xx/5xx bodies as well, so the failure fragment is a
/// partial rather than a bare string.
///
/// The whole-document failure page renders an anonymous header: every caller
/// here is a state-changing action endpoint that already required at least
/// [`Auth::require_user`] to reach this point, so the failure itself is
/// reachable only for the safe, generic errors (rate limit, conflict, a
/// stale/invalid target) — not for "not signed in" (that 401/403s earlier,
/// through `Auth::deny`, which does carry the real header). Threading the
/// caller's `Auth` through every one of these small toasts (verify,
/// parked-here, favorite, report, photo upload/moderate) for that edge case
/// was judged not worth the extra parameter on every call site.
pub(crate) fn fragment_answer(
    headers: &HeaderMap,
    map: &MapConfig,
    tr: Translator,
    status: StatusCode,
    message: &str,
    redirect_to: &str,
    fragment: impl FnOnce() -> Response,
) -> Response {
    if !is_fragment_request(headers) && !status.is_success() {
        return crate::error_response(
            headers,
            map,
            &Auth::default(),
            tr,
            status,
            message.to_string(),
        );
    }
    fragment_or_redirect(headers, fragment(), redirect_to)
}

/// Default page size for the moderation queues and other bounded lists.
/// Matches the audit viewer's convention.
pub(crate) const DEFAULT_PAGE_LIMIT: i64 = 50;

/// A keyset cursor, or `None` for the first page.
pub(crate) fn parse_after_id(after_id: i64) -> Option<i64> {
    (after_id > 0).then_some(after_id)
}

/// Percent-encodes a user-supplied search term for a query-string value.
/// Only unreserved characters pass through, so a term containing `&`, `#` or a
/// space cannot rewrite the rest of the URL.
pub(crate) fn urlencoding_query(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// URL-encodes an RFC3339 timestamp for a query-string cursor value.
pub(crate) fn urlencoding_rfc3339(at: chrono::DateTime<chrono::Utc>) -> String {
    // The only characters RFC3339 introduces that aren't already URL-safe are
    // `:` and `+`; percent-encode just those rather than pulling in a crate.
    at.to_rfc3339().replace('+', "%2B").replace(':', "%3A")
}

pub(crate) fn parse_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// The request locale as the code an account stores. `Locale` is the
/// presentation type (it selects catalog strings and drives templates);
/// `LocaleCode` is what the domain persists and what a queued email carries,
/// which is how mail sent later — with no request in scope — still finds the
/// right language.
pub(crate) fn locale_code(locale: Locale) -> LocaleCode {
    LocaleCode::parse(locale.html_lang()).unwrap_or_default()
}

/// Render a page. A page that declares `template[data-map-assets]` is marked
/// [`MapPage`](crate::security::MapPage) so the security middleware can give
/// it the map provider's policy.
pub(crate) fn render<T: Template>(template: T, status: StatusCode) -> Response {
    match template.render() {
        Ok(html) => {
            let map_page = html.contains("<template data-map-assets");
            let mut response = (status, Html(html)).into_response();
            if map_page {
                response.extensions_mut().insert(crate::security::MapPage);
            }
            response
        }
        // A render failure is a bug; keep the fallback minimal (no template).
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Internal error").into_response(),
    }
}

/// Render an anonymous form page. The auth middleware owns the anonymous
/// token cookie lifecycle; handlers only render its request-scoped value.
pub(crate) fn render_anon<T: Template>(page: T, _token: &str) -> Response {
    render(page, StatusCode::OK)
}
