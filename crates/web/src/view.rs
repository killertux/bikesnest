//! View models: translate application/domain types into presentation-ready
//! data for Askama templates (labels, formatting, CSS classes).
//!
//! Business rules stay in the application/domain layers; this module only
//! formats, maps and localizes. Every user-facing label goes through the
//! request's [`Translator`].

use crate::PhotoVm;
use crate::i18n::Translator;
use bikesnest_application::{
    AuthenticatedUser, GeoHit, ObjectStorage, ParkingSummary, PendingPhoto,
};
use bikesnest_domain::{
    AccountState, Cost, FreshnessCategory, OpenStatus, OpeningHours, ParkingType, PricingUnit,
    ReportTargetType, Role,
};
use chrono::{Datelike, Timelike};
use std::time::Duration;

/// TTL for presigned photo GET URLs rendered into a page (S3-presign parity).
pub const PHOTO_URL_TTL: Duration = Duration::from_secs(3600);

/// JSON escaped so it can be embedded verbatim in a `<script type="application/json">`
/// (or an HTML attribute) without breaking out — the stored-XSS fix, .
///
/// `serde_json` does not escape `<`, `>`, `&`, U+2028 or U+2029, all of which can
/// terminate a `<script>` block or an attribute. Escaping them to `\uXXXX` keeps
/// `JSON.parse` (or the browser's JSON handling) decoding back the original value,
/// but a literal `</script><img …>` can no longer appear in the output.
pub fn escape_script_json(s: String) -> String {
    s.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Resolve a location's stored photo key to a presigned URL, if present.
pub async fn resolve_photo(storage: &dyn ObjectStorage, key: Option<&str>) -> Option<String> {
    let k = key?;
    storage.presigned_get(k, PHOTO_URL_TTL).await.ok()
}

/// Filterable parking-type codes (labels come from the translator).
pub const TYPE_CODES: &[&str] = &["rack", "parking_facility", "indoor", "secured", "locker"];

/// The security catalog codes (labels come from the translator). Canonical list
/// lives in the domain (`SECURITY_FEATURE_CODES`).
use bikesnest_domain::SECURITY_FEATURE_CODES as SECURITY_CODES;

/// One checkbox/radio option with its label and checked-state, precomputed in
/// Rust (Askama templates stay logic-light).
#[derive(Debug, Clone)]
pub struct OptionVm {
    pub value: &'static str,
    pub label: &'static str,
    pub checked: bool,
}

/// Field-scoped form errors: a small set of
/// `(input name, message)` pairs a handler records when it already knows
/// which input a rejected submission belongs to. Askama calls methods on a
/// struct field directly (as it already does for `tr.t(...)`), so a template
/// asks `{% if let Some(msg) = field_errors.err("email") %}` and renders
/// `aria-invalid` + `aria-describedby` on the matching input without the
/// template itself doing any string matching.
///
/// The page-level `error: Option<String>` banner is unchanged and still
/// covers everything that is not field-specific (rate limits, conflicts,
/// "try again").
#[derive(Debug, Clone, Default)]
pub struct FieldErrors(Vec<(&'static str, String)>);

impl FieldErrors {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// One error for one field — the common case (most handlers reject at
    /// most one input per submission).
    pub fn single(field: &'static str, message: String) -> Self {
        Self(vec![(field, message)])
    }

    /// Record another field error (e.g. the same message under more than one
    /// input, when a validation failure cannot be attributed to just one —
    /// `GeoPoint::new`'s "coordinates out of range" flags both `lat` and `lon`).
    pub fn push(&mut self, field: &'static str, message: String) {
        self.0.push((field, message));
    }

    /// The message recorded for `field`, if the handler that rendered this
    /// page found one.
    pub fn err(&self, field: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, m)| m.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub fn type_options(t: Translator, selected: Option<&str>) -> Vec<OptionVm> {
    let sel = selected.unwrap_or("");
    TYPE_CODES
        .iter()
        .map(|code| OptionVm {
            value: code,
            label: type_label_for_code(t, code),
            checked: sel.split(',').any(|c| c.trim() == *code),
        })
        .collect()
}

pub fn security_options(t: Translator, selected: Option<&str>) -> Vec<OptionVm> {
    let sel = selected.unwrap_or("");
    SECURITY_CODES
        .iter()
        .map(|code| OptionVm {
            value: code,
            label: t.security(code),
            checked: sel.split(',').any(|c| c.trim() == *code),
        })
        .collect()
}

fn type_label_for_code(t: Translator, code: &str) -> &'static str {
    match code {
        "rack" => t.t("type.rack"),
        "parking_facility" => t.t("type.parking_facility"),
        "indoor" => t.t("type.indoor"),
        "secured" => t.t("type.secured"),
        "locker" => t.t("type.locker"),
        _ => t.t("type.other"),
    }
}

pub fn type_label(t: Translator, ty: ParkingType) -> &'static str {
    match ty {
        ParkingType::Rack => t.t("type.rack"),
        ParkingType::ParkingFacility => t.t("type.parking_facility"),
        ParkingType::Indoor => t.t("type.indoor"),
        ParkingType::Secured => t.t("type.secured"),
        ParkingType::Locker => t.t("type.locker"),
        ParkingType::Other => t.t("type.other"),
    }
}

pub fn distance_label(m: f64) -> String {
    if m < 1000.0 {
        format!("{m:.0} m")
    } else {
        format!("{:.1} km", m / 1000.0)
    }
}

fn currency_symbol(code: &str) -> &str {
    match code {
        "BRL" => "R$",
        "EUR" => "€",
        "USD" => "$",
        other => other,
    }
}

pub fn cost_label(t: Translator, cost: &Cost) -> String {
    match cost {
        Cost::Free => t.t("cost.free").to_string(),
        Cost::Unknown => t.t("cost.unknown").to_string(),
        Cost::Paid { price: None } => t.t("cost.paid_unknown").to_string(),
        Cost::Paid { price: Some(money) } => {
            let major = money.cents() as f64 / 100.0;
            let unit = match money.unit() {
                PricingUnit::Hour => t.t("unit.hour"),
                PricingUnit::Day => t.t("unit.day"),
                PricingUnit::Month => t.t("unit.month"),
                PricingUnit::Entry => t.t("unit.entry"),
            };
            format!(
                "{} {} / {unit}",
                currency_symbol(money.currency().as_str()),
                format_money(t, major)
            )
        }
    }
}

/// Locale-aware decimal formatting (.6): pt-BR uses a comma as the decimal
/// separator (`1234,56`), en uses a dot. Thousands separators are not added.
pub fn format_money(t: Translator, value: f64) -> String {
    let s = format!("{value:.2}");
    if t.is_pt() { s.replace('.', ",") } else { s }
}

pub fn rating_label(t: Translator, avg: Option<f64>, count: i64) -> String {
    match avg {
        Some(a) => format!("{a:.1} ({count})"),
        None => t.t("rating.none").to_string(),
    }
}

pub fn freshness_label(t: Translator, f: FreshnessCategory) -> &'static str {
    match f {
        FreshnessCategory::Fresh => t.t("freshness.fresh"),
        FreshnessCategory::RecentlyVerified => t.t("freshness.recently_verified"),
        FreshnessCategory::Aging => t.t("freshness.aging"),
        FreshnessCategory::Stale => t.t("freshness.stale"),
        FreshnessCategory::VeryStale => t.t("freshness.very_stale"),
        FreshnessCategory::Never => t.t("freshness.never"),
    }
}

pub fn open_label(t: Translator, s: OpenStatus) -> &'static str {
    match s {
        OpenStatus::Open => t.t("open.now"),
        OpenStatus::Closed => t.t("open.closed"),
        OpenStatus::Unknown => t.t("open.unknown"),
    }
}

/// One parking card in the results list (P2 search results).
#[derive(Debug, Clone)]
pub struct CardVm {
    pub id: i64,
    /// 1-based position in the results list this card belongs to — the number
    /// on the card's badge and on its map marker. `0` for cards outside a
    /// numbered list (the home page's featured strip, the favorites list).
    pub n: usize,
    pub name: String,
    pub address: String,
    pub type_label: String,
    pub cost_label: String,
    pub distance_label: String,
    pub rating_label: String,
    pub has_rating: bool,
    pub freshness_code: &'static str,
    pub freshness_label: &'static str,
    pub open_label: &'static str,
    pub is_open_now: bool,
    /// Up to 3 confirmed security attribute labels.
    pub security_chips: Vec<String>,
    /// True when the location has no confirmed security attributes.
    pub security_unknown: bool,
    pub url: String,
    pub lat: f64,
    pub lon: f64,
    /// Presigned URL of the location's own primary photo (object storage), when
    /// one exists; the template prefers this over the positional `image`.
    pub photo_url: Option<String>,
    /// Illustrative fallback photo (per type) when a location has no photo yet.
    pub image: &'static str,
    pub image_alt: &'static str,
}

impl CardVm {
    pub fn from_summary(
        t: Translator,
        s: &ParkingSummary,
        freshness: FreshnessCategory,
        photo_url: Option<String>,
    ) -> Self {
        let (image, image_alt) = image_for(s.parking_type);
        let security_chips: Vec<String> = s
            .security_yes
            .iter()
            .take(3)
            .map(|c| t.security(c).to_string())
            .filter(|l| !l.is_empty())
            .collect();
        Self {
            id: s.id,
            // Only a numbered results list knows a card's position; callers
            // that render one set it after the fact (`build_results`).
            n: 0,
            name: s.name.clone(),
            address: s.address.clone(),
            type_label: type_label(t, s.parking_type).to_string(),
            cost_label: cost_label(t, &s.cost),
            distance_label: distance_label(s.distance_m),
            rating_label: rating_label(t, s.rating.avg(), s.rating.count()),
            has_rating: s.rating.avg().is_some(),
            freshness_code: freshness.as_code(),
            freshness_label: freshness_label(t, freshness),
            open_label: open_label(
                t,
                if s.is_open_now {
                    OpenStatus::Open
                } else {
                    OpenStatus::Closed
                },
            ),
            is_open_now: s.is_open_now,
            security_unknown: security_chips.is_empty(),
            security_chips,
            url: format!("/parking/{}", s.id),
            lat: s.point.lat(),
            lon: s.point.lon(),
            photo_url,
            image,
            image_alt,
        }
    }
}

/// Deterministic fallback photo per parking type (photos from the approved
/// design export). Used only when a location has no photo of its own.
fn image_for(ty: ParkingType) -> (&'static str, &'static str) {
    match ty {
        ParkingType::Rack => (
            "/static/img/street-rack-mint-bike.jpg",
            "Mint-green city bicycle locked to a street bike rack",
        ),
        ParkingType::ParkingFacility | ParkingType::Indoor | ParkingType::Secured => (
            "/static/img/square-bike-rows.jpg",
            "Row of bicycles parked under trees in a sunny public square",
        ),
        ParkingType::Locker | ParkingType::Other => (
            "/static/img/mtb-pair-rack.jpg",
            "Two mountain bikes resting on a simple metal bike rack",
        ),
    }
}

/// One marker's data for the search page's `<script type="application/json"
/// id="search-data">` island — only what `web/static/js/search.js`
/// actually reads: `id` (card↔marker sync via `data-parking-id`), `n` (the
/// number drawn in the marker, matching the card's badge), `lat`/`lon`
/// (position), `name`, the two labels the popup shows, and `href` (the popup's
/// "view details" link). Deliberately not the full `CardVm` — that duplicated
/// every card field (image paths, security chips, freshness…) into a ~30 KB
/// JSON blob the map never touches.
///
/// Every string here is written into the popup with `textContent`, never
/// `innerHTML`, so a location's name cannot smuggle markup into the map.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MapItemVm {
    pub id: i64,
    pub n: usize,
    pub lat: f64,
    pub lon: f64,
    pub name: String,
    pub distance_label: String,
    pub cost_label: String,
    pub href: String,
}

impl MapItemVm {
    /// The marker for a card, numbered by its position in the list.
    fn from_card(c: &CardVm, n: usize) -> Self {
        Self {
            id: c.id,
            n,
            lat: c.lat,
            lon: c.lon,
            name: c.name.clone(),
            distance_label: c.distance_label.clone(),
            cost_label: c.cost_label.clone(),
            href: c.url.clone(),
        }
    }

    /// A marker for a browse row the list does not show: the map draws every
    /// location inside the viewport (up to the marker cap), while the list
    /// stops at [`bikesnest_application::BROWSE_LIST_CAP`], so these markers
    /// carry their own labels rather than a card's.
    fn from_summary(t: Translator, s: &bikesnest_application::ParkingSummary, n: usize) -> Self {
        Self {
            id: s.id,
            n,
            lat: s.point.lat(),
            lon: s.point.lon(),
            name: s.name.clone(),
            distance_label: distance_label(s.distance_m),
            cost_label: cost_label(t, &s.cost),
            href: format!("/parking/{}", s.id),
        }
    }
}

/// Shared results payload used by both the full page and the HTMX fragment.
#[derive(Debug, Clone)]
pub struct ResultsData {
    pub destination_label: Option<String>,
    pub total_label: String,
    pub items: Vec<CardVm>,
    pub cursor_url: Option<String>,
    pub error: Option<String>,
    /// Precomputed JSON for the map (Alpine/JS reads this).
    pub map_json: String,
    /// Browse mode (`?bbox=`): the heading names the area rather than a
    /// destination, and the list says distances are from the map's centre.
    pub browse: bool,
    /// Browse mode, area too full to list: how many matched, and the ask to
    /// zoom in. Browse has no next page, so this is what stands in for one.
    pub refine_hint: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub async fn build_results(
    t: Translator,
    page: &bikesnest_application::SearchPage,
    hit: Option<&GeoHit>,
    destination_label: Option<String>,
    query_string: String,
    now: chrono::DateTime<chrono::Utc>,
    thresholds: &bikesnest_domain::FreshnessThresholds,
    storage: &dyn ObjectStorage,
) -> ResultsData {
    let mut items = Vec::with_capacity(page.items.len());
    for (i, s) in page.items.iter().enumerate() {
        let freshness = bikesnest_domain::categorize(s.last_verified_at, now, thresholds);
        let photo_url = resolve_photo(storage, s.photo_key.as_deref()).await;
        let mut card = CardVm::from_summary(t, s, freshness, photo_url);
        // The badge on the card and the number in its marker are the same
        // position, assigned once here so they cannot drift apart.
        card.n = i + 1;
        items.push(card);
    }

    // Trimmed to what search.js reads — see `MapItemVm`.
    let map_items: Vec<MapItemVm> = items.iter().map(|c| MapItemVm::from_card(c, c.n)).collect();
    let map_json = escape_script_json(
        serde_json::json!({
            "origin": hit.map(|h| serde_json::json!({"lat": h.point.lat(), "lon": h.point.lon(), "label": h.label})),
            "items": map_items,
        })
        .to_string(),
    );

    // `query_string` carries no leading "?" — it is the joined parameters, so
    // the cursor is appended to a query string this function opens itself.
    let cursor_url = page.next_cursor.as_ref().map(|c| {
        let cursor = c.encode();
        if query_string.is_empty() {
            format!("/search?cursor={cursor}")
        } else {
            format!("/search?{query_string}&cursor={cursor}")
        }
    });

    ResultsData {
        destination_label,
        total_label: t.spots(page.total),
        items,
        cursor_url,
        error: None,
        map_json,
        browse: false,
        refine_hint: None,
    }
}

/// The `west,south,east,north` box every "browse the map" entry point uses:
/// [`FEATURED_ORIGIN`](bikesnest_infrastructure::FEATURED_ORIGIN) ± the
/// featured half-span. A constant, so the nav link, the home page's explore
/// link and the empty-search prompt all open the same view.
pub fn featured_bbox_param() -> String {
    let (lat, lon) = bikesnest_infrastructure::FEATURED_ORIGIN;
    let d = bikesnest_infrastructure::FEATURED_BBOX_HALF_DEG;
    format!(
        "{:.4},{:.4},{:.4},{:.4}",
        lon - d,
        lat - d,
        lon + d,
        lat + d
    )
}

/// Browse mode's results payload: what is inside the map's own viewport.
///
/// Two caps, one answer. The list is the nearest
/// [`BROWSE_LIST_CAP`](bikesnest_application::BROWSE_LIST_CAP) rows — cards
/// cost a presigned photo URL each, and a list nobody can read is not a
/// result — while the map draws every row the reader returned (up to the
/// marker cap) so panning is honest about what is there. Numbers run across
/// the whole marker set, so marker 7 is card 7 wherever the list stops.
///
/// A viewport past the marker cap comes back as grid counts instead of rows:
/// no cards at all, the counts on the map, and `refine_hint` asking for a
/// smaller area.
pub async fn build_browse_results(
    t: Translator,
    bounds: &bikesnest_application::BoundsQuery,
    page: &bikesnest_application::BoundsPage,
    now: chrono::DateTime<chrono::Utc>,
    thresholds: &bikesnest_domain::FreshnessThresholds,
    storage: &dyn ObjectStorage,
) -> ResultsData {
    let listed = page.items.len().min(bikesnest_application::BROWSE_LIST_CAP);
    let mut items = Vec::with_capacity(listed);
    for (i, s) in page.items.iter().take(listed).enumerate() {
        let freshness = bikesnest_domain::categorize(s.last_verified_at, now, thresholds);
        let photo_url = resolve_photo(storage, s.photo_key.as_deref()).await;
        let mut card = CardVm::from_summary(t, s, freshness, photo_url);
        card.n = i + 1;
        items.push(card);
    }
    let map_items: Vec<MapItemVm> = page
        .items
        .iter()
        .enumerate()
        .map(|(i, s)| MapItemVm::from_summary(t, s, i + 1))
        .collect();
    let clusters: Vec<serde_json::Value> = page
        .clusters
        .iter()
        .map(|c| serde_json::json!({"lat": c.lat, "lon": c.lon, "count": c.count}))
        .collect();
    let map_json = escape_script_json(
        serde_json::json!({
            "origin": null,
            "bbox": [bounds.west, bounds.south, bounds.east, bounds.north],
            "items": map_items,
            "clusters": clusters,
            "total": page.total,
        })
        .to_string(),
    );
    let refine_hint = (page.total > items.len()).then(|| t.t("search.browse.refine").to_string());
    ResultsData {
        destination_label: None,
        total_label: t.spots(page.total as i64),
        items,
        // Browse is not paginated: the answer is a viewport, and the next one
        // is a pan, not a cursor.
        cursor_url: None,
        error: None,
        map_json,
        browse: true,
        refine_hint,
    }
}

/// One row of the weekly hours table on the details page.
pub struct HoursRowVm {
    pub day: &'static str,
    pub label: String,
    pub is_today: bool,
}

pub fn hours_rows(
    t: Translator,
    hours: &OpeningHours,
    tz: chrono_tz::Tz,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<HoursRowVm> {
    const DAY_KEYS: [&str; 7] = [
        "day.mon", "day.tue", "day.wed", "day.thu", "day.fri", "day.sat", "day.sun",
    ];
    let today = now.with_timezone(&tz).weekday().number_from_monday() as usize; // 1..=7
    let rows = hours.rows_by_day();
    (1..=7)
        .map(|d| {
            let ranges = &rows[d - 1];
            let label = if hours.is_unknown() {
                t.t("hours.unknown").to_string()
            } else if ranges.is_empty() {
                t.t("hours.closed").to_string()
            } else if ranges.iter().any(|r| r.all_day) {
                t.t("hours.all_day").to_string()
            } else {
                ranges
                    .iter()
                    .map(|r| {
                        format!(
                            "{:02}:{:02} – {:02}:{:02}",
                            r.opens_at.hour(),
                            r.opens_at.minute(),
                            r.closes_at.hour(),
                            r.closes_at.minute()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            HoursRowVm {
                day: t.t(DAY_KEYS[d - 1]),
                label,
                is_today: d == today,
            }
        })
        .collect()
}

/// Localized role label.
pub fn role_label(t: Translator, role: Role) -> &'static str {
    match role {
        Role::User => t.t("role.user"),
        Role::Moderator => t.t("role.moderator"),
        Role::Admin => t.t("role.admin"),
    }
}

/// Localized account-state label (C1 / ).
pub fn account_state_label(t: Translator, s: AccountState) -> &'static str {
    match s {
        AccountState::PendingEmailVerification => t.t("account.state.pending"),
        AccountState::Active => t.t("account.state.active"),
        AccountState::Suspended => t.t("account.state.suspended"),
        AccountState::Deleted => t.t("account.state.deleted"),
    }
}

/// One row of the admin user-management table.
///
/// The email is **masked** by default: an admin managing roles does not need
/// every address on screen (and neither does anyone glancing at the screen).
/// The full value is one click away in the same row.
#[derive(Debug, Clone)]
pub struct AdminUserVm {
    pub id: i64,
    pub email: String,
    /// `c***@brick.so` — what the row shows until the admin reveals it.
    pub email_masked: String,
    pub display_name: String,
    pub roles_label: String,
    pub state_label: &'static str,
    /// Account-state code (ACTIVE/SUSPENDED/…) for conditional rendering.
    pub state: &'static str,
    pub is_verified: bool,
    pub has_moderator: bool,
    pub has_admin: bool,
    /// Last session activity, absolute; empty for an account that never
    /// signed in.
    pub last_active_label: String,
    pub last_active_title: String,
    pub contributions: i64,
    /// `hx-confirm` copy naming this user, per destructive action.
    pub confirm_suspend: String,
    pub confirm_restore: String,
    pub confirm_grant_moderator: String,
    pub confirm_revoke_moderator: String,
    pub confirm_grant_admin: String,
    pub confirm_revoke_admin: String,
}

/// Mask an email to its first character plus its domain: `c***@brick.so`.
/// A one-character local part still masks (`c***@…`), so the length of the
/// hidden part is never inferable from the mask.
pub fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "***".to_string();
    };
    match local.chars().next() {
        Some(first) => format!("{first}***@{domain}"),
        None => format!("***@{domain}"),
    }
}

// ---------------------------------------------------------------------------
// Community view models
// ---------------------------------------------------------------------------

/// One rendered review (D3 / P3). `photos` are the review's APPROVED photos
/// already resolved to presigned URLs for the card's thumbnails.
#[derive(Debug, Clone)]
pub struct ReviewVm {
    pub id: i64,
    pub rating: u8,
    pub stars: String,
    pub body: String,
    pub created_label: String,
    pub author_label: String,
    pub is_own: bool,
    pub photos: Vec<PhotoVm>,
}

pub fn review_vm(
    t: Translator,
    r: &bikesnest_application::Review,
    is_own: bool,
    photos: Vec<PhotoVm>,
) -> ReviewVm {
    let stars = "★".repeat(r.rating.value() as usize);
    let created_label = time_ago_label(t, r.created_at);
    ReviewVm {
        id: r.id,
        rating: r.rating.value(),
        stars,
        body: r.body.as_str().to_string(),
        created_label,
        author_label: r
            .public_author_name
            .clone()
            .unwrap_or_else(|| t.t("collab.anonymous").to_string()),
        is_own,
        photos,
    }
}

fn time_ago_label(t: Translator, at: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let days = (now - at).num_days();
    if days == 0 {
        t.t("time.today").to_string()
    } else if days == 1 {
        t.t("time.yesterday").to_string()
    } else if days < 30 {
        t.t("time.days_ago").replace("{n}", &days.to_string())
    } else {
        let months = days / 30;
        t.t("time.months_ago").replace("{n}", &months.to_string())
    }
}

/// Build the admin user list as presentation-ready rows. `activity` is the
/// batched last-seen/contribution lookup for exactly these ids.
pub fn admin_users(
    t: Translator,
    users: &[AuthenticatedUser],
    activity: &std::collections::HashMap<i64, bikesnest_application::UserActivity>,
) -> Vec<AdminUserVm> {
    users
        .iter()
        .map(|u| {
            let mut roles = u.roles.clone();
            roles.sort();
            roles.dedup();
            let email = u.email.to_string();
            let name = u.display_name.clone().unwrap_or_default();
            // Whichever of the two the admin will recognize, for the confirm copy.
            let who = if name.trim().is_empty() {
                mask_email(&email)
            } else {
                name.clone()
            };
            let confirm = |key: &str| t.t(key).replace("{name}", &who);
            let act = activity.get(&u.id.0).copied().unwrap_or_default();
            AdminUserVm {
                id: u.id.0,
                email_masked: mask_email(&email),
                email,
                display_name: name,
                roles_label: roles
                    .iter()
                    .map(|r| role_label(t, *r))
                    .collect::<Vec<_>>()
                    .join(", "),
                state_label: account_state_label(t, u.account_state),
                state: u.account_state.as_code(),
                is_verified: u.is_verified,
                has_moderator: u.has_role(Role::Moderator),
                has_admin: u.has_role(Role::Admin),
                last_active_label: act
                    .last_active_at
                    .map(|at| iso_datetime_label(t, at))
                    .unwrap_or_else(|| t.t("admin.never").to_string()),
                last_active_title: act
                    .last_active_at
                    .map(|at| time_ago_label(t, at))
                    .unwrap_or_default(),
                contributions: act.contributions,
                confirm_suspend: confirm("admin.confirm.suspend"),
                confirm_restore: confirm("admin.confirm.restore"),
                confirm_grant_moderator: confirm("admin.confirm.grant_moderator"),
                confirm_revoke_moderator: confirm("admin.confirm.revoke_moderator"),
                confirm_grant_admin: confirm("admin.confirm.grant_admin"),
                confirm_revoke_admin: confirm("admin.confirm.revoke_admin"),
            }
        })
        .collect()
}

/// Localized confidence label for the confidence badge.
pub fn confidence_label(t: Translator, c: bikesnest_domain::Confidence) -> &'static str {
    match c {
        bikesnest_domain::Confidence::Reported => t.t("confidence.reported"),
        bikesnest_domain::Confidence::Verified => t.t("confidence.verified"),
        bikesnest_domain::Confidence::RecentlyVerified => t.t("confidence.recently_verified"),
        bikesnest_domain::Confidence::Stale => t.t("confidence.stale"),
        bikesnest_domain::Confidence::Conflicting => t.t("confidence.conflicting"),
    }
}

/// One rendered confidence badge.
#[derive(Debug, Clone)]
pub struct ConfidenceVm {
    pub code: &'static str,
    pub label: &'static str,
}

pub fn confidence_vm(t: Translator, c: bikesnest_domain::Confidence) -> ConfidenceVm {
    ConfidenceVm {
        code: c.as_code(),
        label: confidence_label(t, c),
    }
}

/// One rendered attribution dispute tally.
#[derive(Debug, Clone)]
pub struct AttrDisputeVm {
    pub label: &'static str,
    pub incorrect: i64,
}

pub fn attr_dispute_vm(t: Translator, code: &str, incorrect: i64) -> AttrDisputeVm {
    AttrDisputeVm {
        label: attribute_label(t, code),
        incorrect,
    }
}

fn attribute_label(t: Translator, code: &str) -> &'static str {
    match code {
        "name" => t.t("attr.name"),
        "address" => t.t("attr.address"),
        "type" => t.t("attr.type"),
        "cost" => t.t("attr.cost"),
        "hours" => t.t("attr.hours"),
        "security" => t.t("attr.security"),
        "location" => t.t("attr.location"),
        _ => t.t("attr.unknown"),
    }
}

/// One rendered "recommended because…" reason.
#[derive(Debug, Clone)]
pub struct ReasonVm {
    pub label: &'static str,
    pub detail: String,
}

pub fn reason_vm(t: Translator, r: &bikesnest_application::Reason) -> ReasonVm {
    ReasonVm {
        label: t.t(r.label_key),
        detail: r.detail.clone(),
    }
}

/// One rendered row of the contribution history contribution history feed.
#[derive(Debug, Clone)]
pub struct ContributionVm {
    /// Stable code for the row's icon: "added" | "edited" | "proposed" |
    /// "reviewed" | "verified" | "parked_here" | "favorited" | "photo_pending"
    /// | "other". Kept separate from `kind_label` (which is localized) so the
    /// template can pick an icon without matching on translated text.
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub target: String,
    pub state_label: &'static str,
    pub at_label: String,
}

/// One advisory duplicate candidate (D1/).
#[derive(Debug, Clone)]
pub struct DuplicateVm {
    pub id: i64,
    pub name: String,
    pub distance_label: String,
    pub similarity_label: String,
}

pub fn duplicate_vm(_t: Translator, d: &bikesnest_application::DuplicateCandidate) -> DuplicateVm {
    DuplicateVm {
        id: d.id,
        name: d.name.clone(),
        distance_label: distance_label(d.distance_m),
        similarity_label: format!("{:.0}%", d.similarity * 100.0),
    }
}

/// One photo in the moderator queue. Includes the presigned URL of
/// the *processed derivative* (exactly what would publish), a small preview and
/// an anonymized "Contributor #id" label — never an email/OAuth subject.
#[derive(Debug, Clone)]
pub struct ModerationPhotoVm {
    pub id: i64,
    pub kind: &'static str,
    pub location_id: i64,
    pub location_name: String,
    pub full_url: String,
    pub thumb_url: Option<String>,
    pub alt: String,
    pub dimensions: Option<String>,
    pub contributor_label: String,
    pub uploaded_label: String,
    /// The object is actually in storage. A presigned URL is issued whether or
    /// not the object exists, so without this check a missing file renders as a
    /// broken image and a moderator can approve a photo nobody can see. CSP
    /// forbids an `onerror` fallback, so the check happens server-side.
    pub available: bool,
    /// Pre-filled rejection reason for an unavailable image, so the moderator
    /// clears the queue in one click instead of inventing wording.
    pub missing_reason: &'static str,
}

pub async fn moderation_photo_vm(
    t: Translator,
    storage: &dyn ObjectStorage,
    p: &PendingPhoto,
) -> ModerationPhotoVm {
    // One HEAD per pending photo. The queue is a bounded page (≤50), and the
    // alternative is a moderator judging a broken image.
    let available = storage.exists(&p.storage_key).await.unwrap_or(false);
    let full_url = resolve_photo(storage, Some(&p.storage_key))
        .await
        .unwrap_or_default();
    let thumb_url = match p.thumbnail_key.as_deref().filter(|_| available) {
        Some(k) => resolve_photo(storage, Some(k)).await,
        None => None,
    };
    let alt = p
        .alt
        .clone()
        .unwrap_or_else(|| format!("Photo of {}", p.parent_name));
    let dimensions = match (p.width, p.height) {
        (Some(w), Some(h)) => Some(format!("{w} × {h}")),
        _ => None,
    };
    let contributor_label = p
        .uploader_id
        .map(|uid| format!("{} #{}", t.t("moderation.contributor"), uid.0))
        .unwrap_or_default();
    ModerationPhotoVm {
        id: p.id,
        kind: match p.kind {
            bikesnest_application::PhotoKind::Parking => "parking",
            bikesnest_application::PhotoKind::Review => "review",
        },
        location_id: p.parent_id,
        location_name: p.parent_name.clone(),
        full_url,
        thumb_url,
        alt,
        dimensions,
        contributor_label,
        uploaded_label: time_ago_label(t, p.created_at),
        available,
        missing_reason: t.t("moderation.photo_missing_reason"),
    }
}

// ---------------------------------------------------------------------------
// Moderation & reporting view models
// ---------------------------------------------------------------------------

/// The one "act on the reported content" button a queue row offers, when the
/// content is still in a state where acting is possible.
///
/// `url` always points at an endpoint that already existed — this adds a way
/// to reach the moderation actions from the queue, not new actions.
#[derive(Debug, Clone)]
pub struct ReportActionVm {
    pub url: String,
    pub label: &'static str,
    /// `hx-confirm` copy — every one of these hides or invalidates something.
    pub confirm: String,
    /// The reject-photo endpoint requires a reason; the others take none.
    pub needs_reason: bool,
}

/// One row of the reports queue.
#[derive(Debug, Clone)]
pub struct ReportVm {
    pub id: i64,
    /// The submitting user's id (moderators may compare against the viewer to
    /// hide resolve/dismiss on one's own report); never rendered on public pages.
    /// `None` once the reporter's account is anonymized.
    pub reporter_id: Option<i64>,
    pub target_type_label: &'static str,
    pub target_id: i64,
    /// Where the reported content actually is. Empty when the target has been
    /// deleted since the report was filed.
    pub target_url: String,
    /// The location's name (what the moderator recognizes), falling back to
    /// "<type> #<id>" for a target that no longer resolves.
    pub target_label: String,
    pub target_address: String,
    /// A review excerpt, or the reported photo's description — whatever tells
    /// the moderator what they are judging without opening the target.
    pub preview: Option<String>,
    /// The reported photo (or the review's photo), if any.
    pub thumb_url: Option<String>,
    pub reason_label: &'static str,
    pub description: String,
    pub state_code: &'static str,
    pub state_label: &'static str,
    /// The badge's complete Tailwind class list. Built here rather than
    /// interpolated in the template (`bg-{{ color }}` never survives Tailwind's
    /// content scan, F-L7).
    pub state_badge_class: &'static str,
    pub reporter_label: String,
    pub claimed_by_label: String,
    /// Absolute filed-at, with the relative phrase as the `title`.
    pub created_label: String,
    pub created_title: String,
    pub action: Option<ReportActionVm>,
}

/// The report-reason option list (value = code, label = i18n) for the modal/select.
use bikesnest_domain::REPORT_REASONS;

pub fn report_reason_options(t: Translator) -> Vec<OptionVm> {
    REPORT_REASONS
        .iter()
        .map(|code| OptionVm {
            value: code,
            label: report_reason_label(t, code),
            checked: false,
        })
        .collect()
}

fn report_target_label(t: Translator, code: &str) -> &'static str {
    match code {
        "parking" => t.t("report.target.parking"),
        "parking_photo" => t.t("report.target.parking_photo"),
        "review" => t.t("report.target.review"),
        "review_photo" => t.t("report.target.review_photo"),
        _ => t.t("report.target.other"),
    }
}

fn report_reason_label(t: Translator, reason: &str) -> &'static str {
    match reason {
        "nonexistent_parking" => t.t("report.reason.nonexistent_parking"),
        "incorrect_location" => t.t("report.reason.incorrect_location"),
        "incorrect_price" => t.t("report.reason.incorrect_price"),
        "incorrect_hours" => t.t("report.reason.incorrect_hours"),
        "incorrect_security" => t.t("report.reason.incorrect_security"),
        "duplicate" => t.t("report.reason.duplicate"),
        "inappropriate_photo" => t.t("report.reason.inappropriate_photo"),
        "inappropriate_review" => t.t("report.reason.inappropriate_review"),
        "spam" => t.t("report.reason.spam"),
        "abuse" => t.t("report.reason.abuse"),
        "other" => t.t("report.reason.other"),
        _ => t.t("report.reason.other"),
    }
}

fn report_state_label(t: Translator, s: bikesnest_domain::ReportState) -> &'static str {
    match s {
        bikesnest_domain::ReportState::Open => t.t("report.state.open"),
        bikesnest_domain::ReportState::UnderReview => t.t("report.state.under_review"),
        bikesnest_domain::ReportState::Resolved => t.t("report.state.resolved"),
        bikesnest_domain::ReportState::Dismissed => t.t("report.state.dismissed"),
    }
}

/// The complete badge classes per report state. A `bg-{{ color }}` built in the
/// template would never reach Tailwind's content scanner, so the class list is
/// spelled out here (F-L7).
fn report_state_badge_class(s: bikesnest_domain::ReportState) -> &'static str {
    match s {
        bikesnest_domain::ReportState::Open => {
            "rounded-full bg-danger/10 px-2 py-0.5 font-medium text-danger"
        }
        bikesnest_domain::ReportState::UnderReview => {
            "rounded-full bg-aging/10 px-2 py-0.5 font-medium text-aging"
        }
        bikesnest_domain::ReportState::Resolved | bikesnest_domain::ReportState::Dismissed => {
            "rounded-full bg-fresh/10 px-2 py-0.5 font-medium text-fresh"
        }
    }
}

/// Deep-link to the reported content: the location page, or the location page
/// anchored at the specific review.
fn report_target_url(
    r: &bikesnest_application::Report,
    preview: Option<&bikesnest_application::ReportTargetPreview>,
) -> String {
    let Some(location_id) = preview.and_then(|p| p.location_id) else {
        return String::new();
    };
    match r.target_type {
        ReportTargetType::Review | ReportTargetType::ReviewPhoto => {
            match preview.and_then(|p| p.review_id) {
                Some(review_id) => format!("/parking/{location_id}#review-{review_id}"),
                None => format!("/parking/{location_id}"),
            }
        }
        _ => format!("/parking/{location_id}"),
    }
}

/// Which moderation action is still open on this target, if any. Returns
/// `None` once the content is already hidden/invalidated/rejected — offering
/// "hide" on a hidden review would just fail with `InvalidState`.
fn report_action(
    t: Translator,
    r: &bikesnest_application::Report,
    preview: Option<&bikesnest_application::ReportTargetPreview>,
    target_label: &str,
) -> Option<ReportActionVm> {
    let state = preview?.target_state.as_deref()?;
    let id = r.target_id;
    let (url, label, needs_reason) = match (r.target_type, state) {
        (ReportTargetType::Parking, "ACTIVE") => (
            format!("/moderation/parking/{id}/invalidate"),
            t.t("moderation.action.invalidate_parking"),
            false,
        ),
        (ReportTargetType::Review, "ACTIVE") => (
            format!("/moderation/reviews/{id}/hide"),
            t.t("moderation.action.hide_review"),
            false,
        ),
        (ReportTargetType::ParkingPhoto | ReportTargetType::ReviewPhoto, "APPROVED") => (
            format!(
                "/moderation/photos/{}/{id}/hide",
                photo_kind_code(r.target_type)
            ),
            t.t("moderation.action.hide_photo"),
            false,
        ),
        // Still in the upload queue: rejecting it is the terminal action, and
        // that endpoint wants a reason.
        (ReportTargetType::ParkingPhoto | ReportTargetType::ReviewPhoto, "PENDING_REVIEW") => (
            format!(
                "/moderation/photos/{}/{id}/reject",
                photo_kind_code(r.target_type)
            ),
            t.t("moderation.action.reject_photo"),
            true,
        ),
        _ => return None,
    };
    Some(ReportActionVm {
        confirm: t
            .t("moderation.confirm.act_on_content")
            .replace("{label}", label)
            .replace("{name}", target_label),
        url,
        label,
        needs_reason,
    })
}

/// The `{kind}` path segment the photo moderation endpoints expect.
fn photo_kind_code(target_type: ReportTargetType) -> &'static str {
    match target_type {
        ReportTargetType::ReviewPhoto => "review",
        _ => "parking",
    }
}

/// Build a queue row. `preview` is the batched lookup's entry for this
/// report's target (absent when the target has since been deleted), and
/// `thumb_url` a resolved presigned URL for its photo.
pub fn report_vm(
    t: Translator,
    r: &bikesnest_application::Report,
    preview: Option<&bikesnest_application::ReportTargetPreview>,
    thumb_url: Option<String>,
) -> ReportVm {
    let target_type_label = report_target_label(t, r.target_type.as_code());
    let target_label = preview
        .and_then(|p| p.location_name.clone())
        .unwrap_or_else(|| format!("{} #{}", target_type_label, r.target_id));
    let preview_text = match r.target_type {
        ReportTargetType::Review | ReportTargetType::ReviewPhoto => {
            preview.and_then(|p| p.review_excerpt.clone())
        }
        _ => None,
    }
    .filter(|s| !s.trim().is_empty());
    ReportVm {
        id: r.id,
        reporter_id: r.reporter_id.map(|u| u.0),
        target_type_label,
        target_id: r.target_id,
        target_url: report_target_url(r, preview),
        target_address: preview
            .and_then(|p| p.location_address.clone())
            .unwrap_or_default(),
        preview: preview_text,
        thumb_url,
        reason_label: report_reason_label(t, &r.reason),
        description: r.description.clone().unwrap_or_default(),
        state_code: r.state.as_code(),
        state_label: report_state_label(t, r.state),
        state_badge_class: report_state_badge_class(r.state),
        reporter_label: r
            .reporter_id
            .map(|u| format!("{} #{}", t.t("moderation.contributor"), u.0))
            .unwrap_or_else(|| t.t("report.reporter.anonymous").to_string()),
        claimed_by_label: r
            .claimed_by
            .map(|c| format!("{} #{}", t.t("moderation.moderator"), c.0))
            .unwrap_or_else(|| t.t("report.claimed.none").to_string()),
        created_label: iso_datetime_label(t, r.created_at),
        created_title: time_ago_label(t, r.created_at),
        action: report_action(t, r, preview, &target_label),
        target_label,
    }
}

/// One "current → proposed" pair the moderator has to judge.
#[derive(Debug, Clone)]
pub struct ProposalDiffVm {
    pub label: String,
    pub current: String,
    pub proposed: String,
    /// `false` when the proposal would leave this field as it is — the row is
    /// still shown (context), just not highlighted.
    pub changed: bool,
}

/// The two points a move proposal's mini-map shows. Rendered as `data-*`
/// attributes for `details-map.js`, which is why they are pre-formatted
/// strings rather than floats.
#[derive(Debug, Clone)]
pub struct ProposalMapVm {
    pub current_lat: String,
    pub current_lon: String,
    pub proposed_lat: String,
    pub proposed_lon: String,
}

/// One row of the proposal review queue.
///
/// Everything the moderator needs to decide without leaving the page: where the
/// spot is, who asked, why, what would change, and — for a move — the approve
/// form already filled in with the proposed values (editing them is the
/// "modify" path; the merge rule lives in the application layer).
#[derive(Debug, Clone)]
pub struct ProposalVm {
    pub id: i64,
    pub location_id: i64,
    pub location_name: String,
    pub location_address: String,
    /// Stable kind code ("move_location" | "change_existence") — templates must
    /// branch on this, never on the localized label.
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub proposer_label: String,
    /// The proposer's note, when they left one.
    pub reason: Option<String>,
    pub base_version: i64,
    /// Absolute submitted-at ("2026-09-04 14:02"), with the relative phrase
    /// ("yesterday") as the `title`.
    pub created_label: String,
    pub created_title: String,
    pub diff: Vec<ProposalDiffVm>,
    pub map: Option<ProposalMapVm>,
    /// The location moved on since this was written: approving it will be
    /// refused by the repository, so the queue says so up front.
    pub is_stale: bool,
    /// The stored payload could not be read; approving requires the moderator
    /// to supply every value.
    pub needs_manual_review: bool,
    /// Pre-filled approve-form values (empty only when there is nothing to
    /// pre-fill, i.e. a payload that needs manual review).
    pub form_lat: String,
    pub form_lon: String,
    pub form_timezone: String,
    /// The `existence` option the form preselects: "exists" | "removed" | "".
    pub form_existence: &'static str,
    /// `hx-confirm` copy for an approval that takes a spot off the map.
    /// `None` for every other approval.
    pub confirm: Option<String>,
}

fn proposal_kind_label(t: Translator, kind: bikesnest_domain::ProposalKind) -> &'static str {
    match kind {
        bikesnest_domain::ProposalKind::EditDetails => t.t("profile.edit_details"),
        bikesnest_domain::ProposalKind::MoveLocation => t.t("proposal.kind.move"),
        bikesnest_domain::ProposalKind::ChangeExistence => t.t("proposal.kind.existence"),
    }
}

fn existence_label(t: Translator, exists: bool) -> &'static str {
    if exists {
        t.t("proposal.existence.exists")
    } else {
        t.t("proposal.existence.removed")
    }
}

/// Coordinates at the precision the approve form round-trips (≈1 m), so
/// re-submitting an untouched form is a no-op rather than a tiny move.
fn coord(v: f64) -> String {
    format!("{v:.6}")
}

fn coord_pair(lat: Option<f64>, lon: Option<f64>, t: Translator) -> String {
    match (lat, lon) {
        (Some(lat), Some(lon)) => format!("{}, {}", coord(lat), coord(lon)),
        _ => t.t("proposal.value.unknown").to_string(),
    }
}

pub fn proposal_vm(t: Translator, p: &bikesnest_application::Proposal) -> ProposalVm {
    use bikesnest_domain::ProposedChange;

    let mut diff = Vec::new();
    let mut map = None;
    let mut form_lat = String::new();
    let mut form_lon = String::new();
    let mut form_timezone = String::new();
    let mut form_existence = "";
    let mut confirm = None;

    match &p.change {
        ProposedChange::EditDetails(edit) => {
            let current = crate::profile::snapshot_values(&p.current_snapshot, t);
            for field in crate::profile::edit_values(edit, t) {
                let before = current
                    .iter()
                    .find(|f| f.key == field.key)
                    .map(|f| f.value.clone())
                    .unwrap_or_else(|| t.t("proposal.value.unknown").into());
                if before != field.value {
                    diff.push(ProposalDiffVm {
                        label: field.label,
                        current: before,
                        proposed: field.value,
                        changed: true,
                    });
                }
            }
        }
        ProposedChange::MoveLocation { lat, lon, timezone } => {
            let changed = p.current_lat != Some(*lat) || p.current_lon != Some(*lon);
            diff.push(ProposalDiffVm {
                label: t.t("proposal.field.coordinates").into(),
                current: coord_pair(p.current_lat, p.current_lon, t),
                proposed: format!("{}, {}", coord(*lat), coord(*lon)),
                changed,
            });
            let proposed_tz = timezone
                .clone()
                .unwrap_or_else(|| p.current_timezone.clone());
            diff.push(ProposalDiffVm {
                label: t.t("proposal.field.timezone").into(),
                current: p.current_timezone.clone(),
                proposed: proposed_tz.clone(),
                changed: proposed_tz != p.current_timezone,
            });
            // Two markers only when there is a "before" to compare against; a
            // location with no coordinates yet gets the single proposed pin.
            map = Some(ProposalMapVm {
                current_lat: p.current_lat.map(coord).unwrap_or_default(),
                current_lon: p.current_lon.map(coord).unwrap_or_default(),
                proposed_lat: coord(*lat),
                proposed_lon: coord(*lon),
            });
            form_lat = coord(*lat);
            form_lon = coord(*lon);
            form_timezone = proposed_tz;
        }
        ProposedChange::ChangeExistence { exists } => {
            let currently_exists = p.current_state == bikesnest_domain::ModerationState::Active;
            diff.push(ProposalDiffVm {
                label: t.t("proposal.field.existence").into(),
                current: existence_label(t, currently_exists).to_string(),
                proposed: existence_label(t, *exists).to_string(),
                changed: currently_exists != *exists,
            });
            form_existence = if *exists {
                bikesnest_domain::ProposedChange::EXISTS
            } else {
                bikesnest_domain::ProposedChange::REMOVED
            };
            // Taking a spot off the map is the one approval that is hard to
            // notice and annoying to undo, so it asks first.
            if !*exists {
                confirm = Some(
                    t.t("moderation.confirm.remove")
                        .replace("{name}", &p.location_name),
                );
            }
        }
        ProposedChange::Unknown => {}
    }

    ProposalVm {
        id: p.id,
        location_id: p.location_id,
        location_name: p.location_name.clone(),
        location_address: p.location_address.clone(),
        kind_code: p.kind.as_code(),
        kind_label: proposal_kind_label(t, p.kind),
        proposer_label: p
            .proposer_id
            .map(|u| format!("{} #{}", t.t("moderation.proposer"), u.0))
            .unwrap_or_else(|| t.t("report.reporter.anonymous").to_string()),
        reason: p.reason.clone(),
        base_version: p.base_version,
        created_label: iso_datetime_label(t, p.created_at),
        created_title: time_ago_label(t, p.created_at),
        diff,
        map,
        is_stale: p.is_stale(),
        needs_manual_review: p.change == ProposedChange::Unknown,
        form_lat,
        form_lon,
        form_timezone,
        form_existence,
        confirm,
    }
}

/// One row of the admin audit-log viewer. Metadata rendered as an escaped
/// JSON blob — by construction it carries no secrets/PII.
///
/// An audit log is read to answer "who did what, exactly when" — so the
/// timestamp is absolute (a relative "yesterday" cannot be correlated with
/// anything) and the actor is a name, with the raw id kept for reference.
#[derive(Debug, Clone)]
pub struct AuditRowVm {
    pub id: i64,
    pub actor_label: String,
    /// Link to the actor's account row, when there is an actor.
    pub actor_url: Option<String>,
    pub action: String,
    pub target_label: String,
    /// Link to the target the event names, for the types that have a page.
    pub target_url: Option<String>,
    pub result_label: &'static str,
    pub metadata: String,
    /// Exact UTC instant, unambiguous and sortable.
    pub created_utc: String,
    /// The same instant in the reading operator's locale format.
    pub created_local: String,
    /// The relative phrase, kept as a `title` for a quick sense of recency.
    pub created_title: String,
}

/// Where an audit target can be inspected. Types with no page (a session, a
/// token) get no link rather than a dead one.
fn audit_target_url(target_type: &str, target_id: &str) -> Option<String> {
    if target_id.trim().is_empty() {
        return None;
    }
    // Only ever numeric ids reach a URL — anything else is a token/opaque
    // handle and must not be pasted into a path.
    let numeric = target_id.parse::<i64>().ok();
    match target_type {
        "parking_location" => numeric.map(|id| format!("/parking/{id}")),
        "user" => numeric.map(|id| format!("/admin/users?q={id}")),
        "report" => Some("/moderation/reports".to_string()),
        "parking_proposal" => Some("/moderation/proposals".to_string()),
        "parking_photo" | "review_photo" => Some("/moderation/photos".to_string()),
        "privacy_request" => Some("/admin/privacy-requests".to_string()),
        _ => None,
    }
}

/// `labels` is the batched `AccountRepository::labels_for` result for every
/// actor on the page; an id absent from it (a deleted account) keeps the
/// "#id" form so the trail stays readable.
pub fn audit_row_vm(
    t: Translator,
    e: &bikesnest_application::AuditStoredEvent,
    labels: &std::collections::HashMap<i64, String>,
) -> AuditRowVm {
    let actor = e.event.actor_user_id.map(|a| a.0);
    let actor_label = match actor {
        Some(id) => match labels.get(&id) {
            Some(label) => format!("{label} (#{id})"),
            None => format!("{} #{}", t.t("moderation.actor"), id),
        },
        None => t.t("audit.system").to_string(),
    };
    let result_label = if e.event.result == "success" {
        t.t("audit.result.success")
    } else {
        t.t("audit.result.failure")
    };
    AuditRowVm {
        id: e.id,
        actor_label,
        actor_url: actor.map(|id| format!("/admin/users?q={id}")),
        action: e.event.action.clone(),
        target_label: format!("{}:{}", e.event.target_type, e.event.target_id),
        target_url: audit_target_url(&e.event.target_type, &e.event.target_id),
        result_label,
        metadata: e.event.metadata.to_string(),
        created_utc: utc_datetime_label(e.created_at),
        created_local: iso_datetime_label(t, e.created_at),
        created_title: time_ago_label(t, e.created_at),
    }
}

/// An exact UTC instant, seconds included and the zone spelled out — the form
/// an operator can paste into a ticket or correlate with a log line.
pub fn utc_datetime_label(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// The value an `<input type="datetime-local">` expects (no zone, minutes).
pub fn datetime_local_value(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M").to_string()
}

// ---------------------------------------------------------------------------
// Privacy & account lifecycle
// ---------------------------------------------------------------------------

/// A locale-neutral ISO date-time label (YYYY-MM-DD HH:MM).
pub fn iso_datetime_label(t: Translator, dt: chrono::DateTime<chrono::Utc>) -> String {
    if t.is_pt() {
        dt.format("%d/%m/%Y %H:%M").to_string()
    } else {
        dt.format("%Y-%m-%d %H:%M").to_string()
    }
}

/// One row of the export export-status page (status + optional single-use link).
#[derive(Debug, Clone)]
pub struct ExportVm {
    pub id: i64,
    pub state_code: &'static str,
    pub state_label: &'static str,
    pub created_label: String,
    pub expires_label: String,
    /// Whether the request holds this export's download token, and so whether
    /// to render the link. The token itself is never put in the page: it lives
    /// in the path-scoped `export_{id}` cookie and the browser attaches it.
    pub downloadable: bool,
    pub is_ready: bool,
}

pub fn export_state_label(t: Translator, s: bikesnest_domain::ExportState) -> &'static str {
    match s {
        bikesnest_domain::ExportState::Ready => t.t("export.state.ready"),
        bikesnest_domain::ExportState::Downloaded => t.t("export.state.downloaded"),
        bikesnest_domain::ExportState::Expired => t.t("export.state.expired"),
    }
}

/// Build a export row. `token_held` says whether this request carries the export's
/// single-use download token (the `export_{id}` cookie) — true only for the
/// export the owner just requested, and only while it is still `READY`.
pub fn export_vm(t: Translator, e: &bikesnest_application::Export, token_held: bool) -> ExportVm {
    let is_ready = e.state == bikesnest_domain::ExportState::Ready;
    ExportVm {
        id: e.id,
        state_code: e.state.as_code(),
        state_label: export_state_label(t, e.state),
        created_label: iso_datetime_label(t, e.created_at),
        expires_label: iso_datetime_label(t, e.expires_at),
        downloadable: is_ready && token_held,
        is_ready,
    }
}

/// One row of the admin privacy-request queue (or the privacy rights list).
///
/// A rights request is a legal clock, so the row carries the three facts an
/// operator needs to act: who asked, what they wrote, and how long is left
/// (LGPD art. 19 — [`bikesnest_domain::PRIVACY_REQUEST_SLA_DAYS`]).
#[derive(Debug, Clone)]
pub struct PrivacyRequestVm {
    pub id: i64,
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub state_code: &'static str,
    pub state_label: &'static str,
    pub created_label: String,
    /// Display name or email of the subject; the anonymized-account fallback
    /// when the user row is gone.
    pub subject_label: String,
    /// Their row in the admin user list, when the account still exists.
    pub subject_url: Option<String>,
    /// The free text the user typed with the request.
    pub details: Option<String>,
    /// "3 days left" / "2 days overdue"; empty once the request is closed.
    pub deadline_label: String,
    pub is_overdue: bool,
}

pub fn privacy_request_kind_label(
    t: Translator,
    kind: bikesnest_domain::PrivacyRequestKind,
) -> &'static str {
    match kind {
        bikesnest_domain::PrivacyRequestKind::Access => t.t("privacy.kind.access"),
        bikesnest_domain::PrivacyRequestKind::Rectification => t.t("privacy.kind.rectification"),
        bikesnest_domain::PrivacyRequestKind::Deletion => t.t("privacy.kind.deletion"),
        bikesnest_domain::PrivacyRequestKind::Export => t.t("privacy.kind.export"),
        bikesnest_domain::PrivacyRequestKind::Restriction => t.t("privacy.kind.restriction"),
        bikesnest_domain::PrivacyRequestKind::Objection => t.t("privacy.kind.objection"),
        bikesnest_domain::PrivacyRequestKind::ConsentWithdrawal => t.t("privacy.kind.consent"),
    }
}

pub fn privacy_request_state_label(
    t: Translator,
    s: bikesnest_domain::PrivacyRequestState,
) -> &'static str {
    match s {
        bikesnest_domain::PrivacyRequestState::Open => t.t("privacy.state.open"),
        bikesnest_domain::PrivacyRequestState::InProgress => t.t("privacy.state.in_progress"),
        bikesnest_domain::PrivacyRequestState::Completed => t.t("privacy.state.completed"),
        bikesnest_domain::PrivacyRequestState::Declined => t.t("privacy.state.declined"),
    }
}

/// `labels` is the batched subject lookup for every request on the page.
pub fn privacy_request_vm(
    t: Translator,
    r: &bikesnest_application::PrivacyRequest,
    labels: &std::collections::HashMap<i64, String>,
) -> PrivacyRequestVm {
    let subject = r.user_id.map(|u| u.0);
    let is_open = matches!(
        r.state,
        bikesnest_domain::PrivacyRequestState::Open
            | bikesnest_domain::PrivacyRequestState::InProgress
    );
    let days_left = bikesnest_domain::privacy_request_days_left(r.created_at, chrono::Utc::now());
    let deadline_label = if !is_open {
        String::new()
    } else if days_left < 0 {
        t.t("admin.privacy_requests.overdue")
            .replace("{n}", &(-days_left).to_string())
    } else {
        t.t("admin.privacy_requests.days_left")
            .replace("{n}", &days_left.to_string())
    };
    PrivacyRequestVm {
        id: r.id,
        kind_code: r.kind.as_code(),
        kind_label: privacy_request_kind_label(t, r.kind),
        state_code: r.state.as_code(),
        state_label: privacy_request_state_label(t, r.state),
        created_label: iso_datetime_label(t, r.created_at),
        subject_label: subject
            .and_then(|id| labels.get(&id).cloned())
            .or_else(|| subject.map(|id| format!("#{id}")))
            .unwrap_or_else(|| t.t("privacy.subject.anonymized").to_string()),
        subject_url: subject
            .filter(|id| labels.contains_key(id))
            .map(|id| format!("/admin/users?q={id}")),
        details: r
            .details
            .get("note")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        deadline_label,
        is_overdue: is_open && days_left < 0,
    }
}

/// One row of the policy version-history list.
#[derive(Debug, Clone)]
pub struct PolicyVersionVm {
    pub version: String,
    pub effective_label: String,
    pub is_current: bool,
}

pub fn policy_version_vm(
    t: Translator,
    doc: &bikesnest_application::PolicyDocument,
) -> PolicyVersionVm {
    PolicyVersionVm {
        version: doc.version.clone(),
        effective_label: iso_datetime_label(t, doc.effective_at),
        is_current: doc.superseded_at.is_none(),
    }
}

/// One selectable manual rights kind on the privacy hub.
#[derive(Debug, Clone)]
pub struct PrivacyRequestKindVm {
    pub code: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

/// The manual (operator-fulfilled) rights kinds, with descriptions, for the privacy
/// request list. Access/export and deletion are automatic and have their
/// own cards.
pub fn privacy_request_kind_options(t: Translator) -> Vec<PrivacyRequestKindVm> {
    vec![
        PrivacyRequestKindVm {
            code: "rectification",
            label: privacy_request_kind_label(
                t,
                bikesnest_domain::PrivacyRequestKind::Rectification,
            ),
            description: t.t("privacy.rights.rectification_desc"),
        },
        PrivacyRequestKindVm {
            code: "restriction",
            label: privacy_request_kind_label(t, bikesnest_domain::PrivacyRequestKind::Restriction),
            description: t.t("privacy.rights.restriction_desc"),
        },
        PrivacyRequestKindVm {
            code: "objection",
            label: privacy_request_kind_label(t, bikesnest_domain::PrivacyRequestKind::Objection),
            description: t.t("privacy.rights.objection_desc"),
        },
        PrivacyRequestKindVm {
            code: "consent_withdrawal",
            label: privacy_request_kind_label(
                t,
                bikesnest_domain::PrivacyRequestKind::ConsentWithdrawal,
            ),
            description: t.t("privacy.rights.consent_desc"),
        },
    ]
}

pub fn contribution_vm(
    t: Translator,
    i: &bikesnest_application::ContributionItem,
) -> ContributionVm {
    // "parked_here" and "photo.pending" are their own kinds — a parked-here
    // signal is not a verification, and a pending photo isn't approved yet,
    // so neither should read as "Verificou" (a real existence/attribute
    // verification).
    let (kind_code, kind): (&'static str, &'static str) = match i.kind.as_str() {
        "added" => ("added", t.t("contrib.kind.added")),
        "edited" => ("edited", t.t("contrib.kind.edited")),
        "proposed" => ("proposed", t.t("contrib.kind.proposed")),
        "reviewed" => ("reviewed", t.t("contrib.kind.reviewed")),
        "verified" => ("verified", t.t("contrib.kind.verified")),
        "parked_here" => ("parked_here", t.t("contrib.kind.parked_here")),
        "favorited" => ("favorited", t.t("contrib.kind.favorited")),
        "photo.pending" => ("photo_pending", t.t("contrib.kind.photo_pending")),
        _ => ("other", t.t("contrib.kind.other")),
    };
    let state = match i.state.as_str() {
        "active" => t.t("contrib.state.active"),
        "pending" => t.t("contrib.state.pending"),
        "approved" => t.t("profile.approved"),
        "rejected" => t.t("profile.rejected"),
        "superseded" => t.t("profile.superseded"),
        "history" => t.t("contrib.state.history"),
        _ => t.t("contrib.state.other"),
    };
    ContributionVm {
        kind_code,
        kind_label: kind,
        target: i.target.clone(),
        state_label: state,
        at_label: time_ago_label(t, i.at),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Locale;
    use bikesnest_application::{Cursor, SearchPage, Sort};
    use bikesnest_test_support::TestObjectStorage;

    fn page_with_next() -> SearchPage {
        SearchPage {
            items: Vec::new(),
            total: 42,
            next_cursor: Some(Cursor {
                sort: Sort::Distance,
                v: 250.0,
                id: 7,
            }),
        }
    }

    async fn cursor_url_for(query_string: &str) -> Option<String> {
        let storage = TestObjectStorage::new();
        build_results(
            Translator::new(Locale::En),
            &page_with_next(),
            None,
            None,
            query_string.to_string(),
            chrono::Utc::now(),
            &bikesnest_domain::DEFAULT_THRESHOLDS,
            &storage,
        )
        .await
        .cursor_url
    }

    /// The query string arrives without a leading "?" — the next-page link has
    /// to open the query itself, or the first parameter fuses onto the path.
    #[tokio::test]
    async fn next_page_url_keeps_the_current_query() {
        let url = cursor_url_for("q=rua&radius=1000")
            .await
            .expect("a next page exists");
        assert!(
            url.starts_with("/search?q=rua"),
            "query must open with '?': {url}"
        );
        assert!(url.contains("&radius=1000"), "filters are kept: {url}");
        assert!(url.contains("&cursor="), "cursor is appended: {url}");
    }

    #[tokio::test]
    async fn next_page_url_without_a_query_still_opens_the_query_string() {
        let url = cursor_url_for("").await.expect("a next page exists");
        assert!(url.starts_with("/search?cursor="), "{url}");
    }

    #[tokio::test]
    async fn no_next_cursor_means_no_next_page_link() {
        let storage = TestObjectStorage::new();
        let page = SearchPage {
            items: Vec::new(),
            total: 3,
            next_cursor: None,
        };
        let results = build_results(
            Translator::new(Locale::En),
            &page,
            None,
            None,
            "q=rua".to_string(),
            chrono::Utc::now(),
            &bikesnest_domain::DEFAULT_THRESHOLDS,
            &storage,
        )
        .await;
        assert!(results.cursor_url.is_none());
    }

    /// the search map's JSON island must carry only what `search.js`
    /// reads for a marker — not the full `CardVm` (labels, image paths,
    /// security chips, …).
    #[test]
    fn map_item_json_contains_only_the_allowed_keys() {
        let item = MapItemVm {
            id: 42,
            n: 1,
            lat: -25.43,
            lon: -49.27,
            name: "Paraciclo Rua XV".to_string(),
            distance_label: "300 m".to_string(),
            cost_label: "Free".to_string(),
            href: "/parking/42".to_string(),
        };
        let value = serde_json::to_value(&item).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .expect("MapItemVm serializes to a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "cost_label",
                "distance_label",
                "href",
                "id",
                "lat",
                "lon",
                "n",
                "name"
            ]
        );
    }

    #[test]
    fn mask_email_keeps_the_domain_and_one_character() {
        assert_eq!(mask_email("clemente@brick.so"), "c***@brick.so");
        // A one-character local part masks the same way, so the mask never
        // leaks how long the hidden part is.
        assert_eq!(mask_email("a@example.com"), "a***@example.com");
        assert_eq!(mask_email("@example.com"), "***@example.com");
        // Not an address at all: reveal nothing rather than echo it back.
        assert_eq!(mask_email("not-an-email"), "***");
        assert_eq!(mask_email(""), "***");
    }

    #[test]
    fn audit_targets_only_link_where_a_page_exists() {
        assert_eq!(
            audit_target_url("parking_location", "12"),
            Some("/parking/12".to_string())
        );
        assert_eq!(
            audit_target_url("user", "7"),
            Some("/admin/users?q=7".to_string())
        );
        assert_eq!(
            audit_target_url("report", "3"),
            Some("/moderation/reports".to_string())
        );
        // A non-numeric handle (a token, a session hash) must never be pasted
        // into a path.
        assert_eq!(audit_target_url("parking_location", "abc"), None);
        assert_eq!(audit_target_url("user", "1; DROP TABLE"), None);
        // No page for this type, and no empty-id links.
        assert_eq!(audit_target_url("session", "9"), None);
        assert_eq!(audit_target_url("user", ""), None);
    }
}
