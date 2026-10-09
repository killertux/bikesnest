//! The error responses: the styled 404/500 pages, the fragment-shaped
//! variants, and the middleware that upgrades the plain-text failures axum
//! and the router emit into the same styled page.

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::{extract::State, middleware};
use bikesnest_infrastructure::MapConfig;

use crate::auth::Auth;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;

/// A translated failure page. Like every other error path this respects the
/// request's shape: a real htmx fragment request gets
/// `partials/fragment_error.html` (it is swapped into a live target, so a whole
/// document would land inside it), anything else gets the styled `error.html`.
pub(crate) fn error_page(
    headers: &HeaderMap,
    map: &MapConfig,
    auth: &Auth,
    tr: Translator,
    status: StatusCode,
    body_key: &str,
) -> Response {
    crate::error_response(headers, map, auth, tr, status, tr.t(body_key).to_string())
}

/// The styled 500. `error` is what went wrong; it is logged here at error
/// level with its whole `source()` chain, because the page itself says only
/// "something went wrong" and a stored row that no longer decodes would
/// otherwise fail silently. Callers pass application errors, whose messages
/// carry codes and kinds (a bad timezone name, an unknown state code, a
/// database failure class), never request input or account data.
pub(crate) fn internal_error(
    headers: &HeaderMap,
    map: &MapConfig,
    auth: &Auth,
    tr: Translator,
    error: &(dyn std::error::Error + 'static),
) -> Response {
    tracing::error!(
        category = "internal_error",
        error = %error_chain(error),
        "request failed with an internal error"
    );
    error_page(
        headers,
        map,
        auth,
        tr,
        StatusCode::INTERNAL_SERVER_ERROR,
        "error.500.body",
    )
}

/// `error` and each of its sources, joined as `outer: inner: root`.
pub(crate) fn error_chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut out = error.to_string();
    let mut source = error.source();
    while let Some(e) = source {
        out.push_str(": ");
        out.push_str(&e.to_string());
        source = e.source();
    }
    out
}

pub(crate) fn not_found_page(
    headers: &HeaderMap,
    map: &MapConfig,
    auth: &Auth,
    tr: Translator,
) -> Response {
    error_page(
        headers,
        map,
        auth,
        tr,
        StatusCode::NOT_FOUND,
        "error.404.body",
    )
}

/// Router fallback. A 404 is reachable by every kind of request — a typed
/// URL, a boosted link, a stale htmx fragment poll — so it answers each in its
/// own shape rather than always emitting a whole document. `auth` renders the
/// right header (a signed-in user's stray/mistyped link still shows Sair, not
/// Entrar) — an unmatched route runs through the same auth middleware as
/// every other route, so the session is always resolved here too.
pub(crate) async fn not_found(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
) -> Response {
    not_found_page(&headers, &state.map, &auth, Translator::new(locale))
}

/// Paths whose plain-text bodies are the contract, not a leaked failure: the
/// probes and the static file service. (Media is served via direct S3
/// presigned URLs, not through this app, so there is no media route here.)
pub(crate) fn skips_styled_errors(path: &str) -> bool {
    path == "/healthz"
        || path == "/readyz"
        || path == "/robots.txt"
        || path == "/sitemap.xml"
        || path.starts_with("/static/")
}

/// True when the caller would understand an HTML answer (no `Accept` at all,
/// or one that admits `text/html` / `*/*`).
pub(crate) fn accepts_html(headers: &HeaderMap) -> bool {
    match headers.get(header::ACCEPT).and_then(|v| v.to_str().ok()) {
        None => true,
        Some(a) => a.contains("text/html") || a.contains("*/*"),
    }
}

/// A translated sentence for a status the router (rather than a handler)
/// produced, so the user never sees axum's English rejection text.
pub(crate) fn status_message(tr: Translator, status: StatusCode) -> String {
    let key = match status {
        StatusCode::UNAUTHORIZED => "error.login_required",
        StatusCode::FORBIDDEN => "error.forbidden",
        StatusCode::NOT_FOUND => "error.404.body",
        StatusCode::METHOD_NOT_ALLOWED => "error.method_not_allowed",
        StatusCode::CONFLICT => "error.conflict",
        StatusCode::PAYLOAD_TOO_LARGE => "error.too_large",
        StatusCode::TOO_MANY_REQUESTS => "error.too_many",
        StatusCode::SERVICE_UNAVAILABLE => "error.unavailable",
        s if s.is_server_error() => "error.500.body",
        _ => "error.bad_request",
    };
    tr.t(key).to_string()
}

/// Last line of defence for the 404/500 family: any failing response that is still bare text
/// (axum's extractor rejections, the router's 405, a stray literal) is re-rendered
/// as the styled error page — or as `partials/fragment_error.html` when htmx
/// asked for a fragment, because htmx 4 swaps 4xx/5xx bodies too.
pub(crate) async fn styled_errors(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let headers = req.headers().clone();
    // `auth_middleware` runs outside this layer and already stashed the
    // resolved session as a request extension — grab it before `next.run`
    // consumes `req`, so a failure this last line of defence re-renders still
    // gets the signed-in header rather than falling back to anonymous.
    let auth = req.extensions().get::<Auth>().cloned().unwrap_or_default();
    let res = next.run(req).await;

    if res.status().is_success() || res.status().is_redirection() {
        return res;
    }
    if skips_styled_errors(&path) || !accepts_html(&headers) {
        return res;
    }
    let is_plain = match res.headers().get(header::CONTENT_TYPE) {
        None => true,
        Some(ct) => ct
            .to_str()
            .is_ok_and(|ct| ct.starts_with("text/plain") || ct.starts_with("application/octet")),
    };
    if !is_plain {
        return res;
    }
    let status = res.status();
    let tr = Translator::new(Locale::from_headers(&headers));
    let mut styled = crate::error_response(
        &headers,
        &state.map,
        &auth,
        tr,
        status,
        status_message(tr, status),
    );
    // Keep whatever the inner response set (`Allow` on a 405, `Set-Cookie`, …);
    // only the body and its content type are replaced.
    for (name, value) in res.headers() {
        if name != header::CONTENT_TYPE && name != header::CONTENT_LENGTH {
            styled.headers_mut().append(name, value.clone());
        }
    }
    styled
}

#[cfg(test)]
mod tests {
    use super::error_chain;

    #[test]
    fn error_chain_includes_every_source() {
        let err = bikesnest_application::DetailsError::Read(
            bikesnest_application::ReaderError::Unexpected("bad timezone Mars/Olympus".into()),
        );
        // `DetailsError::Read` is transparent, so the chain is the reader's
        // message followed by nothing else.
        assert_eq!(
            error_chain(&err),
            "database error: bad timezone Mars/Olympus"
        );

        #[derive(Debug)]
        struct Outer(std::io::Error);
        impl std::fmt::Display for Outer {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("outer")
            }
        }
        impl std::error::Error for Outer {
            fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
                Some(&self.0)
            }
        }
        let nested = Outer(std::io::Error::other("root cause"));
        assert_eq!(error_chain(&nested), "outer: root cause");
    }
}
