//! SQL-backed parking search: the nearby query behind the results page.
//!
//! Two runtime-checked statements per search, both driven by the same filter
//! text: a COUNT that answers "how many match" over nothing but the filters,
//! and a PAGE that orders, limits, and only then decorates.
//!
//! Why two. The count used to ride along in the paging query as a CTE
//! (`total AS (SELECT count(*) FROM matching)`), which forced the whole
//! filtered set to be materialised before the LIMIT could apply — so the four
//! per-row subqueries (security count, security codes, open-now, first photo)
//! ran once for every location inside the radius to render a page of twenty.
//! Splitting them makes the work O(page) instead of O(radius) and fixes a
//! second bug for free: the total no longer rides on the rows, so a page past
//! the end reports the real total instead of 0.
//!
//! The PAGE statement is a keyset page of ids computed from columns and cheap
//! expressions, wrapped in a `MATERIALIZED` CTE so the decoration below it
//! runs exactly once per row on the page (EXPLAIN: `loops=21` for a page of
//! twenty plus the look-ahead row).
//!
//! All five sorts normalize to an ascending `sort_key` (distance keeps its
//! value; the others negate), so a single keyset predicate
//! `(sort_key, id) > ($v, $id)` paginates every one of them — `Recommended`
//! included: its score is this query's sort key, computed in SQL from the
//! configured weights, rather than a re-ranking of whatever rows a capped
//! fetch happened to return.
//!
//! The key each row carries back on its summary *is* the value the query
//! computed, so the cursor the application builds and the predicate that
//! consumes it are the same number in the same units. Note that the distance
//! sort's key is the sphere distance the GIST index orders on
//! (`location <-> origin`, which is what makes the index's KNN path reachable)
//! while `distance_m` stays the spheroid `ST_Distance` the card displays and
//! the recommendation score uses. They agree to ~0.3%; the key is opaque, so
//! only its internal consistency matters.
//!
//! Browse mode (`in_bounds`) is the same shape one question over: a count
//! over the map's bounding box, then *either* the nearest-to-centre rows (up
//! to the marker cap, decorated in a `MATERIALIZED` CTE exactly as the page
//! above) *or* — when the box holds more than the cap — those rows snapped to
//! a grid and counted. It shares the filter text but not the numbering: a
//! viewport has no origin, no sort and no cursor, so it gets its own
//! `BOUNDS_FILTERS`/`BOUNDS_ORIGIN` pair rather than bending the radius one.
//!
//! "Open now" is not written here at all: it is `bikesnest_is_open_at`
//! (migration 0020), called once as a filter and once as the row's flag. The
//! domain keeps `OpeningHours::status_at` for the details page, which needs
//! the Open/Closed/**Unknown** tri-state that a card's boolean cannot carry;
//! `parking_test.rs` pins the two together.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{
    BoundsPage, BoundsQuery, Cluster, CostFilter, FreshnessConfig, ParkingSummary, ReaderError,
    RecommendationConfig, SearchPage, SearchRequest, Sort,
};
use bikesnest_domain::{Cost, CurrencyCode, GeoPoint, Money, ParkingType, PricingUnit, Rating};

pub struct SqlxParkingSearchReader {
    db: Db,
    /// Weights for the `Recommended` sort key, bound into the paging query.
    recommendation: RecommendationConfig,
    /// Day thresholds behind the freshness sub-score of that same key.
    freshness: FreshnessConfig,
}

#[derive(sqlx::FromRow)]
struct SearchRow {
    id: i64,
    name: String,
    address: String,
    parking_type: String,
    cost_kind: String,
    price_cents: Option<i64>,
    price_currency: Option<String>,
    price_unit: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    timezone: String,
    rating_avg: Option<f64>,
    rating_count: i32,
    last_verified_at: Option<chrono::DateTime<chrono::Utc>>,
    distance_m: Option<f64>,
    security_yes_codes: Option<Vec<String>>,
    is_open_now: Option<bool>,
    photo_key: Option<String>,
    sort_key: Option<f64>,
}

fn cost_of(row: &SearchRow) -> Result<Cost, ReaderError> {
    let price = match (row.price_cents, &row.price_currency, &row.price_unit) {
        (Some(cents), Some(cur), Some(unit)) => Some(Money::new(
            cents,
            CurrencyCode::parse(cur).map_err(|e| ReaderError::Unexpected(e.to_string()))?,
            PricingUnit::from_code(unit).map_err(|e| ReaderError::Unexpected(e.to_string()))?,
        )),
        _ => None,
    };
    Cost::from_kind_and_price(&row.cost_kind, price)
        .map_err(|e| ReaderError::Unexpected(e.to_string()))
}

fn summary_of(row: SearchRow) -> Result<ParkingSummary, ReaderError> {
    let timezone: chrono_tz::Tz = row
        .timezone
        .parse()
        .map_err(|_| ReaderError::Unexpected(format!("bad timezone {}", row.timezone)))?;
    let cost = cost_of(&row)?;
    let parking_type = ParkingType::from_code(&row.parking_type)
        .map_err(|e| ReaderError::Unexpected(e.to_string()))?;
    Ok(ParkingSummary {
        id: row.id,
        name: row.name,
        address: row.address,
        parking_type,
        cost,
        point: GeoPoint::new(row.lat.unwrap_or(0.0), row.lon.unwrap_or(0.0))
            .map_err(|e| ReaderError::Unexpected(e.to_string()))?,
        distance_m: row.distance_m.unwrap_or(0.0),
        security_yes: row.security_yes_codes.unwrap_or_default(),
        rating: Rating::new(row.rating_avg, i64::from(row.rating_count))
            .map_err(|e| ReaderError::Unexpected(e.to_string()))?,
        last_verified_at: row.last_verified_at,
        timezone,
        is_open_now: row.is_open_now.unwrap_or(false),
        photo_key: row.photo_key,
        sort_key: row.sort_key,
    })
}

/// The search origin as a geography point, from the `$1` (lat) / `$2` (lon)
/// binds. Written once and interpolated, so the count, the sort key and the
/// displayed distance can never drift onto different origins.
const ORIGIN: &str = "ST_SetSRID(ST_MakePoint($2, $1), 4326)::geography";

/// Everything that decides *whether* a location matches, and nothing that
/// decides where it ranks. Shared verbatim by both statements: the count and
/// the page cannot disagree about the size of the result set.
const FILTERS: &str = r#"
    pl.moderation_state = 'ACTIVE'
      AND ST_DWithin(pl.location, ST_SetSRID(ST_MakePoint($2, $1), 4326)::geography, $3)
      AND ($4::text IS NULL OR pl.cost_kind = $4)
      AND ($5::text[] IS NULL OR pl.parking_type = ANY($5))
      AND ($6 = false OR bikesnest_is_open_at(pl.id, pl.timezone, $8))
      AND NOT EXISTS (
          SELECT 1 FROM unnest($7::text[]) AS wanted(code)
          WHERE NOT EXISTS (
              SELECT 1 FROM parking_security ps
              WHERE ps.location_id = pl.id
                AND ps.feature_code = wanted.code
                AND ps.state = 1
          )
      )
"#;

/// Browse mode's matching rule: the same criteria as [`FILTERS`], with the
/// radius replaced by the map's own bounding box.
///
/// `&&` (bounding-box overlap) rather than `ST_Intersects`/`ST_DWithin`: for a
/// point against an envelope the two answer the same question, and `&&` is the
/// operator `parking_location_location_gist` indexes, so the viewport query is
/// an index scan over the box instead of a filter over the table.
///
/// Parameters are numbered so the count, the cluster grid and the row page can
/// share the text: `$1..$4` the box (west/south/east/north — `ST_MakeEnvelope`
/// takes x/y, i.e. lon/lat), `$5..$8` the filters, `$9` the instant "open now"
/// is judged at.
const BOUNDS_FILTERS: &str = r#"
    pl.moderation_state = 'ACTIVE'
      AND pl.location && ST_MakeEnvelope($1, $2, $3, $4, 4326)::geography
      AND ($5::text IS NULL OR pl.cost_kind = $5)
      AND ($6::text[] IS NULL OR pl.parking_type = ANY($6))
      AND ($7 = false OR bikesnest_is_open_at(pl.id, pl.timezone, $9))
      AND NOT EXISTS (
          SELECT 1 FROM unnest($8::text[]) AS wanted(code)
          WHERE NOT EXISTS (
              SELECT 1 FROM parking_security ps
              WHERE ps.location_id = pl.id
                AND ps.feature_code = wanted.code
                AND ps.state = 1
          )
      )
"#;

/// Browse mode's origin: the centre of the box, bound as `$10` (lat) / `$11`
/// (lon). A viewport has no destination, so the centre is what distances —
/// displayed and ordered on — are measured from.
const BOUNDS_ORIGIN: &str = "ST_SetSRID(ST_MakePoint($11, $10), 4326)::geography";

/// Count of confirmed security attributes, as a lateral join rather than a
/// correlated subquery: one index lookup per *candidate* row, joined in only
/// for the two sorts whose key needs it.
///
/// `WHERE state = 1` rather than `count(*) FILTER (WHERE state = 1)`: the
/// filtered aggregate has to read every one of a location's eight attribute
/// rows and discard most, while the predicate is answered by
/// `parking_security_yes_idx` (migration 0020) without touching the heap.
const SECURITY_COUNT_JOIN: &str = r#"
    LEFT JOIN LATERAL (
        SELECT count(*) AS yes
        FROM parking_security ps
        WHERE ps.location_id = pl.id AND ps.state = 1
    ) sec ON true
"#;

/// Whole days since the last verification, `NULL` when never verified —
/// `chrono`'s `(now - verified).num_days().max(0)`, which the freshness ladder
/// then compares against the configured thresholds exactly as
/// `bikesnest_domain::categorize` does.
const FRESHNESS_DAYS_JOIN: &str = r#"
    CROSS JOIN LATERAL (
        SELECT GREATEST(
            TRUNC(EXTRACT(EPOCH FROM ($8::timestamptz - pl.last_verified_at)) / 86400.0::float8),
            0.0::float8
        ) AS days
    ) fr
"#;

/// The `Recommended` sort key: `-recommendation_score` (negated, so ascending
/// keyset order is best-first like every other sort).
///
/// This is the documented score, term for term, with the weights bound as `$12..$16`
/// and the freshness thresholds as `$17..$20`. Every sub-score is 0..1 and
/// missing input scores a neutral 0.5 — never the worst value. `parking_test`
/// asserts it against a Rust transcription of the same formula, row by row,
/// across the freshness ladder's boundaries.
const RECOMMENDED_SORT_KEY: &str = r#"-(
                  $12::float8 * (1.0::float8 - LEAST(GREATEST(
                      ST_Distance(pl.location, ST_SetSRID(ST_MakePoint($2, $1), 4326)::geography) / $3::float8,
                      0.0::float8), 1.0::float8))
                + $13::float8 * (CASE WHEN sec.yes > 0
                      THEN LEAST(sec.yes::float8 / 8.0::float8, 1.0::float8)
                      ELSE 0.5::float8 END)
                + $14::float8 * COALESCE(pl.rating_avg::float8 / 5.0::float8, 0.5::float8)
                + $15::float8 * (CASE
                      WHEN pl.last_verified_at IS NULL THEN 0.5::float8
                      WHEN fr.days < $17::float8 THEN 1.0::float8
                      WHEN fr.days < $18::float8 THEN 0.75::float8
                      WHEN fr.days < $19::float8 THEN 0.5::float8
                      WHEN fr.days < $20::float8 THEN 0.25::float8
                      ELSE 0.1::float8 END)
                + $16::float8 * (CASE WHEN pl.last_verified_at IS NULL
                      THEN 0.5::float8 ELSE 1.0::float8 END)
              )"#;

/// The ascending sort key for a sort, plus the joins that key needs. A sort
/// that needs no per-row lookup gets none: an unused lateral would still be
/// executed for every row the sort has to order.
fn sort_key_sql(sort: Sort) -> (String, String) {
    match sort {
        // The GIST index can order on `<->` directly, which is what lets a
        // page be read without touching every row in the radius.
        Sort::Distance => (format!("(pl.location <-> {ORIGIN})::float8"), String::new()),
        Sort::Security => ("-sec.yes::float8".to_string(), SECURITY_COUNT_JOIN.into()),
        // No reviews → the middle of the scale, not the bottom.
        Sort::Rating => (
            "-COALESCE(pl.rating_avg, 2.5)::float8".to_string(),
            String::new(),
        ),
        Sort::RecentlyVerified => (
            "-COALESCE(EXTRACT(EPOCH FROM pl.last_verified_at), 0)::float8".to_string(),
            String::new(),
        ),
        // The score needs both the confirmed-attribute count and the age in days.
        Sort::Recommended => (
            RECOMMENDED_SORT_KEY.to_string(),
            format!("{SECURITY_COUNT_JOIN}{FRESHNESS_DAYS_JOIN}"),
        ),
    }
}

#[async_trait]
impl bikesnest_application::ParkingSearchReader for SqlxParkingSearchReader {
    async fn search(
        &self,
        request: &SearchRequest,
        limit: usize,
        apply_cursor: bool,
    ) -> Result<SearchPage, ReaderError> {
        self.search_at(request, limit, apply_cursor, chrono::Utc::now())
            .await
    }

    async fn in_bounds(&self, query: &BoundsQuery) -> Result<BoundsPage, ReaderError> {
        self.in_bounds_at(query, chrono::Utc::now()).await
    }
}

impl SqlxParkingSearchReader {
    /// `recommendation` and `freshness` are the operator's configured values:
    /// they are bound into the `Recommended` sort key, so this reader — not the
    /// use case above it — is where the recommendation score is applied.
    pub fn new(db: Db, recommendation: RecommendationConfig, freshness: FreshnessConfig) -> Self {
        Self {
            db,
            recommendation,
            freshness,
        }
    }

    /// The search, evaluated as of `now`.
    ///
    /// `now` feeds the "open now" filter and flag, which compare the instant
    /// against each location's own wall clock, and the age in days behind the
    /// recommendation score's freshness term. The trait method passes the
    /// current time; tests pin it so the SQL and the domain rules can be
    /// compared at the same instant.
    pub async fn search_at(
        &self,
        request: &SearchRequest,
        limit: usize,
        apply_cursor: bool,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<SearchPage, ReaderError> {
        let cursor = if apply_cursor { request.cursor } else { None };
        let (cost, types, security_all) = filter_binds(&request.filters);

        let total = self
            .count(request, &cost, &types, &security_all, now)
            .await?;
        let items = self
            .page(request, &cost, &types, &security_all, now, cursor, limit)
            .await?;
        Ok(SearchPage {
            items,
            total,
            next_cursor: None,
        })
    }

    /// Statement (a): how many locations match, with no decoration at all.
    ///
    /// Independent of the page, so it is right even when the page is empty
    /// (a cursor past the end, or a page size larger than the result set).
    async fn count(
        &self,
        request: &SearchRequest,
        cost: &Option<String>,
        types: &Option<Vec<String>>,
        security_all: &Option<Vec<String>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("search.count.acquire", e))?;
        let sql = format!(
            r#"
            SELECT count(*)::bigint AS n
            FROM parking_location pl
            WHERE {FILTERS}
            "#
        );
        let total: (i64,) = sqlx::query_as(&sql)
            .bind(request.origin.lat())
            .bind(request.origin.lon())
            .bind(f64::from(request.radius_m))
            .bind(cost.clone())
            .bind(types.clone())
            .bind(request.filters.open_now)
            .bind(security_all.clone())
            .bind(now)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| reader_err("search.count", e))?;
        Ok(total.0)
    }

    /// Statement (b): the keyset page.
    ///
    /// The CTE picks ids by sort key — cheap columns, one lateral count for
    /// the two sorts that need it — and is `MATERIALIZED` so the planner
    /// cannot inline it back into the decoration below and re-run the laterals
    /// per radius row. Everything expensive (the codes array, the open-now
    /// flag, the primary photo) then runs once per row on the page.
    #[allow(clippy::too_many_arguments)]
    async fn page(
        &self,
        request: &SearchRequest,
        cost: &Option<String>,
        types: &Option<Vec<String>>,
        security_all: &Option<Vec<String>>,
        now: chrono::DateTime<chrono::Utc>,
        cursor: Option<bikesnest_application::Cursor>,
        limit: usize,
    ) -> Result<Vec<ParkingSummary>, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("search.page.acquire", e))?;
        let (sort_key, sort_key_joins) = sort_key_sql(request.sort);
        let sql = format!(
            r#"
            WITH candidates AS MATERIALIZED (
                SELECT c.id, c.sort_key
                FROM (
                    SELECT pl.id AS id, {sort_key} AS sort_key
                    FROM parking_location pl
                    {sort_key_joins}
                    WHERE {FILTERS}
                ) c
                WHERE ($9::float8 IS NULL OR (c.sort_key, c.id) > ($9::float8, $10::int8))
                ORDER BY c.sort_key ASC, c.id ASC
                LIMIT $11
            )
            SELECT
                pl.id,
                pl.name,
                pl.address,
                pl.parking_type,
                pl.cost_kind,
                pl.price_cents,
                pl.price_currency,
                pl.price_unit,
                COALESCE(pl.lat, 0) AS lat,
                COALESCE(pl.lon, 0) AS lon,
                pl.timezone,
                pl.rating_avg::float8 AS rating_avg,
                pl.rating_count,
                pl.last_verified_at,
                ST_Distance(pl.location, {ORIGIN}) AS distance_m,
                codes.security_yes_codes,
                bikesnest_is_open_at(pl.id, pl.timezone, $8) AS is_open_now,
                photo.storage_key AS photo_key,
                c.sort_key
            FROM candidates c
            JOIN parking_location pl ON pl.id = c.id
            LEFT JOIN LATERAL (
                SELECT array_agg(ps.feature_code ORDER BY ps.feature_code) AS security_yes_codes
                FROM parking_security ps
                WHERE ps.location_id = pl.id AND ps.state = 1
            ) codes ON true
            LEFT JOIN LATERAL (
                SELECT ph.storage_key FROM parking_photo ph
                WHERE ph.location_id = pl.id AND ph.moderation_state = 'APPROVED'
                ORDER BY ph.position, ph.id
                LIMIT 1
            ) photo ON true
            ORDER BY c.sort_key ASC, c.id ASC
            "#
        );

        let mut query = sqlx::query_as::<_, SearchRow>(&sql)
            .bind(request.origin.lat())
            .bind(request.origin.lon())
            .bind(f64::from(request.radius_m))
            .bind(cost.clone())
            .bind(types.clone())
            .bind(request.filters.open_now)
            .bind(security_all.clone())
            .bind(now)
            .bind(cursor.map(|c| c.v))
            .bind(cursor.map(|c| c.id))
            .bind(limit as i64);
        // The weights and thresholds appear in the SQL only for the sort that
        // scores, so they are only bound for that sort: Postgres rejects a
        // bind list longer than the statement's parameter list.
        if request.sort == Sort::Recommended {
            let w = &self.recommendation;
            let t = &self.freshness.thresholds;
            query = query
                .bind(w.w_distance)
                .bind(w.w_security)
                .bind(w.w_rating)
                .bind(w.w_freshness)
                .bind(w.w_verification)
                .bind(t.fresh_days as f64)
                .bind(t.recent_days as f64)
                .bind(t.aging_days as f64)
                .bind(t.stale_days as f64);
        }
        let rows: Vec<SearchRow> = query
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| reader_err("search.page", e))?;
        rows.into_iter().map(summary_of).collect()
    }

    /// Browse mode, evaluated as of `now` (the "open now" filter's instant).
    ///
    /// One count first, then *either* the rows or the grid — never both, and
    /// never the rows for a box holding thousands of them: the count is a
    /// bounding-box index scan with no decoration, so it is what decides
    /// whether individual markers are worth reading at all.
    pub async fn in_bounds_at(
        &self,
        query: &BoundsQuery,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<BoundsPage, ReaderError> {
        let (cost, types, security_all) = filter_binds(&query.filters);
        let total = self
            .bounds_count(query, &cost, &types, &security_all, now)
            .await?;
        let total = usize::try_from(total).unwrap_or(0);
        if total > query.limit {
            let clusters = self
                .bounds_clusters(query, &cost, &types, &security_all, now)
                .await?;
            return Ok(BoundsPage {
                items: Vec::new(),
                total,
                clusters,
            });
        }
        let items = self
            .bounds_page(query, &cost, &types, &security_all, now)
            .await?;
        Ok(BoundsPage {
            items,
            total,
            clusters: Vec::new(),
        })
    }

    /// How many locations are inside the box, with no decoration at all.
    async fn bounds_count(
        &self,
        query: &BoundsQuery,
        cost: &Option<String>,
        types: &Option<Vec<String>>,
        security_all: &Option<Vec<String>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<i64, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("search.bounds_count.acquire", e))?;
        let sql = format!(
            r#"
            SELECT count(*)::bigint AS n
            FROM parking_location pl
            WHERE {BOUNDS_FILTERS}
            "#
        );
        let total: (i64,) = sqlx::query_as(&sql)
            .bind(query.west)
            .bind(query.south)
            .bind(query.east)
            .bind(query.north)
            .bind(cost.clone())
            .bind(types.clone())
            .bind(query.filters.open_now)
            .bind(security_all.clone())
            .bind(now)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| reader_err("search.bounds_count", e))?;
        Ok(total.0)
    }

    /// The viewport's rows, nearest the centre first, capped at `query.limit`.
    ///
    /// Same shape as the keyset page above and for the same reason: the ids
    /// and their distances are picked in a `MATERIALIZED` CTE so the codes
    /// array and the primary photo are read once per row that survives the
    /// cap, not once per row in the box.
    async fn bounds_page(
        &self,
        query: &BoundsQuery,
        cost: &Option<String>,
        types: &Option<Vec<String>>,
        security_all: &Option<Vec<String>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<ParkingSummary>, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("search.bounds_page.acquire", e))?;
        let center = query.center();
        let sql = format!(
            r#"
            WITH candidates AS MATERIALIZED (
                SELECT pl.id AS id,
                       ST_Distance(pl.location, {BOUNDS_ORIGIN})::float8 AS distance_m
                FROM parking_location pl
                WHERE {BOUNDS_FILTERS}
                ORDER BY distance_m ASC, pl.id ASC
                LIMIT $12
            )
            SELECT
                pl.id,
                pl.name,
                pl.address,
                pl.parking_type,
                pl.cost_kind,
                pl.price_cents,
                pl.price_currency,
                pl.price_unit,
                COALESCE(pl.lat, 0) AS lat,
                COALESCE(pl.lon, 0) AS lon,
                pl.timezone,
                pl.rating_avg::float8 AS rating_avg,
                pl.rating_count,
                pl.last_verified_at,
                c.distance_m,
                codes.security_yes_codes,
                bikesnest_is_open_at(pl.id, pl.timezone, $9) AS is_open_now,
                photo.storage_key AS photo_key,
                -- Browse is not paginated, so these rows carry no keyset key:
                -- a cursor minted from one would page through a viewport that
                -- no longer exists by the time it is followed.
                NULL::float8 AS sort_key
            FROM candidates c
            JOIN parking_location pl ON pl.id = c.id
            LEFT JOIN LATERAL (
                SELECT array_agg(ps.feature_code ORDER BY ps.feature_code) AS security_yes_codes
                FROM parking_security ps
                WHERE ps.location_id = pl.id AND ps.state = 1
            ) codes ON true
            LEFT JOIN LATERAL (
                SELECT ph.storage_key FROM parking_photo ph
                WHERE ph.location_id = pl.id AND ph.moderation_state = 'APPROVED'
                ORDER BY ph.position, ph.id
                LIMIT 1
            ) photo ON true
            ORDER BY c.distance_m ASC, c.id ASC
            "#
        );
        let rows: Vec<SearchRow> = sqlx::query_as(&sql)
            .bind(query.west)
            .bind(query.south)
            .bind(query.east)
            .bind(query.north)
            .bind(cost.clone())
            .bind(types.clone())
            .bind(query.filters.open_now)
            .bind(security_all.clone())
            .bind(now)
            .bind(center.lat())
            .bind(center.lon())
            .bind(query.limit as i64)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| reader_err("search.bounds_page", e))?;
        rows.into_iter().map(summary_of).collect()
    }

    /// The same rows as counts on a grid, for a box too full to draw.
    ///
    /// The grid key is `ST_SnapToGrid` at [`BoundsQuery::cell_deg`] (the box's
    /// width over twelve), so the cluster map is a ~12-column grid at every
    /// zoom. The marker sits at the *mean* position of the cell's members
    /// rather than the cell's corner, which is what keeps a cluster over the
    /// spots it stands for; the counts sum to the count above.
    async fn bounds_clusters(
        &self,
        query: &BoundsQuery,
        cost: &Option<String>,
        types: &Option<Vec<String>>,
        security_all: &Option<Vec<String>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<Vec<Cluster>, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("search.bounds_clusters.acquire", e))?;
        let sql = format!(
            r#"
            SELECT
                avg(ST_Y(g.geom))::float8 AS lat,
                avg(ST_X(g.geom))::float8 AS lon,
                count(*)::bigint AS n
            FROM (
                SELECT
                    pl.location::geometry AS geom,
                    ST_SnapToGrid(pl.location::geometry, $10::float8) AS cell
                FROM parking_location pl
                WHERE {BOUNDS_FILTERS}
            ) g
            GROUP BY g.cell
            ORDER BY n DESC, lat ASC, lon ASC
            "#
        );
        let rows: Vec<ClusterRow> = sqlx::query_as(&sql)
            .bind(query.west)
            .bind(query.south)
            .bind(query.east)
            .bind(query.north)
            .bind(cost.clone())
            .bind(types.clone())
            .bind(query.filters.open_now)
            .bind(security_all.clone())
            .bind(now)
            .bind(query.cell_deg())
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| reader_err("search.bounds_clusters", e))?;
        Ok(rows
            .into_iter()
            .map(|r| Cluster {
                lat: r.lat.unwrap_or(0.0),
                lon: r.lon.unwrap_or(0.0),
                count: r.n,
            })
            .collect())
    }
}

/// The three filter binds every statement in this file shares, in the order
/// their placeholders appear: the cost kind, the wanted type codes, and the
/// security codes that must all be confirmed. `None` means "no such filter" —
/// each fragment tests for `NULL` rather than building different SQL, so the
/// count, the page and the cluster grid are one statement text each.
fn filter_binds(
    filters: &bikesnest_application::Filters,
) -> (Option<String>, Option<Vec<String>>, Option<Vec<String>>) {
    let cost = filters.cost.map(|c: CostFilter| match c {
        CostFilter::Free => "free".to_string(),
        CostFilter::Paid => "paid".to_string(),
        CostFilter::Unknown => "unknown".to_string(),
    });
    let types = (!filters.types.is_empty()).then(|| {
        filters
            .types
            .iter()
            .map(|t| t.as_code().to_string())
            .collect()
    });
    let security_all = (!filters.security_all.is_empty()).then(|| filters.security_all.clone());
    (cost, types, security_all)
}

#[derive(sqlx::FromRow)]
struct ClusterRow {
    lat: Option<f64>,
    lon: Option<f64>,
    n: i64,
}

/// Classify + log the sqlx error, then map it onto [`ReaderError`]. Shared by
/// every read-only repository. `context` names the operation, e.g. `"search.page"`.
pub(crate) fn reader_err(context: &'static str, e: sqlx::Error) -> ReaderError {
    crate::db_error::classify_and_log(context, e).into()
}
