//! Search, browse and details view models: cards, map items, hours.

use super::format::*;
use crate::i18n::Translator;
use bikesnest_application::{GeoHit, ObjectStorage, ParkingSummary};
use bikesnest_domain::{FreshnessCategory, OpenStatus, OpeningHours, ParkingType};
use chrono::{Datelike, Timelike};

/// One parking card in the search results list.
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
    /// Illustrative fallback photo (per type) when a location has no photo
    /// yet. Decorative: it is not a picture of this spot, so it has no alt text.
    pub image: &'static str,
}

impl CardVm {
    pub fn from_summary(
        t: Translator,
        s: &ParkingSummary,
        freshness: FreshnessCategory,
        photo_url: Option<String>,
    ) -> Self {
        let image = image_for(s.parking_type);
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
            distance_label: distance_label(t, s.distance_m),
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
        }
    }
}

/// Deterministic fallback photo per parking type (photos from the approved
/// design export). Used only when a location has no photo of its own.
fn image_for(ty: ParkingType) -> &'static str {
    match ty {
        ParkingType::Rack => "/static/img/street-rack-mint-bike.jpg",
        ParkingType::ParkingFacility | ParkingType::Indoor | ParkingType::Secured => {
            "/static/img/square-bike-rows.jpg"
        }
        ParkingType::Locker | ParkingType::Other => "/static/img/mtb-pair-rack.jpg",
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
            distance_label: distance_label(t, s.distance_m),
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
            "bbox": [bounds.west(), bounds.south(), bounds.east(), bounds.north()],
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
