//! Public discovery pages: home, search, parking details, about.

use crate::auth::Auth;
use crate::i18n::Translator;
use crate::{CollaborationProposalVm, CollaborationRevisionVm, profile, routes};
use crate::{PageLayout, view};
use askama::Template;
use bikesnest_application::ParkingDetailsView;
use bikesnest_infrastructure::MapConfig;

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
    fn clear_filters_url(&self) -> String {
        self.form.clear_filters_url()
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
    pub has_unknown_security: bool,
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
    pub error_notice: Option<String>,
    pub search_href: String,
    /// The location's moderation state code (ACTIVE/PENDING_REVIEW/…). Public
    /// viewers only ever reach ACTIVE; moderators see a banner for the rest.
    pub moderation_state: &'static str,
    /// Whether the viewer is a moderator/admin (sees the hidden/invalid banner).
    pub is_moderator: bool,
    /// The report-reason options for the details-page report modal.
    pub reason_options: Vec<view::OptionVm>,
    /// Public-safe persisted proposals; no voter identities or photo keys.
    pub collaboration_proposals: Vec<CollaborationProposalVm>,
    pub collaboration_history: Vec<CollaborationRevisionVm>,
    pub version: i64,
    pub tab: String,
    pub pending_photos: i64,
    pub pending_proposals_total: i64,
    pub proposals_total: i64,
    pub history_total: i64,
    pub reviews_total: i64,
    pub gallery_total: i64,
    pub reviews_next: Option<String>,
    pub reviews_has_more: bool,
    pub proposals_next: Option<String>,
    pub history_next: Option<String>,
    pub collaboration_summary_available: bool,
    pub current_content_available: bool,
    pub gallery_available: bool,
    pub proposals_available: bool,
    pub history_available: bool,
    pub pending_fields: Vec<(String, i64)>,
    pub published_values: Vec<profile::ProfileValueVm>,
    pub viewer_id: Option<bikesnest_domain::UserId>,
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
        let security = loc
            .security()
            .iter()
            .map(|f| SecVm {
                code: f.code().to_string(),
                label: tr.security(f.code()).to_string(),
                state: match f.state() {
                    bikesnest_domain::SecurityState::Yes => "yes",
                    bikesnest_domain::SecurityState::No => "no",
                    bikesnest_domain::SecurityState::Unknown => "unknown",
                },
            })
            .collect::<Vec<_>>();
        let has_unknown_security = security.iter().any(|f| f.state == "unknown");
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
            security,
            has_unknown_security,
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
            // External navigation only — links to providers, coordinates
            // only (no user data is sent; the user leaves the app to navigate).
            osm_url: format!(
                "https://www.openstreetmap.org/?mlat={lat}&mlon={lon}#map=18/{lat}/{lon}"
            ),
            // Google Maps URLs documents `travelmode=bicycling` for the
            // directions action. Coordinates are the only value we send when
            // the rider explicitly follows this external link.
            google_url: format!(
                "https://www.google.com/maps/dir/?api=1&destination={lat},{lon}&travelmode=bicycling"
            ),
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
            can_contribute: auth.user.as_ref().is_some_and(|u| u.is_verified),
            is_authenticated: auth.authenticated(),
            has_own_review: false,
            own_rating: 0,
            reasons: Vec::new(),
            notice: None,
            error_notice: None,
            search_href: "/search".to_string(),
            moderation_state: loc.moderation_state().as_code(),
            is_moderator: auth.user.as_ref().is_some_and(|u| {
                u.has_role(bikesnest_domain::Role::Moderator)
                    || u.has_role(bikesnest_domain::Role::Admin)
            }),
            reason_options: view::report_reason_options(tr),
            collaboration_proposals: Vec::new(),
            collaboration_history: Vec::new(),
            version: loc.version(),
            tab: "current".into(),
            pending_photos: 0,
            pending_proposals_total: 0,
            proposals_total: 0,
            history_total: 0,
            reviews_total: loc.rating().count(),
            gallery_total: 0,
            reviews_next: None,
            reviews_has_more: false,
            proposals_next: None,
            history_next: None,
            collaboration_summary_available: true,
            current_content_available: true,
            gallery_available: true,
            proposals_available: true,
            history_available: true,
            pending_fields: Vec::new(),
            published_values: {
                let mut snapshot = bikesnest_domain::ParkingEdit::from_location(loc).to_json();
                snapshot["point"] = serde_json::json!({"lat":lat,"lon":lon});
                snapshot["timezone"] = serde_json::json!(loc.timezone().name());
                snapshot["moderation_state"] = serde_json::json!(loc.moderation_state().as_code());
                profile::snapshot_values(&snapshot, tr)
            },
            viewer_id: auth.user.as_ref().map(|u| u.id),
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
            let mut media_available = true;
            if let Some(ps) = c.review_photos.get(&r.id) {
                for p in ps {
                    let Some(url) = view::resolve_photo(storage, Some(&p.key)).await else {
                        media_available = false;
                        tracing::warn!(
                            category = "review_media_signing_unavailable",
                            location_id = page.id,
                            review_id = r.id
                        );
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
                        alt: p
                            .alt
                            .clone()
                            .unwrap_or_else(|| tr.t("details.review_photo_alt").to_string()),
                    });
                }
            }
            let mut review = view::review_vm(tr, r, false, photos);
            review.media_available = media_available;
            reviews.push(review);
        }
        page.reviews = reviews;
        page.reviews_has_more = c.reviews_has_more;
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
        page.has_own_review = c.own_review.is_some();
        page.own_rating = c.own_review.map(|r| r.rating.value()).unwrap_or(0);
        page.reasons = c.reasons.iter().map(|r| view::reason_vm(tr, r)).collect();
        page
    }

    /// Set (or overwrite) the page-level notice banner (e.g. "your change will
    /// be reviewed").
    pub fn notice(mut self, notice: Option<String>) -> Self {
        self.notice = notice;
        self
    }
}

pub struct SecVm {
    pub code: String,
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
