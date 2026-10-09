//! The shared page layout and the styled error page.

use crate::auth::Auth;
use crate::i18n::Translator;
use crate::{FragmentErrorVm, assets, htmx, view};
use askama::Template;
use bikesnest_infrastructure::MapConfig;

/// Base layout data shared by all pages. `current` drives the active nav item;
/// `csrf` is the request's session or stable anonymous token, rendered into
/// forms and document metadata for native and htmx submissions.
pub struct PageLayout {
    pub csp_nonce: String,
    pub title: String,
    pub current: String,
    pub csrf: String,
    /// Canonical URL — rendered as `<link rel="canonical">` + `og:url`.
    pub canonical: String,
    /// Meta description + `og:description` (short, localised).
    pub description: String,
    /// OpenGraph type: "website" (default) or "article".
    pub og_type: &'static str,
    /// Browser map implementation selected at startup.
    pub map_provider: &'static str,
    /// Map style URL; rendered onto `<body>` data attributes so the map JS
    /// (search.js / details-map.js) reads it, CSP-safe (no inline script).
    pub map_style_url: String,
    /// Public Mapbox access token for the Mapbox renderer; empty otherwise.
    pub map_access_token: String,
    /// Browser-restricted Google Maps JavaScript key; empty for non-Google maps.
    pub google_maps_api_key: String,
    /// Google cloud map id used by Advanced Markers; empty for non-Google maps.
    pub google_map_id: String,
    /// Whether this request carries a resolved session (signed in). Drives the
    /// header: an account menu vs. Entrar/Criar conta. An anonymous page that
    /// can still carry an anonymous double-submit CSRF token and keeps this
    /// `false` even though `csrf` is non-empty — see [`Self::new`].
    pub is_authenticated: bool,
    /// Session user has MODERATOR or ADMIN (shows the Moderação link).
    pub is_moderator: bool,
    /// Session user has ADMIN (shows Administração / Auditoria).
    pub is_admin: bool,
    /// Session user is signed in AND email-verified — the "Adicionar vaga" /
    /// contribution-entry-point gate.
    pub can_contribute: bool,
}

impl PageLayout {
    /// A bare anonymous page layout: no session identity or token. Request
    /// handlers should normally prefer [`Self::for_request`] so middleware's
    /// anonymous token context is retained. The map
    /// style/token come from the configuration parsed at startup and held in
    /// `AppState`, never from the process environment at render time.
    pub fn new(map: &MapConfig, title: String, current: &str) -> Self {
        let (map_provider, map_style_url, map_access_token, google_maps_api_key, google_map_id) =
            match map {
                MapConfig::MapLibre {
                    style_url,
                    access_token,
                } => (
                    "maplibre",
                    style_url.clone(),
                    access_token.clone(),
                    String::new(),
                    String::new(),
                ),
                MapConfig::Mapbox {
                    style_url,
                    access_token,
                } => (
                    "mapbox",
                    style_url.clone(),
                    access_token.clone(),
                    String::new(),
                    String::new(),
                ),
                MapConfig::Google {
                    browser_api_key,
                    map_id,
                } => (
                    "google",
                    String::new(),
                    String::new(),
                    browser_api_key.clone(),
                    map_id.clone(),
                ),
            };
        Self {
            csp_nonce: String::new(),
            title,
            current: current.to_string(),
            csrf: String::new(),
            canonical: String::new(),
            description: String::new(),
            og_type: "website",
            map_provider,
            map_style_url,
            map_access_token,
            google_maps_api_key,
            google_map_id,
            is_authenticated: false,
            is_moderator: false,
            is_admin: false,
            can_contribute: false,
        }
    }

    /// An anonymous layout carrying a double-submit CSRF token (login,
    /// register, password reset, verify-email pages). Identity flags stay
    /// anonymous — only [`Self::for_request`] fills them from a session.
    pub fn with_csrf(map: &MapConfig, title: String, current: &str, csrf: String) -> Self {
        Self::new(map, title, current).csrf(csrf)
    }

    /// The layout for a request whose [`Auth`] has been resolved (signed in or
    /// not): fills the CSRF token and the four identity flags straight from
    /// the session. This is the constructor every page that has an `Auth`
    /// extractor in scope should use — `new`/`with_csrf` remain for the
    /// anonymous auth pages (login/register/reset/verify), which must render
    /// `is_authenticated = false` even while carrying a double-submit token.
    pub fn for_request(title: String, current: &str, auth: &Auth, map: &MapConfig) -> Self {
        let is_moderator = auth.user.as_ref().is_some_and(|u| {
            u.has_role(bikesnest_domain::Role::Moderator)
                || u.has_role(bikesnest_domain::Role::Admin)
        });
        let is_admin = auth
            .user
            .as_ref()
            .is_some_and(|u| u.has_role(bikesnest_domain::Role::Admin));
        let can_contribute = auth.user.as_ref().is_some_and(|u| u.is_verified);
        Self {
            csp_nonce: auth.csp_nonce.to_string(),
            is_authenticated: auth.authenticated(),
            is_moderator,
            is_admin,
            can_contribute,
            ..Self::new(map, title, current).csrf(auth.csrf_value())
        }
    }

    /// Set (or overwrite) the canonical URL.
    pub fn canonical(mut self, url: impl Into<String>) -> Self {
        self.canonical = url.into();
        self
    }

    pub fn csp_nonce(mut self, nonce: impl ToString) -> Self {
        self.csp_nonce = nonce.to_string();
        self
    }

    /// Set (or overwrite) the meta description.
    pub fn description(mut self, desc: impl Into<String>) -> Self {
        self.description = desc.into();
        self
    }

    /// Set the OpenGraph type ("website" | "article").
    pub fn og_type(mut self, og_type: &'static str) -> Self {
        self.og_type = og_type;
        self
    }

    /// Set (or overwrite) the CSRF token on an existing layout (for pages whose
    /// `current`/`title` are computed elsewhere, e.g. details).
    pub fn csrf(mut self, csrf: String) -> Self {
        self.csrf = csrf;
        self
    }

    /// Resolves `path` (relative to `static_root`, forward-slash separated —
    /// e.g. `"css/app.css"`, `"js/maplibre-loader.mjs"`) to its content-hashed
    /// `/static/h/<hash>/<path>` URL. Falls back to the plain
    /// `/static/<path>` when the asset manifest hasn't been built yet or the
    /// path isn't in it, so a template call here never produces a broken
    /// link — just one without the long-lived cache header. Askama calls
    /// this as `layout.asset("css/app.css")`.
    pub fn asset(&self, path: &str) -> String {
        assets::resolve(path)
    }

    /// The origin (`scheme://host[:port]`) of `map_style_url`, or empty when
    /// no style is configured. Used for `<link rel="preconnect">` on pages
    /// with a map — computed here so the template never parses a URL.
    pub fn tile_origin(&self) -> String {
        let url = &self.map_style_url;
        if url.starts_with("mapbox://") {
            return "https://api.mapbox.com".to_string();
        }
        let Some(scheme_end) = url.find("://") else {
            return String::new();
        };
        let after_scheme = &url[scheme_end + 3..];
        let host_end = after_scheme
            .find(['/', '?', '#'])
            .unwrap_or(after_scheme.len());
        format!("{}{}", &url[..scheme_end + 3], &after_scheme[..host_end])
    }

    pub fn uses_maplibre(&self) -> bool {
        self.map_provider == "maplibre"
    }

    pub fn uses_mapbox(&self) -> bool {
        self.map_provider == "mapbox"
    }

    pub fn uses_google_maps(&self) -> bool {
        self.map_provider == "google"
    }

    /// The `west,south,east,north` box behind every "browse the map" link in
    /// the chrome (the nav's parking item, the home page's explore link).
    /// A method rather than a field: it is a constant, and every layout would
    /// otherwise have to carry it.
    pub fn browse_bbox(&self) -> String {
        view::featured_bbox_param()
    }
}

/// Error page, styled via Tailwind tokens.
#[derive(Template)]
#[template(path = "pages/error.html")]
pub struct ErrorPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub status: u16,
    pub message: String,
    pub recovery_url: String,
    pub login_url: String,
}

/// One translated failure, rendered the way the caller can use it: a real
/// fragment request gets `partials/fragment_error.html` (it is swapped into a
/// live target), everything else gets the styled `pages/error.html` document.
/// Both keep `status` — htmx 4 swaps 4xx/5xx bodies (only `config.noSwap`
/// (204/304) is skipped), so an error body must be swap-safe, not a bare
/// string that lands inside a button.
///
/// `auth` renders the right header identity on the styled page (an error page
/// is still a whole document with the usual nav). Every real request has one
/// (the auth middleware wraps the whole router, `not_found`'s fallback and
/// `styled_errors`'s last line of defence both extract it) — callers with no
/// resolved session pass `&Auth::default()`, which renders the anonymous
/// header, exactly like any other unauthenticated page.
pub fn error_response(
    headers: &axum::http::HeaderMap,
    map: &MapConfig,
    auth: &Auth,
    tr: Translator,
    status: axum::http::StatusCode,
    message: String,
) -> axum::response::Response {
    use askama::Template as _;
    use axum::response::{Html, IntoResponse};

    let html = if htmx::is_fragment_request(headers) {
        FragmentErrorVm {
            tr,
            message: message.clone(),
        }
        .render()
    } else {
        // Keep the two titles a visitor (and a search engine) actually sees on
        // the pages they land on; everything else is a generic "Error".
        let title_key = match status.as_u16() {
            404 => "error.404.title",
            s if s >= 500 => "error.500.title",
            _ => "error.title",
        };
        ErrorPage {
            layout: PageLayout::for_request(
                format!("{} — BikesNest", tr.t(title_key)),
                "",
                auth,
                map,
            ),
            tr,
            status: status.as_u16(),
            message: message.clone(),
            recovery_url: auth.next.clone(),
            login_url: auth.login_url(),
        }
        .render()
    };
    let mut resp = match html {
        Ok(body) => (status, Html(body)).into_response(),
        // A template failure is a bug; the message still reaches the user.
        Err(e) => {
            tracing::error!(category = "template_render", error = %e, "error page render failed");
            (status, message).into_response()
        }
    };
    resp.headers_mut().append(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static(htmx::VARY_FRAGMENT),
    );
    resp
}
