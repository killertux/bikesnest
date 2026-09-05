//! BikesNest web crate: axum routing, handlers, Askama templates.

pub mod assets;
pub mod auth;
pub mod client_ip;
pub mod htmx;
pub mod i18n;
pub mod markdown;
pub mod observability;
pub mod routes;
pub mod security;
pub mod state;
pub mod view;
pub mod wiring;

use askama::Template;
use auth::Auth;
use bikesnest_application::ParkingDetailsView;
use bikesnest_infrastructure::MapConfig;
use i18n::Translator;
// The add/edit form's editors are view models like any other, but they live
// next to the grammar that parses them back (routes::contribution_form).
use routes::contribution_form::{
    HiddenField as ContributionHiddenField, HoursDayVm as ContributionHoursDayVm,
    TriStateVm as ContributionTriStateVm,
};

/// Base layout data shared by all pages. `current` drives the active nav item;
/// `csrf` is the per-session synchronizer token (empty when anonymous) — rendered
/// into the `<meta name="csrf">` tag the CSRF middleware / htmx reads.
pub struct PageLayout {
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
    /// still mints a double-submit CSRF token (login/register/reset/verify)
    /// keeps this `false` even though `csrf` is non-empty — see [`Self::new`].
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
    /// An anonymous page layout: no session identity, no CSRF token. The map
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
    /// e.g. `"css/app.css"`, `"vendor/maplibre-gl.js"`) to its content-hashed
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

/// Error page (E1/E2), styled via Tailwind tokens.
#[derive(Template)]
#[template(path = "pages/error.html")]
pub struct ErrorPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub status: u16,
    pub message: String,
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
        }
        .render()
    };
    let mut resp = match html {
        Ok(body) => (status, Html(body)).into_response(),
        // A template failure is a bug; the message still reaches the user.
        Err(_) => (status, message).into_response(),
    };
    resp.headers_mut().append(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static(htmx::VARY_FRAGMENT),
    );
    resp
}

/// Home / landing page.
#[derive(Template)]
#[template(path = "pages/home.html")]
pub struct HomePage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub featured: Vec<view::CardVm>,
}

/// Search results, full page.
#[derive(Template)]
#[template(path = "pages/search.html")]
pub struct SearchPageVm {
    pub layout: PageLayout,
    pub tr: Translator,
    pub results: view::ResultsData,
    pub form: routes::search::SearchParams,
    pub security_options: Vec<view::OptionVm>,
    pub type_options: Vec<view::OptionVm>,
    /// `partials/search_results.html` is included here for the initial render;
    /// this is always `false` so it does not also emit the out-of-band copies
    /// of the destination heading / result count that the standalone HTMX
    /// fragment (`SearchResultsVm`, `oob: true`) uses to update them in place.
    pub oob: bool,
    /// Mirrors `layout.is_authenticated`/`layout.can_contribute`: the "Add a
    /// spot" CTA lives inside `partials/search_results.html`'s empty state too,
    /// which is also rendered standalone (as [`SearchResultsVm`]) without a
    /// `layout` field — so the flags need a home both templates share.
    pub is_authenticated: bool,
    pub can_contribute: bool,
}

impl SearchPageVm {
    // Template-facing helpers: Askama's expression resolver can't compare
    // `Option<String>` directly, so selections are exposed as methods.
    fn sort_is(&self, code: &str) -> bool {
        self.form.sort == code
    }
    fn sort_none(&self) -> bool {
        self.form.sort.is_empty()
    }
    fn cost_is(&self, code: &str) -> bool {
        self.form.cost == code
    }
    fn cost_none(&self) -> bool {
        self.form.cost.is_empty()
    }
    fn open_now_checked(&self) -> bool {
        self.form.open_now == "true"
    }
    fn q_set(&self) -> bool {
        !self.form.q.is_empty()
    }
    fn radius_is(&self, m: u32) -> bool {
        self.form.radius == Some(m)
    }
    fn radius_none(&self) -> bool {
        self.form.radius.is_none()
    }
    /// The explore-the-map box for the empty-search prompt (see
    /// [`PageLayout::browse_bbox`]).
    fn browse_bbox(&self) -> String {
        view::featured_bbox_param()
    }
}

/// HTMX fragment: only the results region.
#[derive(Template)]
#[template(path = "partials/search_results.html")]
pub struct SearchResultsVm {
    pub tr: Translator,
    pub results: view::ResultsData,
    pub form: routes::search::SearchParams,
    /// Always `true`: the destination heading and result count live outside
    /// `#results` in `search.html`, so this fragment updates them via
    /// `hx-swap-oob` alongside the swapped results list.
    pub oob: bool,
    /// See `SearchPageVm::is_authenticated` — this fragment has no `layout`
    /// field, so the empty-state "Add a spot" CTA reads these directly.
    pub is_authenticated: bool,
    pub can_contribute: bool,
}

impl SearchResultsVm {
    /// Same explore-the-map box as the full page: the prompt that carries the
    /// link lives in the shared partial, which is also rendered standalone.
    fn browse_bbox(&self) -> String {
        view::featured_bbox_param()
    }
}

/// Parking details.
#[derive(Template)]
#[template(path = "pages/parking_details.html")]
pub struct DetailsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub id: i64,
    pub name: String,
    pub address: String,
    pub description: Option<String>,
    pub type_label: String,
    pub cost_label: String,
    pub rating_label: String,
    pub has_rating: bool,
    pub freshness_label: &'static str,
    pub freshness_code: &'static str,
    pub open_label: &'static str,
    pub open_code: &'static str,
    pub hours: Vec<view::HoursRowVm>,
    pub timezone_label: String,
    pub security: Vec<SecVm>,
    pub verified_label: String,
    pub osm_url: String,
    pub google_url: String,
    pub lat: f64,
    pub lon: f64,
    /// Approved location photos (presigned URLs), empty when none yet.
    pub gallery: Vec<PhotoVm>,
    // Community additions.
    pub reviews: Vec<view::ReviewVm>,
    pub confidence_code: &'static str,
    pub confidence_label: String,
    pub disputed: bool,
    pub dispute_items: Vec<view::AttrDisputeVm>,
    pub parked_here_count: i64,
    pub is_favorited: bool,
    pub can_contribute: bool,
    pub is_authenticated: bool,
    pub has_own_review: bool,
    pub own_rating: u8,
    pub reasons: Vec<view::ReasonVm>,
    /// A one-time notice banner (post-action confirmation, e.g. "will be reviewed").
    pub notice: Option<String>,
    /// The location's  moderation state code (ACTIVE/PENDING_REVIEW/…). Public
    /// viewers only ever reach ACTIVE; moderators see a banner for the rest.
    pub moderation_state: &'static str,
    /// Whether the viewer is a moderator/admin (sees the hidden/invalid banner).
    pub is_moderator: bool,
    /// The report-reason options for the details-page report modal.
    pub reason_options: Vec<view::OptionVm>,
    /// Public-safe persisted proposals; no voter identities or photo keys.
    pub collaboration_proposals: Vec<CollaborationProposalVm>,
    pub collaboration_history: Vec<CollaborationRevisionVm>,
}

/// One gallery photo: presigned URLs + accessible text. Grid tiles render the
/// (smaller) thumbnail; the lightbox renders the full derivative.
#[derive(Debug, Clone)]
pub struct PhotoVm {
    pub url: String,
    pub thumb_url: String,
    pub alt: String,
}

impl DetailsPage {
    pub fn build(
        map: &MapConfig,
        tr: Translator,
        v: ParkingDetailsView,
        gallery: Vec<PhotoVm>,
        auth: &Auth,
    ) -> Self {
        use bikesnest_domain::OpenStatus;
        let now = chrono::Utc::now();
        let loc = &v.location;
        let (lat, lon) = (loc.point().lat(), loc.point().lon());
        let open_code = match v.is_open_now {
            OpenStatus::Open => "open",
            OpenStatus::Closed => "closed",
            OpenStatus::Unknown => "unknown",
        };
        let open_label = view::open_label(tr, v.is_open_now);
        Self {
            layout: PageLayout::for_request(format!("{} — BikesNest", loc.name()), "", auth, map),
            tr,
            id: loc.id(),
            name: loc.name().to_string(),
            address: loc.address().to_string(),
            description: loc.description().map(str::to_string),
            type_label: view::type_label(tr, loc.parking_type()).to_string(),
            cost_label: view::cost_label(tr, loc.cost()),
            rating_label: view::rating_label(tr, loc.rating().avg(), loc.rating().count()),
            has_rating: loc.rating().avg().is_some(),
            freshness_label: view::freshness_label(tr, v.freshness),
            freshness_code: v.freshness.as_code(),
            open_label,
            open_code,
            hours: view::hours_rows(tr, loc.hours(), loc.timezone(), now),
            timezone_label: loc.timezone().name().to_string(),
            security: loc
                .security()
                .iter()
                .map(|f| SecVm {
                    label: tr.security(f.code()).to_string(),
                    state: match f.state() {
                        bikesnest_domain::SecurityState::Yes => "yes",
                        bikesnest_domain::SecurityState::No => "no",
                        bikesnest_domain::SecurityState::Unknown => "unknown",
                    },
                })
                .collect(),
            verified_label: match loc.last_verified_at() {
                Some(t) => {
                    let days = (now - t).num_days();
                    if days == 0 {
                        tr.t("verified.today").to_string()
                    } else if days == 1 {
                        tr.t("verified.yesterday").to_string()
                    } else {
                        tr.t("verified.days_ago").replace("{n}", &days.to_string())
                    }
                }
                None => tr.t("verified.never").to_string(),
            },
            // : external navigation only — links to providers, coordinates
            // only (no user data is sent; the user leaves the app to navigate).
            osm_url: format!(
                "https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=18/{lat}/{lon}"
            ),
            google_url: format!("https://www.google.com/maps/dir/?api=1&destination={lat},{lon}"),
            lat,
            lon,
            gallery,
            reviews: Vec::new(),
            confidence_code: "reported",
            confidence_label: view::confidence_label(tr, bikesnest_domain::Confidence::Reported)
                .to_string(),
            disputed: false,
            dispute_items: Vec::new(),
            parked_here_count: 0,
            is_favorited: false,
            can_contribute: false,
            is_authenticated: false,
            has_own_review: false,
            own_rating: 0,
            reasons: Vec::new(),
            notice: None,
            moderation_state: loc.moderation_state().as_code(),
            is_moderator: false,
            reason_options: view::report_reason_options(tr),
            collaboration_proposals: Vec::new(),
            collaboration_history: Vec::new(),
        }
    }

    /// Build the details page with the community view (reviews, confidence,
    /// verification panel, favorite, recommendation explanation) overlaid on
    /// the base detail view. `auth`'s verified/authenticated/moderator status
    /// gates the contributor actions; anonymous viewers get a public-only page.
    pub async fn build_community(
        map: &MapConfig,
        tr: Translator,
        v: bikesnest_application::ParkingDetailsView,
        gallery: Vec<PhotoVm>,
        auth: &Auth,
        community: Option<bikesnest_application::CommunityParkingDetails>,
        storage: &dyn bikesnest_application::ObjectStorage,
    ) -> Self {
        let mut page = Self::build(map, tr, v, gallery, auth);
        let Some(c) = community else { return page };
        let mut reviews = Vec::with_capacity(c.reviews.len());
        for r in &c.reviews {
            let mut photos = Vec::new();
            if let Some(ps) = c.review_photos.get(&r.id) {
                for p in ps {
                    let Some(url) = view::resolve_photo(storage, Some(&p.key)).await else {
                        continue;
                    };
                    let thumb_url = match p.thumbnail_key.as_deref() {
                        Some(k) => view::resolve_photo(storage, Some(k))
                            .await
                            .unwrap_or_else(|| url.clone()),
                        None => url.clone(),
                    };
                    photos.push(PhotoVm {
                        url,
                        thumb_url,
                        alt: p.alt.clone().unwrap_or_else(|| "Review photo".to_string()),
                    });
                }
            }
            reviews.push(view::review_vm(tr, r, false, photos));
        }
        page.reviews = reviews;
        page.confidence_code = c.confidence.as_code();
        page.confidence_label = view::confidence_label(tr, c.confidence).to_string();
        page.disputed = c.disputed;
        page.dispute_items = c
            .attribute_summary
            .iter()
            .filter(|a| a.incorrect > 0)
            .map(|a| view::attr_dispute_vm(tr, &a.code, a.incorrect))
            .collect();
        page.parked_here_count = c.parked_here_count;
        page.is_favorited = c.is_favorited;
        page.can_contribute = auth.user.as_ref().is_some_and(|u| u.is_verified);
        page.is_authenticated = auth.authenticated();
        page.has_own_review = c.own_review.is_some();
        page.own_rating = c.own_review.map(|r| r.rating.value()).unwrap_or(0);
        page.reasons = c.reasons.iter().map(|r| view::reason_vm(tr, r)).collect();
        page.is_moderator = auth.user.as_ref().is_some_and(|u| {
            u.has_role(bikesnest_domain::Role::Moderator)
                || u.has_role(bikesnest_domain::Role::Admin)
        });
        page
    }

    /// Set (or overwrite) the page-level notice banner (e.g. "your change will
    /// be reviewed").
    pub fn notice(mut self, notice: Option<String>) -> Self {
        self.notice = notice;
        self
    }

    pub fn collaboration_proposals(mut self, proposals: Vec<CollaborationProposalVm>) -> Self {
        self.collaboration_proposals = proposals;
        self
    }
    pub fn collaboration_history(mut self, history: Vec<CollaborationRevisionVm>) -> Self {
        self.collaboration_history = history;
        self
    }
}

pub struct CollaborationProposalVm {
    pub id: i64,
    pub kind_label: String,
    pub reason: Option<String>,
    pub status: &'static str,
    pub approvals: i64,
    pub rejections: i64,
}

pub struct CollaborationRevisionVm {
    pub version: i64,
    pub kind: String,
    pub summary: Option<String>,
}

pub struct SecVm {
    pub label: String,
    /// "yes" | "no" | "unknown"
    pub state: &'static str,
}

/// About / how it works.
#[derive(Template)]
#[template(path = "pages/about.html")]
pub struct AboutPage {
    pub layout: PageLayout,
    pub tr: Translator,
}

// ---------------------------------------------------------------------------
// Authentication and account pages.
// ---------------------------------------------------------------------------

/// A1 — register.
#[derive(Template)]
#[template(path = "pages/register.html")]
pub struct RegisterPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub display_name: String,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to.
    pub field_errors: view::FieldErrors,
}

/// A2 — login.
#[derive(Template)]
#[template(path = "pages/login.html")]
pub struct LoginPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub notice: Option<String>,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to. A
    /// bad login never says which of email/password was wrong, so a failure
    /// flags both rather than disclosing one over the other.
    pub field_errors: view::FieldErrors,
    /// Where to send the user after a successful login — already reduced to a
    /// safe local path (`htmx::safe_local_path`), empty when there is none.
    /// Rendered as a hidden field so the no-JS round trip keeps it.
    pub next: String,
    /// Google sign-in feature flag: when false the link is replaced by a
    /// disabled "coming soon" button (product decision: disabled until a real
    /// OAuth provider exists).
    pub google_enabled: bool,
}

/// A3 — email verified (success or invalid/expired) + resend.
#[derive(Template)]
#[template(path = "pages/verify_email.html")]
pub struct VerifyEmailPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub success: bool,
    pub error: Option<String>,
}

/// A4 — request a password reset.
#[derive(Template)]
#[template(path = "pages/password_reset.html")]
pub struct PasswordResetPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// A5 — set a new password.
#[derive(Template)]
#[template(path = "pages/password_reset_new.html")]
pub struct PasswordResetNewPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub token: String,
    pub error: Option<String>,
}

/// C1 — account overview.
#[derive(Template)]
#[template(path = "pages/account.html")]
pub struct AccountPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub display_name: Option<String>,
    pub public_contribution_name: bool,
    pub is_verified: bool,
    pub roles_label: String,
    pub notice: Option<String>,
}

/// C2 — change password.
#[derive(Template)]
#[template(path = "pages/account_password.html")]
pub struct AccountPasswordPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub error: Option<String>,
    pub notice: Option<String>,
}

/// C3 — change email.
#[derive(Template)]
#[template(path = "pages/account_email.html")]
pub struct AccountEmailPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub error: Option<String>,
    pub notice: Option<String>,
}

/// User management (role assignment).
#[derive(Template)]
#[template(path = "pages/admin_users.html")]
pub struct AdminUsersPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub users: Vec<view::AdminUserVm>,
    /// The current search term, echoed into the search box.
    pub query: String,
    /// Keyset "load more" link, present only when the page was full.
    pub next_url: Option<String>,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// A versioned legal page: current version and effective date.
#[derive(Template)]
#[template(path = "pages/policy.html")]
pub struct PolicyPage {
    pub layout: PageLayout,
    pub tr: Translator,
    /// Stable kind code ("privacy" | "terms" | "cookies").
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub version: String,
    pub effective_label: String,
    /// HTML produced by [`markdown::render_policy_markdown`] from the stored
    /// markdown (raw HTML in the source is escaped there). This is the only
    /// template field marked `|safe`.
    pub content: String,
}

/// The version history for a legal page.
#[derive(Template)]
#[template(path = "pages/policy_versions.html")]
pub struct PolicyVersionsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub items: Vec<view::PolicyVersionVm>,
}

/// Privacy and data hub.
#[derive(Template)]
#[template(path = "pages/account_privacy.html")]
pub struct AccountPrivacyPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub request_types: Vec<view::PrivacyRequestKindVm>,
    pub consent_records: bool,
    pub notice: Option<String>,
}

/// Data export status.
#[derive(Template)]
#[template(path = "pages/account_export.html")]
pub struct AccountExportPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::ExportVm>,
    pub notice: Option<String>,
}

/// Account deletion confirmation.
#[derive(Template)]
#[template(path = "pages/account_delete.html")]
pub struct AccountDeletePage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub error: Option<String>,
}

/// Admin privacy-request queue.
#[derive(Template)]
#[template(path = "pages/admin_privacy_requests.html")]
pub struct AdminPrivacyRequestsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::PrivacyRequestVm>,
    pub notice: Option<String>,
}

// ---------------------------------------------------------------------------
// Community pages.
// ---------------------------------------------------------------------------

/// Add a parking location.
#[derive(Template)]
#[template(path = "pages/parking_new.html")]
pub struct ParkingNewPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub name: String,
    pub address: String,
    pub description: String,
    pub parking_type: String,
    pub cost_kind: String,
    pub price: String,
    pub price_currency: String,
    pub price_unit: String,
    pub lat: String,
    pub lon: String,
    pub timezone: String,
    /// Where the picker centres its map when the form carries no position yet.
    pub default_lat: f64,
    pub default_lon: f64,
    pub hours_days: Vec<ContributionHoursDayVm>,
    pub security_states: Vec<ContributionTriStateVm>,
    pub type_options: Vec<view::OptionVm>,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to.
    pub field_errors: view::FieldErrors,
    pub duplicates: Vec<view::DuplicateVm>,
    /// Set when the add succeeded but similar listings turned up anyway — the
    /// safety net behind the interstitial, not the normal path.
    pub added_id: Option<i64>,
}

/// The duplicate interstitial, rendered *before* anything is created.
#[derive(Template)]
#[template(path = "pages/parking_new_confirm.html")]
pub struct ParkingNewConfirmPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub duplicates: Vec<view::DuplicateVm>,
    /// The whole submission, re-posted verbatim by "create it anyway".
    pub fields: Vec<ContributionHiddenField>,
}

/// Edit a location (reversible fields).
#[derive(Template)]
#[template(path = "pages/parking_edit.html")]
pub struct ParkingEditPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub id: i64,
    pub version: i64,
    pub name: String,
    pub address: String,
    pub description: String,
    pub parking_type: String,
    pub cost_kind: String,
    pub price: String,
    pub price_currency: String,
    pub price_unit: String,
    pub hours_days: Vec<ContributionHoursDayVm>,
    pub security_states: Vec<ContributionTriStateVm>,
    pub type_options: Vec<view::OptionVm>,
    /// The spot's current position. Not editable here — moving a pin is a
    /// reviewed proposal — but it seeds the map on the "move the pin" form.
    pub lat: f64,
    pub lon: f64,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to.
    pub field_errors: view::FieldErrors,
    pub notice: Option<String>,
}

/// Write or edit a review.
#[derive(Template)]
#[template(path = "pages/review_form.html")]
pub struct ReviewFormPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub id: i64,
    pub rating: u8,
    pub body: String,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to.
    pub field_errors: view::FieldErrors,
}

/// C4 — favorites list.
#[derive(Template)]
#[template(path = "pages/favorites.html")]
pub struct FavoritesPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::CardVm>,
    pub notice: Option<String>,
    /// "Load more" link when the page is full (a next keyset page exists).
    pub next_url: Option<String>,
}

/// C5 — contribution history.
#[derive(Template)]
#[template(path = "pages/contributions.html")]
pub struct ContributionsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::ContributionVm>,
    pub next_url: Option<String>,
}

/// HTMX fragment: the favorite button state.
#[derive(Template)]
#[template(path = "partials/favorite_button.html")]
pub struct FavoriteButtonVm {
    pub tr: Translator,
    pub id: i64,
    pub is_favorited: bool,
    pub csrf: String,
}

/// HTMX fragment: a short verification confirmation, or its error state.
#[derive(Template)]
#[template(path = "partials/verification_result.html")]
pub struct VerificationResultVm {
    pub tr: Translator,
    /// `"success"` or `"error"` — picks the confirmation or the alert styling.
    pub state: &'static str,
    pub label: String,
}

/// HTMX fragment: a bare translated error, for endpoints whose success
/// response is a control rather than a toast (the favorite button).
#[derive(Template)]
#[template(path = "partials/fragment_error.html")]
pub struct FragmentErrorVm {
    pub tr: Translator,
    pub message: String,
}

/// Photo moderation queue page.
#[derive(Template)]
#[template(path = "pages/moderation_photos.html")]
pub struct ModerationPhotosPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::ModerationPhotoVm>,
    pub notice: Option<String>,
    pub next_url: Option<String>,
}

/// HTMX fragment for a photo upload result (success or error).
#[derive(Template)]
#[template(path = "partials/photo_upload_result.html")]
pub struct PhotoUploadResultVm {
    pub tr: Translator,
    /// "success" | "error".
    pub state: &'static str,
    pub message: String,
}

// ---------------------------------------------------------------------------
// Moderation and reporting pages.
// ---------------------------------------------------------------------------

/// Moderation dashboard (counts and links to the queues).
#[derive(Template)]
#[template(path = "pages/moderation_dashboard.html")]
pub struct ModerationDashboardPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub pending_photos: i64,
    pub open_reports: i64,
    pub under_review_reports: i64,
    pub pending_proposals: i64,
    pub is_admin: bool,
}

/// Reports queue.
#[derive(Template)]
#[template(path = "pages/moderation_reports.html")]
pub struct ModerationReportsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub state_filter: String,
    pub items: Vec<view::ReportVm>,
    /// The current moderator's id — the template hides resolve/dismiss on one's
    /// own report (the server guard still enforces it).
    pub viewer_id: i64,
    pub notice: Option<String>,
    pub next_url: Option<String>,
}

/// Proposal review queue.
#[derive(Template)]
#[template(path = "pages/moderation_proposals.html")]
pub struct ModerationProposalsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::ProposalVm>,
    pub notice: Option<String>,
    pub next_url: Option<String>,
}

/// Admin audit-log viewer.
#[derive(Template)]
#[template(path = "pages/admin_audit.html")]
pub struct AdminAuditPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::AuditRowVm>,
    pub next_cursor: Option<i64>,
    pub action: String,
    pub target_type: String,
    pub actor: String,
    pub from: String,
    pub to: String,
    pub notice: Option<String>,
}

/// Admin: a target user's contribution history (C5 aggregation scoped to a user).
#[derive(Template)]
#[template(path = "pages/admin_user_contributions.html")]
pub struct AdminUserContributionsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub user_id: i64,
    pub email: String,
    pub items: Vec<view::ContributionVm>,
}

/// HTMX fragment: the report-submit result (success or error).
#[derive(Template)]
#[template(path = "partials/report_result.html")]
pub struct ReportResultVm {
    pub tr: Translator,
    pub state: &'static str,
    pub message: String,
}

/// HTMX fragment: a generic moderation-action toast.
#[derive(Template)]
#[template(path = "partials/moderation_action_result.html")]
pub struct ModerationActionResultVm {
    pub tr: Translator,
    pub state: &'static str,
    pub message: String,
}

pub use wiring::{RouterDeps, app_router, app_router_with};

#[cfg(test)]
mod lib_tests;
