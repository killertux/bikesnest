//! Authentication middleware and extractors. Resolves the session cookie
//! into a principal, enforces CSRF on state-changing authenticated requests,
//! and hands handlers an [`Auth`] principal.

use axum::body::{Body, to_bytes};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use bikesnest_application::{AuthenticatedUser, TokenGenerator};
use bikesnest_domain::{CsrfToken, SessionId};
use bikesnest_infrastructure::MapConfig;

use crate::htmx;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "session_id";
/// Name of the synchronizer-token header / form field.
pub const CSRF_HEADER: &str = "x-csrf-token";
/// Name of the `csrf` query parameter — the token source a multipart form uses,
/// because the middleware cannot parse a multipart body without consuming the
/// stream the handler's `Multipart` extractor needs.
pub const CSRF_QUERY: &str = "csrf";
/// Name of the anonymous double-submit CSRF cookie ( — protects pre-session
/// requests like login/register/reset, which have no session row yet).
pub const ANON_CSRF_COOKIE: &str = "csrf";

/// How much of a urlencoded body the CSRF middleware buffers to find the `csrf`
/// field. Kept well under the route body limits (`DefaultBodyLimit`), which is
/// what makes buffering safe: a body larger than this is not a form post, and
/// is rejected with 413 rather than silently forwarded without its token.
pub const CSRF_BODY_LIMIT: usize = 64 * 1024;

/// The resolved principal for the current request. Always present on requests
/// flowing through the auth middleware (anonymous when no session).
///
/// It also carries the little request context the `require_*` gates need to
/// answer correctly: the locale + map config to render a styled error page, and
/// whether this request wants a swap-safe fragment or a whole document.
#[derive(Debug, Clone)]
pub struct Auth {
    pub user: Option<AuthenticatedUser>,
    pub csrf: Option<CsrfToken>,
    /// The raw session id (for revoking the current session).
    pub session: Option<SessionId>,
    /// Where a successful login should return the user to (see
    /// [`crate::htmx::login_next`]).
    pub next: String,
    /// True when htmx asked for a fragment (not a boosted/history/body swap).
    pub fragment: bool,
    /// Request locale, so a gate can answer in the user's language.
    pub tr: Translator,
    /// Map style/token for the error page's layout.
    pub map: MapConfig,
}

impl Default for Auth {
    fn default() -> Self {
        Self {
            user: None,
            csrf: None,
            session: None,
            next: "/".to_string(),
            fragment: false,
            tr: Translator::new(Locale::PtBr),
            map: MapConfig::open_free_map(),
        }
    }
}

impl Auth {
    pub fn authenticated(&self) -> bool {
        self.user.is_some()
    }

    /// The `/login?next=…` URL for this request.
    pub fn login_url(&self) -> String {
        format!("/login?next={}", htmx::encode_query_value(&self.next))
    }

    /// Returns the authenticated principal, or the anonymous answer.
    ///
    /// A fragment request cannot follow a redirect usefully: htmx's `fetch`
    /// follows it transparently and swaps the whole login page into whatever
    /// small target the control had. So it gets `401` plus `HX-Redirect`, which
    /// htmx turns into a real navigation regardless of status. Everything else
    /// gets the 303 to `/login?next=…`.
    #[allow(clippy::result_large_err)]
    pub fn require_user(&self) -> Result<&AuthenticatedUser, Response> {
        self.user.as_ref().ok_or_else(|| {
            if !self.fragment {
                return Redirect::to(&self.login_url()).into_response();
            }
            // A rendered body (with its content type and its one `Vary`), so the
            // styled-error fallback leaves this response alone — and so a client
            // that ignores `HX-Redirect` still shows why it was refused.
            let mut resp = self.deny(StatusCode::UNAUTHORIZED, "error.login_required");
            if let Ok(value) = HeaderValue::from_str(&self.login_url()) {
                resp.headers_mut().insert(htmx::HX_REDIRECT, value);
            }
            resp
        })
    }

    /// A translated 403, rendered as a fragment or as the styled error page.
    fn forbidden(&self, key: &str) -> Response {
        self.deny(StatusCode::FORBIDDEN, key)
    }

    /// A translated failure at `status`, rendered for this request's shape.
    pub fn deny(&self, status: StatusCode, key: &str) -> Response {
        let headers = self.fragment_headers();
        crate::error_response(
            &headers,
            &self.map,
            self,
            self.tr,
            status,
            self.tr.t(key).to_string(),
        )
    }

    /// The minimal header map `crate::error_response` needs to pick between the
    /// fragment and the document rendering (the middleware already decided).
    fn fragment_headers(&self) -> HeaderMap {
        let mut h = HeaderMap::new();
        if self.fragment {
            h.insert(htmx::HX_REQUEST, HeaderValue::from_static("true"));
        }
        h
    }

    /// Returns the authenticated principal iff it has `role`, else 403.
    #[allow(clippy::result_large_err)]
    pub fn require_role(
        &self,
        role: bikesnest_domain::Role,
    ) -> Result<&AuthenticatedUser, Response> {
        let user = self.require_user()?;
        if user.has_role(role) {
            Ok(user)
        } else {
            Err(self.forbidden("error.forbidden"))
        }
    }

    /// Returns the authenticated principal iff they are a moderator **or** an
    /// admin (the moderation queue grants both).
    #[allow(clippy::result_large_err)]
    pub fn require_moderator(&self) -> Result<&AuthenticatedUser, Response> {
        let user = self.require_user()?;
        if user.has_role(bikesnest_domain::Role::Moderator)
            || user.has_role(bikesnest_domain::Role::Admin)
        {
            Ok(user)
        } else {
            Err(self.forbidden("error.forbidden"))
        }
    }

    /// Returns the authenticated principal iff their email is verified (the
    /// contribution gate), else 403 with the verification notice. Applies
    /// to add/edit/proposal/review/verify; favorites use [`Self::require_user`].
    #[allow(clippy::result_large_err)]
    pub fn require_verified(&self) -> Result<&AuthenticatedUser, Response> {
        let user = self.require_user()?;
        if user.is_verified {
            Ok(user)
        } else {
            Err(self.forbidden("contribution.error.not_verified"))
        }
    }

    /// The session's CSRF token for rendering into a form / meta tag.
    pub fn csrf_value(&self) -> String {
        self.csrf
            .as_ref()
            .map(|c| c.to_base64url())
            .unwrap_or_default()
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Auth {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        Ok(parts.extensions.get::<Auth>().cloned().unwrap_or_default())
    }
}

// ---------------------------------------------------------------------------
// Cookie helpers
// ---------------------------------------------------------------------------

/// Read one named cookie's value from a request's `Cookie` header.
pub fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie
        .split(';')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().to_string())
}

/// Read the raw session id from the `session_id` cookie, if present.
fn session_id_from_headers(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, SESSION_COOKIE)
}

/// `Set-Cookie` header that persists the raw session id (HttpOnly, Secure,
/// SameSite=Lax, Path=/; ).
pub fn set_session_cookie(id: &SessionId) -> String {
    format!(
        "{SESSION_COOKIE}={}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=2592000",
        id.to_hex()
    )
}

/// `Set-Cookie` header that clears the session cookie.
pub fn clear_session_cookie() -> String {
    format!("{SESSION_COOKIE}=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0")
}

/// `Set-Cookie` for the anonymous double-submit CSRF cookie. `SameSite=Lax`
/// means a cross-site POST won't carry it, so it can't be validated — the CSRF
/// defense for pre-session requests (login/register/reset).
pub fn set_anon_csrf_cookie(token: &str) -> String {
    format!("{ANON_CSRF_COOKIE}={token}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=3600")
}

/// Cookie name carrying an export's single-use download token.
///
/// One cookie per export id, scoped with `Path=/account/export/{id}` so the
/// browser only ever sends it to that export's own page and download route
/// (RFC 6265 path matching is per-segment, so `/account/export/12` does not
/// match `/account/export/123`).
pub fn export_cookie_name(id: i64) -> String {
    format!("export_{id}")
}

/// `Set-Cookie` carrying an export's download token.
///
/// The token used to travel in the redirect's query string, which puts it in
/// the browser's history, in any `Referer` the page sends, and in every proxy
/// and access log on the way — for a credential that downloads the user's
/// entire personal-data export. `HttpOnly` also keeps it away from page
/// scripts. `Max-Age` matches the export's own 24-hour TTL, so the cookie dies
/// no later than the thing it unlocks.
pub fn set_export_cookie(id: i64, token: &str) -> String {
    format!(
        "{}={token}; HttpOnly; Secure; SameSite=Lax; Path=/account/export/{id}; Max-Age={}",
        export_cookie_name(id),
        EXPORT_TOKEN_MAX_AGE_SECONDS,
    )
}

/// Lifetime of the export cookie, matching `PrivacyService`'s 24-hour export TTL.
const EXPORT_TOKEN_MAX_AGE_SECONDS: u32 = 24 * 60 * 60;

/// A fresh random token for an anonymous page, set both as the `csrf` cookie
/// and in the form's hidden field / `<meta name="csrf">`.
pub fn anon_csrf_token() -> String {
    CsrfToken::new(bikesnest_infrastructure::RealTokenGenerator.generate()).to_base64url()
}

/// A fresh random hex nonce, for the OAuth `state` parameter. Minted here,
/// beside the CSRF token, so the CSPRNG has a single caller in the web layer.
pub fn random_state_hex() -> String {
    bikesnest_infrastructure::RealTokenGenerator
        .generate()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// CSRF
// ---------------------------------------------------------------------------

/// Constant-time string comparison (defends against a timing side channel).
fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn csrf_cookie_value(headers: &HeaderMap) -> Option<String> {
    cookie_value(headers, ANON_CSRF_COOKIE)
}

fn csrf_from_headers(headers: &HeaderMap) -> Option<String> {
    headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// Extract the `csrf` query parameter. This is the token source for the two
/// **multipart** forms (review, photo upload): the middleware must not touch a
/// multipart body — draining it would leave the handler's `Multipart` extractor
/// with nothing — so those forms carry the token on their `action` instead.
fn csrf_from_query(query: Option<&str>) -> Option<String> {
    for pair in query.unwrap_or("").split('&') {
        if let Some((key, value)) = pair.split_once('=')
            && key == CSRF_QUERY
        {
            return Some(url_decode(value.as_bytes()));
        }
    }
    None
}

/// Extract the `csrf` form field from a urlencoded body.
fn csrf_from_form_body(bytes: &[u8], content_type: Option<&str>) -> Option<String> {
    let ct = content_type.unwrap_or("");
    if !ct.starts_with("application/x-www-form-urlencoded") {
        return None;
    }
    for pair in bytes.split(|&b| b == b'&') {
        let eq = pair.iter().position(|&b| b == b'=');
        let (key, value) = match eq {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => continue,
        };
        if key == b"csrf" {
            return Some(url_decode(value));
        }
    }
    None
}

fn url_decode(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// The auth middleware: resolve the session cookie into a principal, enforce
/// CSRF on state-changing requests, and stash the [`Auth`] extension.
pub async fn auth_middleware(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    // Snapshot what `Auth` needs *before* the await: `&Request<Body>` is not
    // `Send` (axum's `Body` is not `Sync`), and holding one across an await
    // would make the middleware future non-`Send`.
    let auth = resolve_auth(&state, req.method(), req.uri(), req.headers().clone()).await;

    // Safe methods never carry a token. `HEAD` and `OPTIONS` are as read-only
    // as `GET` (axum answers `HEAD` with the `GET` route), so requiring one
    // there only broke `HEAD`.
    let is_safe = matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS);

    if !is_safe {
        // Token sources, in order:
        //   1. `X-CSRF-Token` — every htmx request (web/static/js/auth.js reads
        //      `<meta name="csrf">` in `htmx:config:request`);
        //   2. the `csrf` query parameter — the multipart forms, whose body the
        //      middleware must leave untouched for the `Multipart` extractor;
        //   3. the `csrf` field of a urlencoded body — plain form POSTs.
        let mut submitted =
            csrf_from_headers(req.headers()).or_else(|| csrf_from_query(req.uri().query()));
        if submitted.is_none() {
            let content_type = req
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string);
            let body = std::mem::replace(req.body_mut(), Body::empty());
            match to_bytes(body, CSRF_BODY_LIMIT).await {
                Ok(bytes) => {
                    submitted = csrf_from_form_body(&bytes, content_type.as_deref());
                    // Re-inject the body so the handler's Form extractor can read it.
                    *req.body_mut() = Body::from(bytes);
                }
                // The body is gone: it either exceeded `CSRF_BODY_LIMIT` or the
                // stream failed. Forwarding a request whose body we destroyed
                // would surface as a confusing parse error, so say what happened.
                Err(_) => {
                    return auth.deny(StatusCode::PAYLOAD_TOO_LARGE, "error.too_large");
                }
            }
        }

        let ok = match (&auth.csrf, submitted.as_deref()) {
            // Authenticated: the per-session synchronizer token.
            (Some(session_csrf), Some(sub)) => session_csrf.verify(sub),
            (Some(_), None) => false,
            // Anonymous: double-submit cookie. A cross-site POST won't
            // carry the `csrf` cookie (SameSite=Lax), so it cannot be validated → 403.
            (None, Some(got)) => match csrf_cookie_value(req.headers()) {
                Some(expected) => constant_time_eq(&expected, got),
                None => false,
            },
            (None, None) => false,
        };
        if !ok {
            return auth.deny(StatusCode::FORBIDDEN, "error.csrf");
        }
    }

    req.extensions_mut().insert(auth);
    next.run(req).await
}

async fn resolve_auth(
    state: &AppState,
    method: &Method,
    uri: &axum::http::Uri,
    headers: HeaderMap,
) -> Auth {
    let base = Auth {
        next: htmx::login_next(method, uri, &headers),
        fragment: htmx::is_fragment_request(&headers),
        tr: Translator::new(Locale::from_headers(&headers)),
        map: state.map.clone(),
        ..Auth::default()
    };
    let Some(raw) = session_id_from_headers(&headers) else {
        return base;
    };
    let Some(session_id) = SessionId::from_hex(&raw) else {
        return base;
    };
    match state.auth.resolve_session(&session_id).await {
        Ok(Some(resolved)) => Auth {
            user: Some(resolved.user),
            csrf: Some(resolved.csrf_token),
            session: Some(session_id),
            ..base
        },
        _ => base,
    }
}
