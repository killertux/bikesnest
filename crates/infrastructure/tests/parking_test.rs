//! Real-PostgreSQL parking-reader tests with rollback-scoped fixtures.
//! Builders, direct SQL and readers share one injected Db; repository
//! savepoint commits never publish fixture data to another test.

use bikesnest_application::{
    BoundsPage, BoundsQuery, CostFilter, Cursor, Filters, ParkingDetailsReader, ParkingSummary,
    ReaderError, SearchInput, SearchPage, SearchParking, SearchRequest, SitemapReader, Sort,
};
use bikesnest_domain::{Cost, CurrencyCode, Money, ParkingType, PricingUnit};
use bikesnest_infrastructure::Db;
use bikesnest_test_support::{ParkingBuilder, db_test};

/// Base origin: far from any seed data (Serra da Cantareira area).
const ORIGIN: (f64, f64) = (-23.400_000, -46.600_000);

/// Geographically separated scenarios also avoid matching unrelated baseline
/// rows; transaction rollback provides fixture isolation and cleanup.
fn test_origin(k: f64) -> bikesnest_domain::GeoPoint {
    bikesnest_domain::GeoPoint::new(ORIGIN.0 + k * 0.05, ORIGIN.1).unwrap()
}

/// Place a builder `meters` north of test patch `k`'s origin.
fn at(k: f64, meters: f64) -> ParkingBuilder {
    let o = test_origin(k);
    ParkingBuilder::new().at(o.lat() + meters / 111_320.0, o.lon())
}

fn request_at(k: f64, sort: Sort) -> SearchRequest {
    SearchRequest::new(
        test_origin(k),
        Some("test origin".to_string()),
        1000,
        Default::default(),
        sort,
        20,
        None,
    )
}

/// A fixed instant (noon in São Paulo — clear of every fixture's hour
/// boundaries in this file) so `real_search` results never depend on the
/// wall-clock time the suite happens to run at. Tests that specifically
/// exercise "open now" behavior use `search_at`/`sql_open_now` with their own
/// pinned instants instead.
fn fixed_now() -> chrono::DateTime<chrono::Utc> {
    instant_at("America/Sao_Paulo", 2026, 3, 10, 12, 0)
}

/// The search reader under test, carrying the documented default weights and
/// freshness thresholds — the same values production loads from the
/// environment, so the SQL sort key the tests assert on is the shipped one.
fn reader(db: bikesnest_infrastructure::Db) -> bikesnest_infrastructure::SqlxParkingSearchReader {
    bikesnest_infrastructure::SqlxParkingSearchReader::new(
        db,
        bikesnest_application::DEFAULT_RECOMMENDATION_CONFIG,
        Default::default(),
    )
}

async fn real_search(
    db: &Db,
    request: &SearchRequest,
    limit: usize,
    apply_cursor: bool,
) -> Result<SearchPage, ReaderError> {
    reader(db.clone())
        .search_at(request, limit, apply_cursor, fixed_now())
        .await
}

/// A browse box of ±`half` degrees around test patch `k`'s origin — so the
/// box's centre *is* the patch origin and a fixture's distance from it is the
/// distance the fixture was placed at.
fn bounds_at(k: f64, half: f64, limit: usize) -> BoundsQuery {
    bounds_with(k, half, limit, Filters::default())
}

/// [`bounds_at`] with filters. A `BoundsQuery` is only ever built by `parse`,
/// so the filters go in there too.
fn bounds_with(k: f64, half: f64, limit: usize, filters: Filters) -> BoundsQuery {
    let o = test_origin(k);
    BoundsQuery::parse(
        &format!(
            "{},{},{},{}",
            o.lon() - half,
            o.lat() - half,
            o.lon() + half,
            o.lat() + half
        ),
        filters,
        limit,
    )
    .expect("a valid test box")
}

async fn real_bounds(db: &Db, query: &BoundsQuery) -> Result<BoundsPage, ReaderError> {
    reader(db.clone()).in_bounds_at(query, fixed_now()).await
}

async fn real_details(
    db: &Db,
    id: i64,
) -> Result<Option<bikesnest_domain::ParkingLocation>, ReaderError> {
    bikesnest_infrastructure::SqlxParkingDetailsReader::new(db.clone())
        .details(id)
        .await
}

#[db_test]
async fn within_radius_ordered_by_distance_with_correct_total(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    at(1.0, 100.0)
        .with_name("A 100m")
        .create(&mut conn)
        .await
        .unwrap();
    at(1.0, 300.0)
        .with_name("B 300m")
        .create(&mut conn)
        .await
        .unwrap();
    at(1.0, 2000.0)
        .with_name("C 2km")
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    let mut req = request_at(1.0, Sort::Distance);
    req.radius_m = 500;
    let page = real_search(&db, &req, 20, false).await.unwrap();
    assert_eq!(page.items.len(), 2, "2km location is outside the radius");
    assert_eq!(page.total, 2);
    assert_eq!(page.items[0].name, "A 100m");
    assert_eq!(page.items[1].name, "B 300m");
    assert!(page.items[0].distance_m < page.items[1].distance_m);
}

#[db_test]
async fn radius_filter_excludes_far_locations(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    at(2.0, 50.0).create(&mut conn).await.unwrap();
    at(2.0, 1500.0).create(&mut conn).await.unwrap();
    drop(conn);

    let mut req = request_at(2.0, Sort::Distance);
    req.radius_m = 250;
    let page = real_search(&db, &req, 20, false).await.unwrap();
    assert_eq!(page.total, 1);
    assert!(page.items[0].distance_m < 60.0);
}

#[db_test]
async fn cost_type_and_security_filters_apply(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    at(3.0, 30.0)
        .with_type(ParkingType::Rack)
        .with_security("cctv", 1)
        .create(&mut conn)
        .await
        .unwrap();
    at(3.0, 60.0)
        .with_cost(Cost::Paid {
            price: Some(Money::new(
                500,
                CurrencyCode::parse("BRL").unwrap(),
                PricingUnit::Day,
            )),
        })
        .with_type(ParkingType::Locker)
        .create(&mut conn)
        .await
        .unwrap();
    at(3.0, 90.0)
        .with_type(ParkingType::Rack)
        .with_cost(Cost::Unknown)
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    let mut req = request_at(3.0, Sort::Distance);
    req.radius_m = 250;

    // Cost filter: free only.
    req.filters.cost = Some(CostFilter::Free);
    let page = real_search(&db, &req, 20, false).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].cost.kind_code(), "free");

    // Type filter: rack only (free + unknown-cost racks).
    req.filters.cost = None;
    req.filters.types = vec![ParkingType::Rack];
    let page = real_search(&db, &req, 20, false).await.unwrap();
    assert_eq!(page.total, 2);

    // Security all-of: cctv=yes required.
    req.filters.types.clear();
    req.filters.security_all = vec!["cctv".to_string()];
    let page = real_search(&db, &req, 20, false).await.unwrap();
    assert_eq!(page.total, 1);
    assert!(page.items[0].security_yes.contains(&"cctv".to_string()));

    // Security all-of with two required features → none match.
    req.filters.security_all = vec!["cctv".to_string(), "security_guard".to_string()];
    let page = real_search(&db, &req, 20, false).await.unwrap();
    assert_eq!(page.total, 0);
}

#[db_test]
async fn open_now_filter_agrees_with_domain_for_all_day_hours(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let all_day = at(4.0, 30.0)
        .with_all_day_hours(1..=7)
        .create(&mut conn)
        .await
        .unwrap();
    let narrow = at(4.0, 60.0)
        .with_hours(1..=7, (3, 0), (4, 0)) // open only 03:00–04:00 local
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    // Pinned to noon in São Paulo (Tue 2026-03-10) — clear of the narrow
    // 03:00-04:00 window regardless of when the suite itself runs, and
    // checked against the domain rule directly (not just the SQL flag).
    let now = instant_at("America/Sao_Paulo", 2026, 3, 10, 12, 0);
    assert!(
        domain_open_now(&all_day, now),
        "domain: all-day location is open at noon"
    );
    assert!(
        !domain_open_now(&narrow, now),
        "domain: narrow 03:00-04:00 window is closed at noon"
    );

    let mut req = request_at(4.0, Sort::Distance);
    req.filters.open_now = true;
    let page = search_at(&db, &req, now).await.unwrap();
    assert_eq!(page.total, 1);
    assert!(
        page.items[0].is_open_now,
        "all-day location must be open now"
    );
}

#[db_test]
async fn non_active_locations_are_hidden_from_search(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    at(5.0, 30.0)
        .with_moderation_state("INVALID")
        .create(&mut conn)
        .await
        .unwrap();
    at(5.0, 60.0).create(&mut conn).await.unwrap();
    drop(conn);

    let page = real_search(&db, &request_at(5.0, Sort::Distance), 20, false)
        .await
        .unwrap();
    assert_eq!(page.total, 1);
}

#[db_test]
async fn the_sitemap_reader_lists_only_active_ids(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let active = at(5.5, 30.0).create(&mut conn).await.unwrap();
    let invalid = at(5.5, 60.0)
        .with_moderation_state("INVALID")
        .create(&mut conn)
        .await
        .unwrap();
    let pending = at(5.5, 90.0)
        .with_moderation_state("PENDING_REVIEW")
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    let ids = bikesnest_infrastructure::SqlxSitemapReader::new(db)
        .active_parking_ids()
        .await
        .unwrap();

    assert!(ids.contains(&active.id()), "an ACTIVE location is listed");
    assert!(
        !ids.contains(&invalid.id()) && !ids.contains(&pending.id()),
        "a non-ACTIVE location is never listed"
    );
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "the order is stable (by id)");
}

#[db_test]
async fn keyset_pagination_is_stable_across_inserts(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    for m in 1..=25 {
        at(6.0, m as f64 * 10.0).create(&mut conn).await.unwrap();
    }
    drop(conn);

    let mut req = request_at(6.0, Sort::Distance);
    let mut seen: Vec<i64> = Vec::new();
    for _ in 0..8 {
        let page = real_search(&db, &req, 5, true).await.unwrap(); // page of 4 + lookahead
        for item in &page.items {
            assert!(!seen.contains(&item.id), "duplicate id across pages");
            seen.push(item.id);
        }
        if page.items.len() <= 4 {
            break;
        }
        let last = page.items.last().unwrap();
        req.cursor = Some(Cursor {
            sort: Sort::Distance,
            // The key the query itself computed. `distance_m` is the spheroid
            // distance the card displays; the distance sort orders on the
            // sphere distance the GIST index can supply, and they differ by a
            // fraction of a percent — enough, at this spacing, to repeat a row
            // across the page boundary if the cursor were recomputed here.
            v: last
                .sort_key
                .expect("the reader always returns its sort key"),
            id: last.id,
        });
    }
    assert_eq!(seen.len(), 25, "all 25 items paginated exactly once");
}

#[db_test]
async fn rating_and_recently_verified_sorts_work(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    at(7.0, 30.0)
        .with_rating(3.0, 2)
        .create(&mut conn)
        .await
        .unwrap();
    at(7.0, 60.0)
        .with_rating(4.9, 10)
        .create(&mut conn)
        .await
        .unwrap();
    at(7.0, 90.0)
        .never_verified()
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    let page = real_search(&db, &request_at(7.0, Sort::Rating), 20, false)
        .await
        .unwrap();
    assert!(page.items[0].rating.avg().unwrap() > page.items[1].rating.avg().unwrap());

    // Recently verified: never-verified item sorts last (key 0).
    let page = real_search(&db, &request_at(7.0, Sort::RecentlyVerified), 20, false)
        .await
        .unwrap();
    assert!(page.items[2].last_verified_at.is_none());
}

#[db_test]
async fn details_assemble_the_full_aggregate(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let created = at(8.0, 30.0)
        .with_name("Detalhe Completo")
        .with_cost(Cost::Paid {
            price: Some(Money::new(
                500,
                CurrencyCode::parse("BRL").unwrap(),
                PricingUnit::Day,
            )),
        })
        .with_type(ParkingType::Secured)
        .with_hours(1..=5, (8, 0), (18, 0))
        .with_security("cctv", 1)
        .with_security("indoor", 2) // explicitly no
        .with_security("staffed", 0) // explicitly unknown
        .with_rating(4.2, 5)
        .verified_days_ago(100)
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    let details = real_details(&db, created.id())
        .await
        .unwrap()
        .expect("found");
    assert_eq!(details.name(), "Detalhe Completo");
    assert_eq!(details.parking_type(), ParkingType::Secured);
    match details.cost() {
        Cost::Paid { price: Some(m) } => {
            assert_eq!(m.cents(), 500);
            assert_eq!(m.currency().as_str(), "BRL");
            assert_eq!(m.unit(), PricingUnit::Day);
        }
        other => panic!("expected paid cost, got {other:?}"),
    }
    assert_eq!(details.rating().avg(), Some(4.2));
    assert_eq!(details.rating().count(), 5);
    assert_eq!(
        details.hours(),
        &bikesnest_domain::OpeningHours::weekly(
            (1u8..=5)
                .map(|d| {
                    (
                        d,
                        bikesnest_domain::TimeRange::new(
                            bikesnest_domain::hours::hms(8, 0),
                            bikesnest_domain::hours::hms(18, 0),
                        ),
                    )
                })
                .collect()
        )
    );
    let by_code = |c: &str| details.security().iter().find(|f| f.code() == c).unwrap();
    assert_eq!(
        by_code("cctv").state(),
        bikesnest_domain::SecurityState::Yes
    );
    assert_eq!(
        by_code("indoor").state(),
        bikesnest_domain::SecurityState::No
    );
    assert_eq!(
        by_code("staffed").state(),
        bikesnest_domain::SecurityState::Unknown
    );
    // Labels are localized in the presentation layer, not stored — only the
    // code round-trips through the reader.
    assert_eq!(by_code("cctv").code(), "cctv");
    assert_eq!(
        details.security().len(),
        8,
        "every catalog feature recorded"
    );
    assert_eq!(details.security_yes_count(), 1);
}

#[db_test]
async fn unknown_hours_map_to_opening_hours_unknown(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let created = at(9.0, 30.0)
        .with_unknown_hours()
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let details = real_details(&db, created.id()).await.unwrap().unwrap();
    assert!(details.hours().is_unknown());
}

#[db_test]
async fn details_of_missing_id_is_none(tx: &mut TestTx) {
    let db = tx.db().await;
    assert!(real_details(&db, 987_654_321).await.unwrap().is_none());
}

#[db_test]
async fn invalid_type_code_is_reported_not_silently_mapped(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let id: (i64,) = sqlx::query_as(
        "INSERT INTO parking_location (name, address, parking_type, cost_kind, location, timezone) VALUES ('x','y','flying_carpet','free', ST_SetSRID(ST_MakePoint(-46.6,-23.4),4326)::geography, 'America/Sao_Paulo') RETURNING id",
    )
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    drop(conn);
    let _ = test_origin(10.0);

    let err = real_details(&db, id.0).await.unwrap_err();
    assert!(matches!(err, ReaderError::Unexpected(msg) if msg.contains("flying_carpet")));
}

// ---------------------------------------------------------------------------
// "Open now": the SQL flag must agree with the domain's opening-hours rule
// ---------------------------------------------------------------------------

/// The search evaluated at a pinned instant, so the SQL wall-clock arithmetic
/// and `OpeningHours::status_at` can be compared on the same input.
async fn search_at(
    db: &Db,
    request: &SearchRequest,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<SearchPage, ReaderError> {
    reader(db.clone()).search_at(request, 20, false, now).await
}

/// `is_open_now` for the single fixture living in test patch `k`.
async fn sql_open_now(db: &Db, k: f64, now: chrono::DateTime<chrono::Utc>) -> bool {
    let page = search_at(db, &request_at(k, Sort::Distance), now)
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1, "patch {k} holds exactly one fixture");
    page.items[0].is_open_now
}

fn domain_open_now(
    location: &bikesnest_domain::ParkingLocation,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    location.hours().status_at(now, location.timezone()) == bikesnest_domain::OpenStatus::Open
}

/// A UTC instant from a wall-clock reading in `tz`.
fn instant_at(tz: &str, y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    let tz: chrono_tz::Tz = tz.parse().expect("valid timezone");
    tz.with_ymd_and_hms(y, mo, d, h, mi, 0)
        .single()
        .expect("unambiguous local time")
        .with_timezone(&chrono::Utc)
}

fn utc_at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(y, mo, d, h, mi, 0)
        .single()
        .expect("valid instant")
}

#[db_test]
async fn open_now_flag_matches_the_domain_on_a_same_day_range(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let spot = at(11.0, 30.0)
        .with_hours(1..=7, (8, 0), (18, 0))
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    // Tue 2026-03-10, São Paulo (UTC-3). Both edges of the range, both sides.
    for (h, min, expected) in [(7, 59, false), (8, 0, true), (17, 59, true), (18, 0, false)] {
        let now = instant_at("America/Sao_Paulo", 2026, 3, 10, h, min);
        assert_eq!(
            sql_open_now(&db, 11.0, now).await,
            expected,
            "SQL at {h:02}:{min:02} local"
        );
        assert_eq!(
            domain_open_now(&spot, now),
            expected,
            "domain at {h:02}:{min:02} local"
        );
    }
}

#[db_test]
async fn open_now_flag_matches_the_domain_across_an_overnight_range(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    // 22:00 → 02:00: the row belongs to one day but runs into the next.
    let spot = at(12.0, 30.0)
        .with_hours(1..=7, (22, 0), (2, 0))
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    for (h, expected) in [(21, false), (23, true), (1, true), (3, false)] {
        let now = instant_at("America/Sao_Paulo", 2026, 3, 10, h, 0);
        assert_eq!(
            sql_open_now(&db, 12.0, now).await,
            expected,
            "SQL at {h:02}:00"
        );
        assert_eq!(domain_open_now(&spot, now), expected, "domain at {h:02}:00");
    }
}

#[db_test]
async fn open_now_flag_matches_the_domain_across_a_dst_transition(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    // New York springs forward 2026-03-08 at 02:00 local (07:00 UTC).
    let spot = at(13.0, 30.0)
        .with_timezone("America/New_York")
        .with_hours(1..=7, (3, 0), (6, 0))
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    // 06:30 UTC is still EST → 01:30 local, before opening.
    let before = utc_at(2026, 3, 8, 6, 30);
    assert!(
        !sql_open_now(&db, 13.0, before).await,
        "SQL before the transition"
    );
    assert!(
        !domain_open_now(&spot, before),
        "domain before the transition"
    );

    // 07:30 UTC is EDT → 03:30 local, inside the range.
    let after = utc_at(2026, 3, 8, 7, 30);
    assert!(
        sql_open_now(&db, 13.0, after).await,
        "SQL after the transition"
    );
    assert!(domain_open_now(&spot, after), "domain after the transition");
}

#[db_test]
async fn open_now_filter_uses_the_same_rule_as_the_flag(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    at(14.0, 30.0)
        .with_name("Overnight")
        .with_hours(1..=7, (22, 0), (2, 0))
        .create(&mut conn)
        .await
        .unwrap();
    at(14.0, 60.0)
        .with_name("Daytime")
        .with_hours(1..=7, (8, 0), (18, 0))
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    let mut req = request_at(14.0, Sort::Distance);
    req.filters.open_now = true;
    // 23:00 in São Paulo: only the overnight location is open.
    let now = instant_at("America/Sao_Paulo", 2026, 3, 10, 23, 0);
    let page = search_at(&db, &req, now).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].name, "Overnight");
    assert!(page.items[0].is_open_now);
}

// ---------------------------------------------------------------------------
// Keyset pagination through the use case (cursor built from the SQL sort key)
// ---------------------------------------------------------------------------

/// The real use case over the real reader: it is the use case that turns a
/// row's sort key into the next cursor, so the round trip is what matters.
fn search_use_case(db: &Db) -> SearchParking {
    SearchParking::new(
        Box::new(bikesnest_infrastructure::FakeGeocoder),
        Box::new(reader(db.clone())),
    )
}

async fn page_of(db: &Db, k: f64, sort: &str, cursor: Option<String>) -> SearchPage {
    let origin = test_origin(k);
    let (page, _) = search_use_case(db)
        .execute(SearchInput {
            lat: Some(origin.lat()),
            lon: Some(origin.lon()),
            radius_m: Some(1000),
            sort: Some(sort.to_string()),
            page_size: Some(20),
            cursor,
            ..Default::default()
        })
        .await
        .expect("search succeeds");
    page
}

/// Every sort code the search understands, so a new one cannot quietly skip
/// the pagination tests.
const SORTS: [&str; 5] = [
    "recommended",
    "distance",
    "security",
    "rating",
    "recently_verified",
];

/// Walks both pages of a 25-row patch and returns the rows in page order,
/// asserting the page shapes and that the two pages are disjoint.
async fn both_pages(db: &Db, k: f64, sort: &str) -> Vec<ParkingSummary> {
    let mut first = page_of(db, k, sort, None).await;
    assert_eq!(first.items.len(), 20, "{sort}: full first page");
    assert_eq!(first.total, 25, "{sort}: total counts every match");
    let cursor = first
        .next_cursor
        .unwrap_or_else(|| panic!("{sort}: a second page exists"));

    let second = page_of(db, k, sort, Some(cursor.encode())).await;
    assert_eq!(
        second.items.len(),
        5,
        "{sort}: remainder on the second page"
    );
    assert!(
        second.next_cursor.is_none(),
        "{sort}: nothing left after the second page"
    );

    first.items.extend(second.items);
    let mut unique: Vec<i64> = first.items.iter().map(|i| i.id).collect();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), 25, "{sort}: no row appears on both pages");
    first.items
}

#[db_test]
async fn recently_verified_pages_advance_instead_of_repeating(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    for m in 1..=25 {
        at(15.0, f64::from(m) * 10.0)
            .verified_days_ago(i64::from(m))
            .create(&mut conn)
            .await
            .unwrap();
    }
    drop(conn);

    let by_verification = both_pages(&db, 15.0, "recently_verified").await;
    let verified: Vec<_> = by_verification.iter().map(|i| i.last_verified_at).collect();
    assert!(
        verified.windows(2).all(|w| w[0] >= w[1]),
        "verification timestamps descend across the page boundary"
    );

    // The distance sort paginates over the same fixture.
    let by_distance = both_pages(&db, 15.0, "distance").await;
    assert!(
        by_distance
            .windows(2)
            .all(|w| w[0].distance_m <= w[1].distance_m),
        "distances ascend across the page boundary"
    );
}

/// The five sorts, on one 25-row patch: two pages each, disjoint, strictly
/// ordered by the key the query itself computed, and reproducible.
///
/// `Recommended` is in this list because it is now a SQL sort like the others.
/// It used to fetch a 500-row candidate set and re-rank it in memory, which
/// meant its pages were only as correct as that cap and its cursor was a score
/// no query had produced.
#[db_test]
async fn every_sort_pages_disjointly_and_deterministically(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    for m in 1..=25u32 {
        let mut spot = at(16.0, f64::from(m) * 10.0)
            .with_name(format!("Spot {m:02}"))
            .verified_days_ago(i64::from(m) * 7)
            .with_rating(f64::from(1 + m % 5), i64::from(m));
        // Deliberately few distinct values per key, so every sort has ties for
        // the id tiebreak to resolve.
        for code in ["cctv", "well_lit", "indoor"].iter().take((m % 4) as usize) {
            spot = spot.with_security(code, 1);
        }
        spot.create(&mut conn).await.unwrap();
    }
    drop(conn);

    for sort in SORTS {
        let rows = both_pages(&db, 16.0, sort).await;
        let keys: Vec<(f64, i64)> = rows
            .iter()
            .map(|r| (r.sort_key.expect("every sort carries its key"), r.id))
            .collect();
        assert!(
            keys.windows(2).all(|w| w[0] < w[1]),
            "{sort}: (key, id) strictly ascends across the page boundary"
        );

        let again = page_of(&db, 16.0, sort, None).await;
        assert_eq!(
            again.items.iter().map(|i| i.id).collect::<Vec<_>>(),
            rows[..20].iter().map(|i| i.id).collect::<Vec<_>>(),
            "{sort}: the same request twice is the same page"
        );

        // A page past the end. The count is its own statement now, so it is
        // still the truth here; it used to ride on the returned rows and read 0.
        let beyond = Cursor {
            sort: Sort::from_code(sort).unwrap(),
            v: f64::MAX,
            id: i64::MAX,
        };
        let empty = page_of(&db, 16.0, sort, Some(beyond.encode())).await;
        assert!(empty.items.is_empty(), "{sort}: nothing past the end");
        assert_eq!(
            empty.total, 25,
            "{sort}: the total does not ride on the rows"
        );
        assert!(empty.next_cursor.is_none());
    }
}

/// A security code no catalog knows is dropped rather than matched, so the
/// search degrades to the filters that still mean something instead of
/// returning nothing.
#[db_test]
async fn an_unknown_security_filter_code_is_ignored(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    at(17.0, 30.0)
        .with_security("cctv", 1)
        .create(&mut conn)
        .await
        .unwrap();
    at(17.0, 60.0).create(&mut conn).await.unwrap();
    drop(conn);

    let request = |codes: Vec<&str>| {
        SearchRequest::new(
            test_origin(17.0),
            None,
            1000,
            Filters {
                security_all: codes.into_iter().map(str::to_string).collect(),
                ..Filters::default()
            },
            Sort::Distance,
            20,
            None,
        )
    };
    let unfiltered = real_search(&db, &request(vec![]), 20, false).await.unwrap();
    let unknown_only = real_search(&db, &request(vec!["laser_fence"]), 20, false)
        .await
        .unwrap();
    assert_eq!(unknown_only.total, unfiltered.total);
    assert_eq!(
        unknown_only.items.iter().map(|i| i.id).collect::<Vec<_>>(),
        unfiltered.items.iter().map(|i| i.id).collect::<Vec<_>>(),
    );

    // A known code alongside it still applies.
    let mixed = real_search(&db, &request(vec!["laser_fence", "cctv"]), 20, false)
        .await
        .unwrap();
    assert_eq!(mixed.total, 1);
}

/// The recommendation score, transcribed from the SQL sort key back into
/// Rust. The point of the test below is that these two never drift: the score
/// lives in one place (the query), and this is the independent reading of it.
fn recommendation_score(
    item: &ParkingSummary,
    radius_m: u32,
    now: chrono::DateTime<chrono::Utc>,
    weights: &bikesnest_application::RecommendationConfig,
    thresholds: &bikesnest_domain::FreshnessThresholds,
) -> f64 {
    let distance_score = 1.0 - (item.distance_m / f64::from(radius_m)).clamp(0.0, 1.0);

    let yes = item.security_yes.len() as f64;
    let security_score = if yes > 0.0 { (yes / 8.0).min(1.0) } else { 0.5 };

    let rating_score = item.rating.avg().map(|a| a / 5.0).unwrap_or(0.5);

    let freshness_score = match bikesnest_domain::categorize(item.last_verified_at, now, thresholds)
    {
        bikesnest_domain::FreshnessCategory::Fresh => 1.0,
        bikesnest_domain::FreshnessCategory::RecentlyVerified => 0.75,
        bikesnest_domain::FreshnessCategory::Aging => 0.5,
        bikesnest_domain::FreshnessCategory::Stale => 0.25,
        bikesnest_domain::FreshnessCategory::VeryStale => 0.1,
        bikesnest_domain::FreshnessCategory::Never => 0.5,
    };

    let verification_score = if item.last_verified_at.is_some() {
        1.0
    } else {
        0.5
    };

    weights.w_distance * distance_score
        + weights.w_security * security_score
        + weights.w_rating * rating_score
        + weights.w_freshness * freshness_score
        + weights.w_verification * verification_score
}

/// The SQL sort key for `Recommended` is exactly `-recommendation_score`, on
/// both sides of every freshness threshold.
///
/// The ladder's boundaries are where a translation goes wrong: the domain
/// compares *whole days* with `<`, so a location verified exactly 30 days ago
/// is "recently verified", not "fresh". Each row below is pinned to an exact
/// day offset from the instant the search is evaluated at, so the comparison
/// lands on the boundary rather than near it.
#[db_test]
async fn the_recommended_sort_key_is_the_documented_score(tx: &mut TestTx) {
    let db = tx.db().await;
    let now = instant_at("America/Sao_Paulo", 2026, 3, 10, 12, 0);
    let thresholds = bikesnest_domain::DEFAULT_THRESHOLDS;
    let ages: [Option<i64>; 10] = [
        None,
        Some(0),
        Some(thresholds.fresh_days - 1),
        Some(thresholds.fresh_days),
        Some(thresholds.recent_days - 1),
        Some(thresholds.recent_days),
        Some(thresholds.aging_days - 1),
        Some(thresholds.aging_days),
        Some(thresholds.stale_days - 1),
        Some(thresholds.stale_days),
    ];

    let mut conn = db.acquire().await.unwrap();
    let mut ids = Vec::new();
    for (i, age) in ages.iter().enumerate() {
        let mut spot = at(18.0, 30.0 + i as f64 * 40.0).with_name(format!("Score {i}"));
        // Vary the other three terms too, including their neutral defaults:
        // no rating and no confirmed attributes must score 0.5, not 0.
        if i % 3 == 1 {
            spot = spot.with_rating(4.25, 4);
        }
        for code in bikesnest_domain::SECURITY_FEATURE_CODES.iter().take(i) {
            spot = spot.with_security(code, 1);
        }
        spot = match age {
            None => spot.never_verified(),
            Some(days) => spot.verified_days_ago(*days),
        };
        let created = spot.create(&mut conn).await.unwrap();
        ids.push(created.id());
        // The builder dates verification from wall-clock `now()`; pin it to an
        // exact offset from the instant this search is evaluated at, or the
        // boundary rows land a few hours off it.
        if let Some(days) = age {
            sqlx::query(
                "UPDATE parking_location SET last_verified_at = $2 - make_interval(days => $3) WHERE id = $1",
            )
            .bind(created.id())
            .bind(now)
            .bind(i32::try_from(*days).unwrap())
            .execute(&mut *conn)
            .await
            .unwrap();
        }
    }
    drop(conn);

    let page = search_at(&db, &request_at(18.0, Sort::Recommended), now)
        .await
        .unwrap();
    assert_eq!(page.items.len(), ages.len(), "every fixture is in range");

    let weights = bikesnest_application::DEFAULT_RECOMMENDATION_CONFIG;
    for item in &page.items {
        let expected = -recommendation_score(item, 1000, now, &weights, &thresholds);
        let actual = item.sort_key.expect("recommended carries its key");
        assert!(
            (actual - expected).abs() < 1e-9,
            "{}: SQL key {actual} vs Rust score {expected} (verified {:?}, {} confirmed, rating {:?})",
            item.name,
            item.last_verified_at,
            item.security_yes.len(),
            item.rating.avg(),
        );
    }
    let keys: Vec<f64> = page.items.iter().map(|i| i.sort_key.unwrap()).collect();
    assert!(
        keys.windows(2).all(|w| w[0] <= w[1]),
        "the best score comes first: the key is the negated score"
    );
}

/// `bikesnest_is_open_at` (migration 0020) is the SQL half of
/// `OpeningHours::status_at`, so the two must answer the same question at the
/// same instant. "Open now" is one implementation with two callers, not two
/// implementations — this is what keeps it that way.
#[db_test]
async fn the_open_now_function_agrees_with_the_domain(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();

    // (label, fixture) — each shape the rule has an arm for.
    let same_day = at(19.0, 30.0)
        .with_hours(1..=7, (8, 0), (18, 0))
        .create(&mut conn)
        .await
        .unwrap();
    let overnight = at(19.0, 60.0)
        .with_hours(1..=7, (22, 0), (2, 0))
        .create(&mut conn)
        .await
        .unwrap();
    let all_day = at(19.0, 90.0)
        .with_all_day_hours(1..=7)
        .create(&mut conn)
        .await
        .unwrap();
    let weekdays_only = at(19.0, 120.0)
        .with_hours(1..=5, (9, 0), (17, 0))
        .create(&mut conn)
        .await
        .unwrap();
    let no_hours = at(19.0, 150.0)
        .with_unknown_hours()
        .create(&mut conn)
        .await
        .unwrap();
    let dst = at(19.0, 180.0)
        .with_timezone("America/New_York")
        .with_hours(1..=7, (3, 0), (6, 0))
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    // Tuesday 2026-03-10 in São Paulo, Sunday 2026-03-08 around the US
    // spring-forward instant (02:00 EST → 03:00 EDT).
    let sp = |h, mi| instant_at("America/Sao_Paulo", 2026, 3, 10, h, mi);
    let cases: [(
        &str,
        &bikesnest_domain::ParkingLocation,
        chrono::DateTime<chrono::Utc>,
        bool,
    ); 16] = [
        ("same-day, before opening", &same_day, sp(7, 59), false),
        ("same-day, opening minute", &same_day, sp(8, 0), true),
        ("same-day, last minute", &same_day, sp(17, 59), true),
        ("same-day, closing minute", &same_day, sp(18, 0), false),
        ("overnight, before opening", &overnight, sp(21, 59), false),
        ("overnight, own evening", &overnight, sp(23, 0), true),
        ("overnight, after midnight", &overnight, sp(1, 0), true),
        ("overnight, closing minute", &overnight, sp(2, 0), false),
        ("all day, midnight", &all_day, sp(0, 0), true),
        ("all day, last second", &all_day, sp(23, 59), true),
        ("weekdays only, on Tuesday", &weekdays_only, sp(12, 0), true),
        (
            "weekdays only, on Sunday",
            &weekdays_only,
            instant_at("America/Sao_Paulo", 2026, 3, 8, 12, 0),
            false,
        ),
        ("hours unknown is never open", &no_hours, sp(12, 0), false),
        ("hours unknown at midnight", &no_hours, sp(0, 0), false),
        (
            "DST: 01:30 EST, before the skip",
            &dst,
            utc_at(2026, 3, 8, 6, 30),
            false,
        ),
        (
            "DST: 03:30 EDT, after the skip",
            &dst,
            utc_at(2026, 3, 8, 7, 30),
            true,
        ),
    ];

    for (label, spot, at_instant, expected) in cases {
        let sql = sql_is_open_at(&db, spot.id(), spot.timezone().name(), at_instant).await;
        let domain = domain_open_now(spot, at_instant);
        assert_eq!(sql, expected, "SQL disagrees with the case: {label}");
        assert_eq!(
            domain, expected,
            "the domain disagrees with the case: {label}"
        );
        assert_eq!(sql, domain, "SQL and domain disagree: {label}");
    }
}

/// `bikesnest_is_open_at` called directly, rather than through the search: the
/// function is the thing under test.
async fn sql_is_open_at(
    db: &Db,
    id: i64,
    tz: &str,
    at_instant: chrono::DateTime<chrono::Utc>,
) -> bool {
    let row: (bool,) = sqlx::query_as("SELECT bikesnest_is_open_at($1, $2, $3)")
        .bind(id)
        .bind(tz)
        .bind(at_instant)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .expect("open-now function");
    row.0
}

// ---------------------------------------------------------------------------
// Browse mode: the map's viewport, and the grid it falls back to
// ---------------------------------------------------------------------------

#[db_test]
async fn in_bounds_returns_the_envelope_only_and_measures_from_its_centre(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    // ±0.01° is ~1.1 km north–south, so 300 m north is inside the box and
    // 2 km north is not.
    at(20.0, 0.0)
        .with_name("Inside centre")
        .create(&mut conn)
        .await
        .unwrap();
    at(20.0, 300.0)
        .with_name("Inside 300m")
        .with_cost(bikesnest_domain::Cost::Paid { price: None })
        .create(&mut conn)
        .await
        .unwrap();
    at(20.0, 2000.0)
        .with_name("Outside 2km")
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);

    let page = real_bounds(&db, &bounds_at(20.0, 0.01, 200)).await.unwrap();
    assert_eq!(page.total, 2, "the 2 km row is outside the envelope");
    assert!(
        page.clusters.is_empty(),
        "under the cap the answer is rows, not a grid"
    );
    let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["Inside centre", "Inside 300m"], "nearest first");
    // Distances are measured from the box's centre — the only origin a
    // viewport has.
    assert!(page.items[0].distance_m < 5.0, "{:?}", page.items[0]);
    assert!(
        (page.items[1].distance_m - 300.0).abs() < 5.0,
        "{:?}",
        page.items[1]
    );
    // Browse rows carry no keyset key: there is no page after this one.
    assert!(page.items.iter().all(|i| i.sort_key.is_none()));

    // The filters are the radius search's filters, applied to the same box.
    let free_only = bounds_with(
        20.0,
        0.01,
        200,
        Filters {
            cost: Some(CostFilter::Free),
            ..Filters::default()
        },
    );
    let page = real_bounds(&db, &free_only).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].name, "Inside centre");

    let lockers_only = bounds_with(
        20.0,
        0.01,
        200,
        Filters {
            types: vec![ParkingType::Locker],
            ..Filters::default()
        },
    );
    let page = real_bounds(&db, &lockers_only).await.unwrap();
    assert_eq!(page.total, 0, "every fixture here is a rack");
    assert!(page.items.is_empty());
}

#[db_test]
async fn in_bounds_clusters_past_the_cap_and_the_counts_sum_to_the_total(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let o = test_origin(21.0);
    // Two groups of *identical* points, six grid cells apart (the cell is the
    // box's width over twelve, so 0.01° cannot fall in one cell with them):
    // clustering is then a fact about the grid, not about a boundary landing
    // where the test hoped it would.
    for i in 0..3 {
        ParkingBuilder::new()
            .with_name(format!("West {i}"))
            .at(o.lat(), o.lon() - 0.005)
            .create(&mut conn)
            .await
            .unwrap();
    }
    for i in 0..4 {
        ParkingBuilder::new()
            .with_name(format!("East {i}"))
            .at(o.lat(), o.lon() + 0.005)
            .create(&mut conn)
            .await
            .unwrap();
    }
    drop(conn);

    // Cap of 3, seven rows in the box → the grid, not the rows.
    let page = real_bounds(&db, &bounds_at(21.0, 0.01, 3)).await.unwrap();
    assert_eq!(page.total, 7, "the total is the whole box, cap or no cap");
    assert!(
        page.items.is_empty(),
        "past the cap there are no rows to list"
    );
    assert_eq!(page.clusters.len(), 2, "{:?}", page.clusters);
    let counts: Vec<i64> = page.clusters.iter().map(|c| c.count).collect();
    assert_eq!(counts, vec![4, 3], "biggest cluster first");
    assert_eq!(
        counts.iter().sum::<i64>(),
        page.total as i64,
        "every row is in exactly one cluster"
    );
    // A cluster of identical points sits exactly on them.
    let east = page.clusters.iter().find(|c| c.count == 4).unwrap();
    assert!((east.lon - (o.lon() + 0.005)).abs() < 1e-6, "{east:?}");
    assert!((east.lat - o.lat()).abs() < 1e-6, "{east:?}");

    // Exactly at the cap the rows come back instead: the grid is for boxes
    // that hold *more* than can be drawn.
    let page = real_bounds(&db, &bounds_at(21.0, 0.01, 7)).await.unwrap();
    assert_eq!(page.items.len(), 7);
    assert!(page.clusters.is_empty());
}
