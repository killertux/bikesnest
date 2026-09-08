//! Ports and read models for parking search.

use async_trait::async_trait;
use bikesnest_domain::{Cost, FreshnessThresholds, GeoPoint, ParkingType, Rating};

/// A resolved search origin. Geocoders return this for free-text destinations;
/// coordinate searches construct the same shape so callers can render both
/// origins consistently.
#[derive(Debug, Clone, PartialEq)]
pub struct GeoHit {
    pub label: String,
    pub point: GeoPoint,
}

#[derive(Debug, thiserror::Error)]
pub enum GeocodeError {
    #[error("geocoder unavailable")]
    Unavailable,
    #[error("geocoder failed: {0}")]
    Unexpected(String),
}

/// One provider-ranked address prediction.
///
/// Some providers include coordinates with their predictions. Others, such as
/// Google Places, return an opaque reference that must be resolved after the
/// user chooses the prediction. The web API exposes the same shape for both.
#[derive(Debug, Clone, PartialEq)]
pub struct AddressSuggestion {
    pub label: String,
    pub reference: Option<String>,
    pub point: Option<GeoPoint>,
}

/// Port: resolve a destination string to coordinates.
#[async_trait]
pub trait Geocoder: Send + Sync {
    /// `Ok(None)` when nothing matches the query.
    async fn geocode(&self, query: &str) -> Result<Option<GeoHit>, GeocodeError>;

    /// Ordered suggestions for an incomplete destination query.
    ///
    /// Providers that only support a single result still get useful default
    /// behavior. Autocomplete-capable adapters override this to return up to
    /// `limit` candidates in provider-ranked order.
    async fn suggest(
        &self,
        query: &str,
        limit: usize,
        _session_token: Option<&str>,
        _language_code: &str,
    ) -> Result<Vec<AddressSuggestion>, GeocodeError> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        Ok(self
            .geocode(query)
            .await?
            .into_iter()
            .map(|hit| AddressSuggestion {
                label: hit.label,
                reference: None,
                point: Some(hit.point),
            })
            .collect())
    }

    /// Resolve the opaque reference returned by [`Self::suggest`].
    /// Providers whose suggestions already contain coordinates use the
    /// default no-result implementation.
    async fn resolve_suggestion(
        &self,
        _reference: &str,
        _session_token: Option<&str>,
    ) -> Result<Option<GeoHit>, GeocodeError> {
        Ok(None)
    }
}

// ---------------------------------------------------------------------------
// Search criteria
// ---------------------------------------------------------------------------

/// Radius allowlist: a search radius is one of these or the default, never a
/// free number, so no URL can ask the database for the whole country. 5 km is
/// the widest a *radius* goes — past that the honest answer is browse mode's
/// bounding box, not a bigger circle.
pub const RADIUS_OPTIONS_M: &[u32] = &[250, 500, 1000, 2000, 5000];
pub const DEFAULT_RADIUS_M: u32 = 1000;
/// Page size defaults/limits.
pub const DEFAULT_PAGE_SIZE: usize = 20;
pub const MAX_PAGE_SIZE: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Recommended,
    Distance,
    Security,
    Rating,
    RecentlyVerified,
}

impl Sort {
    pub fn as_code(&self) -> &'static str {
        match self {
            Sort::Recommended => "recommended",
            Sort::Distance => "distance",
            Sort::Security => "security",
            Sort::Rating => "rating",
            Sort::RecentlyVerified => "recently_verified",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "recommended" => Some(Sort::Recommended),
            "distance" => Some(Sort::Distance),
            "security" => Some(Sort::Security),
            "rating" => Some(Sort::Rating),
            "recently_verified" => Some(Sort::RecentlyVerified),
            _ => None,
        }
    }
}

/// Cost filter. `Paid` matches any paid location regardless of price.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostFilter {
    Free,
    Paid,
    Unknown,
}

impl CostFilter {
    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "free" => Some(CostFilter::Free),
            "paid" => Some(CostFilter::Paid),
            "unknown" => Some(CostFilter::Unknown),
            _ => None,
        }
    }

    /// Does a location's cost match this filter?
    pub fn matches(&self, cost: &Cost) -> bool {
        matches!(
            (self, cost),
            (CostFilter::Free, Cost::Free)
                | (CostFilter::Paid, Cost::Paid { .. })
                | (CostFilter::Unknown, Cost::Unknown)
        )
    }
}

/// Structured filters. All optional; all ANDed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filters {
    pub cost: Option<CostFilter>,
    pub types: Vec<ParkingType>,
    /// Location must have ALL of these security attributes confirmed (state=yes).
    pub security_all: Vec<String>,
    pub open_now: bool,
}

/// Opaque keyset cursor: `(sort_key, id)`, base64-encoded JSON.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cursor {
    pub sort: Sort,
    /// Normalized ascending sort key (see infrastructure query: distance keeps
    /// its value; other sorts are negated so every sort paginates ascending).
    pub v: f64,
    pub id: i64,
}

impl Cursor {
    pub fn encode(&self) -> String {
        let json = serde_json::json!({ "s": self.sort.as_code(), "v": self.v, "id": self.id });
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.to_string())
    }

    /// Decodes a cursor; unknown/invalid cursors fall back to page 1 rather
    /// than erroring (defensive against tampered URLs).
    pub fn decode(raw: &str) -> Option<Self> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(raw)
            .ok()?;
        let json: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        let sort = Sort::from_code(json.get("s")?.as_str()?)?;
        let v = json.get("v")?.as_f64()?;
        let id = json.get("id")?.as_i64()?;
        Some(Self { sort, v, id })
    }
}

/// Raw, untrusted search input from the web layer (: handlers only map
/// HTTP params; business rules live here).
#[derive(Debug, Clone, Default)]
pub struct SearchInput {
    /// Free-text destination. Ignored when explicit coordinates exist.
    pub query: Option<String>,
    /// Explicit origin (browser geolocation or bookmarked URL).
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub radius_m: Option<u32>,
    pub cost: Option<String>,
    /// Comma-separated type codes.
    pub types: Option<String>,
    /// Comma-separated security feature codes (all-of).
    pub security: Option<String>,
    pub open_now: bool,
    pub sort: Option<String>,
    pub page_size: Option<usize>,
    pub cursor: Option<String>,
    /// Browse mode: a raw `west,south,east,north` bounding box (WGS84) from the
    /// map, parsed and validated by [`BoundsQuery::parse`]. Only honoured when
    /// there is no destination to search around (see `SearchParking::browse`).
    pub bbox: Option<String>,
}

impl SearchInput {
    /// Parses filters and options; coordinate/geometry validation happens in
    /// `SearchRequest::new`. Unknown codes are dropped (not errors) so a
    /// stale shared URL still renders results.
    pub fn filters(&self) -> Filters {
        Filters {
            cost: self.cost.as_deref().and_then(CostFilter::from_code),
            types: self
                .types
                .as_deref()
                .unwrap_or("")
                .split(',')
                .filter_map(|c| ParkingType::from_code(c).ok())
                .collect(),
            security_all: self
                .security
                .as_deref()
                .unwrap_or("")
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            open_now: self.open_now,
        }
    }
}

/// Fully validated search request. Constructed only via [`SearchRequest::resolve`]
/// so business rules (radius allowlist, page-size clamp, filter sanity) cannot
/// be bypassed by the web layer.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchRequest {
    pub origin: GeoPoint,
    /// Resolved destination label, when the origin came from a geocode hit.
    pub destination_label: Option<String>,
    pub radius_m: u32,
    pub filters: Filters,
    pub sort: Sort,
    pub page_size: usize,
    pub cursor: Option<Cursor>,
}

impl SearchRequest {
    /// Validates and clamps: radius to the allowlist, page size to  limits,
    /// type codes, cursor/sort agreement (mismatched cursor → page 1).
    pub fn new(
        origin: GeoPoint,
        destination_label: Option<String>,
        radius_m: u32,
        filters: Filters,
        sort: Sort,
        page_size: usize,
        cursor_raw: Option<&str>,
    ) -> Self {
        let radius_m = if RADIUS_OPTIONS_M.contains(&radius_m) {
            radius_m
        } else {
            DEFAULT_RADIUS_M
        };
        let page_size = page_size.clamp(1, MAX_PAGE_SIZE);
        let cursor = cursor_raw
            .and_then(Cursor::decode)
            .filter(|c| c.sort == sort);
        // Deduplicate + validate type codes; drop unknown security codes.
        let mut types = filters.types.clone();
        types.sort_by_key(|t| t.as_code());
        types.dedup();
        // Unknown codes are dropped, not matched: an unknown code can never be
        // confirmed on any location, so keeping it would silently turn the
        // whole search into "no results" — a stale or hand-edited URL degrades
        // to the results it can still honour instead.
        let mut security_all: Vec<String> = filters
            .security_all
            .iter()
            .map(|c| c.trim().to_string())
            .filter(|c| bikesnest_domain::is_known_security_code(c))
            .collect();
        security_all.sort();
        security_all.dedup();
        Self {
            origin,
            destination_label,
            radius_m,
            filters: Filters {
                cost: filters.cost,
                types,
                security_all,
                open_now: filters.open_now,
            },
            sort,
            page_size,
            cursor,
        }
    }
}

/// One row of search results — everything a search card needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkingSummary {
    pub id: i64,
    pub name: String,
    pub address: String,
    pub parking_type: ParkingType,
    pub cost: Cost,
    pub point: GeoPoint,
    /// Distance from the search origin, in meters.
    pub distance_m: f64,
    /// Codes of security attributes explicitly confirmed present.
    pub security_yes: Vec<String>,
    pub rating: Rating,
    pub last_verified_at: Option<chrono::DateTime<chrono::Utc>>,
    pub timezone: chrono_tz::Tz,
    /// Computed in SQL from wall-clock hours.
    pub is_open_now: bool,
    /// Object-storage key of the location's primary approved photo, if any.
    /// The web layer resolves this to a presigned URL for the card; `None`
    /// falls back to a positional illustrative image.
    pub photo_key: Option<String>,
    /// Normalized ascending keyset sort key, exactly as the search reader
    /// computed it for this request's sort — distance keeps its value, the
    /// other sorts negate. The next-page cursor is built from this value,
    /// so it must never be recomputed in Rust: any rounding or unit mismatch
    /// with the SQL expression would make the keyset predicate skip or repeat
    /// rows. Only the distance sort's key is a distance — and even there it is
    /// the sphere distance the GIST index orders on, not [`Self::distance_m`].
    /// `None` for summaries no cursor can be minted from: anything not read by
    /// the search reader, and browse-mode rows (a viewport has no next page).
    pub sort_key: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    pub items: Vec<ParkingSummary>,
    /// Total matching the criteria within the radius (before pagination).
    pub total: i64,
    /// Present when another page exists.
    pub next_cursor: Option<Cursor>,
}

// ---------------------------------------------------------------------------
// Browse mode: "what is inside the map I am looking at"
// ---------------------------------------------------------------------------

/// Most markers a browse answer may carry. Past this the reader clusters
/// instead: a viewport holding thousands of spots is a zoom-out, not a page,
/// and neither the browser nor the reader should pay for markers nobody can
/// tell apart.
pub const BROWSE_MARKER_CAP: usize = 200;

/// How many of those markers the accessible list shows. Browse is not
/// paginated (there is no stable keyset over "what the map shows"), so the
/// list is the nearest [`BROWSE_LIST_CAP`] to the centre and the UI asks for a
/// smaller area instead of a next page.
pub const BROWSE_LIST_CAP: usize = 50;

/// Largest bounding box a browse request may ask for, per axis, in degrees.
/// One degree of latitude is ~111 km — beyond that the answer is always a
/// cluster map, so a wider box only buys a more expensive count.
pub const MAX_BROWSE_SPAN_DEG: f64 = 1.0;

/// Columns of the clustering grid a browse answer is snapped to when it has
/// too many rows to draw individually. The cell is the box's *width* divided
/// by this, so a cluster map always reads as a ~12-column grid whatever the
/// zoom.
pub const BROWSE_GRID_COLUMNS: f64 = 12.0;

/// A validated map viewport plus the same filters a radius search takes.
///
/// Constructed only through [`BoundsQuery::parse`], so no unbounded,
/// inside-out or off-globe box can reach the reader.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundsQuery {
    pub west: f64,
    pub south: f64,
    pub east: f64,
    pub north: f64,
    pub filters: Filters,
    /// Marker cap for this query — [`BROWSE_MARKER_CAP`] in production; tests
    /// lower it to exercise clustering without inserting hundreds of rows.
    pub limit: usize,
}

impl BoundsQuery {
    /// Parses `west,south,east,north` (WGS84 degrees) as the map writes it.
    ///
    /// `None` — a 400 to the caller — when the box is not four finite numbers,
    /// is off the globe (`|lat| > 90`, `|lon| > 180`), is inside out
    /// (`west >= east` / `south >= north`), or spans more than
    /// [`MAX_BROWSE_SPAN_DEG`] on either axis. An antimeridian-crossing box is
    /// "inside out" by this rule and is rejected rather than silently split.
    pub fn parse(raw: &str, filters: Filters, limit: usize) -> Option<Self> {
        let parts: Vec<f64> = raw
            .split(',')
            .map(|p| p.trim().parse::<f64>())
            .collect::<Result<_, _>>()
            .ok()?;
        let [west, south, east, north] = parts[..] else {
            return None;
        };
        if ![west, south, east, north].iter().all(|v| v.is_finite()) {
            return None;
        }
        if west.abs() > 180.0 || east.abs() > 180.0 || south.abs() > 90.0 || north.abs() > 90.0 {
            return None;
        }
        if west >= east || south >= north {
            return None;
        }
        if east - west > MAX_BROWSE_SPAN_DEG || north - south > MAX_BROWSE_SPAN_DEG {
            return None;
        }
        Some(Self {
            west,
            south,
            east,
            north,
            filters,
            limit: limit.clamp(1, BROWSE_MARKER_CAP),
        })
    }

    /// The box's centre — what browse-mode distances are measured from, since
    /// there is no destination to be near.
    pub fn center(&self) -> GeoPoint {
        GeoPoint::new(
            (self.south + self.north) / 2.0,
            (self.west + self.east) / 2.0,
        )
        .expect("a validated bounds' midpoint is on the globe")
    }

    /// Side of the clustering grid cell, in degrees: the box's width over
    /// [`BROWSE_GRID_COLUMNS`]. Square in degrees (not in metres), which is
    /// what keeps the grid aligned with the box the viewer is looking at.
    pub fn cell_deg(&self) -> f64 {
        (self.east - self.west) / BROWSE_GRID_COLUMNS
    }
}

/// One clustering-grid cell that held more than one location: the mean
/// position of its members, and how many there were.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cluster {
    pub lat: f64,
    pub lon: f64,
    pub count: i64,
}

/// What is inside a viewport.
///
/// Either `items` (at most [`BoundsQuery::limit`], nearest the centre first)
/// or `clusters` — never both: past the cap individual markers are replaced by
/// grid counts whose `count`s sum to `total`.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundsPage {
    pub items: Vec<ParkingSummary>,
    /// Everything matching inside the box, cap or no cap.
    pub total: usize,
    pub clusters: Vec<Cluster>,
}

#[derive(Debug, thiserror::Error)]
pub enum ReaderError {
    #[error("database unavailable")]
    Unavailable,
    #[error("database error: {0}")]
    Unexpected(String),
}

/// Port: the two ways to read `parking_location` for results — a
/// keyset-paginated search around a destination, and a whole map viewport.
#[async_trait]
pub trait ParkingSearchReader: Send + Sync {
    /// Applies every criterion and every sort, `Recommended` included: the
    /// reader owns the sort key, so all five sorts paginate through the same
    /// keyset predicate and nothing is re-ranked after the page is read.
    /// `apply_cursor` gates the keyset predicate; the use case only sets it
    /// once it has checked that the cursor was minted for this sort.
    async fn search(
        &self,
        request: &SearchRequest,
        limit: usize,
        apply_cursor: bool,
    ) -> Result<SearchPage, ReaderError>;

    /// Browse mode: everything inside a map viewport, nearest the box's centre
    /// first, capped at `query.limit` — or, when more rows match than that,
    /// the same rows as counts on a grid ([`BoundsPage::clusters`]).
    ///
    /// Not a radius search with a square: there is no origin to be near, no
    /// sort to choose and no cursor to follow, so it answers with a whole
    /// viewport rather than a page of one.
    async fn in_bounds(&self, query: &BoundsQuery) -> Result<BoundsPage, ReaderError>;
}

/// Port: the ids of every publicly listed location, for the sitemap.
///
/// Its own port rather than a reuse of [`ParkingSearchReader`]: the sitemap
/// wants every ACTIVE id in a stable order, with none of the search criteria,
/// ranking or pagination that reader exists to apply.
#[async_trait]
pub trait SitemapReader: Send + Sync {
    async fn active_parking_ids(&self) -> Result<Vec<i64>, ReaderError>;
}

/// Port: full aggregate for the details page.
#[async_trait]
pub trait ParkingDetailsReader: Send + Sync {
    async fn details(
        &self,
        id: i64,
    ) -> Result<Option<bikesnest_domain::ParkingLocation>, ReaderError>;
}

/// A stored photo reference: an opaque object-storage key plus its content type
/// and accessible description. The web layer turns the key into a presigned URL.
/// When a processed thumbnail exists, [`Self::thumbnail_key`] is present and
/// the gallery prefers it; seeded photos fall back to [`Self::key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPhoto {
    pub key: String,
    pub thumbnail_key: Option<String>,
    pub content_type: String,
    pub alt: Option<String>,
}

/// Port: approved photos for a location, in display order (gallery and search
/// card). Kept separate from [`ParkingDetailsReader`] so the details aggregate
/// and its use case stay unchanged (photos are a read-side, presentation
/// concern joined at the web boundary).
#[async_trait]
pub trait ParkingPhotoReader: Send + Sync {
    async fn photos(&self, location_id: i64) -> Result<Vec<StoredPhoto>, ReaderError>;
    /// Public-safe count only; pending images and uploader identities stay private.
    async fn pending_count(&self, _location_id: i64) -> Result<i64, ReaderError> {
        Ok(0)
    }
}

/// Port: approved photos attached to *reviews*, in display order.
/// Only `APPROVED` review photos render on the review card.
#[async_trait]
pub trait ReviewPhotosReader: Send + Sync {
    /// Batched form of "approved photos for this review, in order" for every
    /// review on a details page — one query instead of one per review.
    /// Review ids absent from the map have no approved photos.
    async fn for_reviews(
        &self,
        review_ids: &[i64],
    ) -> Result<std::collections::HashMap<i64, Vec<StoredPhoto>>, ReaderError>;
}

/// Shared freshness configuration for view-building use cases.
#[derive(Debug, Clone, Copy)]
pub struct FreshnessConfig {
    pub thresholds: FreshnessThresholds,
}

impl Default for FreshnessConfig {
    fn default() -> Self {
        Self {
            thresholds: bikesnest_domain::DEFAULT_THRESHOLDS,
        }
    }
}
