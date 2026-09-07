//! Application-layer tests for `SearchParking` / `GetParkingDetails` using
//! fake ports (no database — the real-SQL tests live in infrastructure).

use bikesnest_application::{
    BoundsPage, BoundsQuery, Cursor, Filters, GeoHit, GeocodeError, Geocoder, GetParkingDetails,
    ParkingDetailsReader, ParkingSearchReader, ParkingSummary, ReaderError, SearchError,
    SearchInput, SearchParking,
};
use bikesnest_domain::{Cost, GeoPoint, ParkingLocation, ParkingType, Rating, TimeRange, hms};
use std::sync::{Arc, Mutex};

fn sp_tz() -> chrono_tz::Tz {
    "America/Sao_Paulo".parse().unwrap()
}

fn summary(
    id: i64,
    distance_m: f64,
    rating_avg: Option<f64>,
    verified_days_ago: Option<i64>,
) -> ParkingSummary {
    ParkingSummary {
        id,
        name: format!("Spot {id}"),
        address: format!("Street {id}"),
        parking_type: ParkingType::Rack,
        cost: Cost::Free,
        point: GeoPoint::new(-23.56, -46.65).unwrap(),
        distance_m,
        security_yes: if id % 2 == 0 {
            vec!["cctv", "well_lit"]
        } else {
            vec![]
        }
        .into_iter()
        .map(str::to_string)
        .collect(),
        rating: Rating::new(rating_avg, if rating_avg.is_some() { 3 } else { 0 }).unwrap(),
        last_verified_at: verified_days_ago.map(|d| chrono::Utc::now() - chrono::Duration::days(d)),
        timezone: sp_tz(),
        is_open_now: true,
        photo_key: None,
        // Stands in for the reader's key: these fakes are read with the
        // distance sort, whose key is the distance itself.
        sort_key: Some(distance_m),
    }
}

/// A summary whose sort key is set independently of its distance — what a
/// scoring sort returns.
fn summary_keyed(id: i64, distance_m: f64, sort_key: f64) -> ParkingSummary {
    ParkingSummary {
        sort_key: Some(sort_key),
        ..summary(id, distance_m, None, Some(2))
    }
}

#[derive(Default, Clone)]
struct FakeGeocoder {
    calls: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Geocoder for FakeGeocoder {
    async fn geocode(&self, query: &str) -> Result<Option<GeoHit>, GeocodeError> {
        self.calls.lock().unwrap().push(query.to_string());
        if query.eq_ignore_ascii_case("rua xv de novembro") {
            Ok(Some(GeoHit {
                label: "Rua XV de Novembro".to_string(),
                point: GeoPoint::new(-23.561_414, -46.655_881).unwrap(),
            }))
        } else {
            Ok(None)
        }
    }
}

#[derive(Default, Clone)]
struct FakeReader {
    items: Vec<ParkingSummary>,
    received_limit: Arc<Mutex<Vec<usize>>>,
    received_apply_cursor: Arc<Mutex<Vec<bool>>>,
    /// Every bounds query that reached the reader — what the browse tests
    /// assert the use case passed through (and, by its emptiness, what it
    /// refused before touching the database).
    received_bounds: Arc<Mutex<Vec<BoundsQuery>>>,
}

impl FakeReader {
    fn new(items: Vec<ParkingSummary>) -> Self {
        Self {
            items,
            ..Default::default()
        }
    }
}

#[async_trait::async_trait]
impl ParkingSearchReader for FakeReader {
    async fn search(
        &self,
        _request: &bikesnest_application::SearchRequest,
        limit: usize,
        apply_cursor: bool,
    ) -> Result<bikesnest_application::SearchPage, ReaderError> {
        self.received_limit.lock().unwrap().push(limit);
        self.received_apply_cursor
            .lock()
            .unwrap()
            .push(apply_cursor);
        let items = self.items.iter().take(limit).cloned().collect();
        Ok(bikesnest_application::SearchPage {
            items,
            total: self.items.len() as i64,
            next_cursor: None,
        })
    }

    async fn in_bounds(&self, query: &BoundsQuery) -> Result<BoundsPage, ReaderError> {
        self.received_bounds.lock().unwrap().push(query.clone());
        Ok(BoundsPage {
            items: self.items.clone(),
            total: self.items.len(),
            clusters: Vec::new(),
        })
    }
}

fn use_case(geocoder: FakeGeocoder, reader: FakeReader) -> SearchParking {
    SearchParking::new(Box::new(geocoder), Box::new(reader))
}

fn input() -> SearchInput {
    SearchInput {
        query: Some("Rua XV de Novembro".to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn missing_destination_without_query_or_coordinates() {
    let result = use_case(FakeGeocoder::default(), FakeReader::default())
        .execute(SearchInput::default())
        .await;
    assert!(matches!(result, Err(SearchError::MissingDestination)));
}

#[tokio::test]
async fn unresolvable_query_is_missing_destination() {
    let input = SearchInput {
        query: Some("nowhere at all".to_string()),
        ..Default::default()
    };
    let result = use_case(FakeGeocoder::default(), FakeReader::default())
        .execute(input)
        .await;
    assert!(matches!(result, Err(SearchError::MissingDestination)));
}

#[tokio::test]
async fn explicit_coordinates_win_over_query_and_skip_geocoder() {
    let geocoder = FakeGeocoder::default();
    let input = SearchInput {
        query: Some("Rua XV de Novembro".to_string()),
        lat: Some(-23.55),
        lon: Some(-46.66),
        ..Default::default()
    };
    let (page, hit) = use_case(geocoder.clone(), FakeReader::default())
        .execute(input)
        .await
        .unwrap();
    let hit = hit.expect("explicit coordinates are returned as the map origin");
    assert_eq!(hit.label, "Rua XV de Novembro");
    assert_eq!(
        hit.point,
        bikesnest_domain::GeoPoint::new(-23.55, -46.66).unwrap()
    );
    assert!(geocoder.calls.lock().unwrap().is_empty());
    assert_eq!(page.total, 0);
}

#[tokio::test]
async fn coordinate_only_search_labels_its_map_origin_with_the_coordinates() {
    let (page, hit) = use_case(FakeGeocoder::default(), FakeReader::default())
        .execute(SearchInput {
            lat: Some(-25.4284),
            lon: Some(-49.2733),
            ..Default::default()
        })
        .await
        .unwrap();

    assert_eq!(hit.unwrap().label, "-25.42840, -49.27330");
    assert_eq!(page.total, 0);
}

#[tokio::test]
async fn query_is_geocoded_and_label_preserved() {
    let (page, hit) = use_case(FakeGeocoder::default(), FakeReader::default())
        .execute(input())
        .await
        .unwrap();
    assert_eq!(hit.unwrap().label, "Rua XV de Novembro");
    assert_eq!(page.total, 0);
}

#[tokio::test]
async fn radius_and_page_size_are_clamped() {
    let mut input = input();
    input.sort = Some("distance".to_string());
    input.radius_m = Some(9999); // not in allowlist → default 1000
    input.page_size = Some(100_000); // clamp to MAX_PAGE_SIZE
    let reader = FakeReader::default();
    use_case(FakeGeocoder::default(), reader.clone())
        .execute(input)
        .await
        .unwrap();
    assert_eq!(*reader.received_limit.lock().unwrap().last().unwrap(), 101);

    // Every sort now reads one page plus the look-ahead row, `recommended`
    // (the default) included: there is no candidate fetch to cap any more.
    let rec_input = SearchInput {
        query: Some("Rua XV de Novembro".to_string()),
        page_size: Some(100_000),
        ..Default::default()
    };
    let reader = FakeReader::default();
    use_case(FakeGeocoder::default(), reader.clone())
        .execute(rec_input)
        .await
        .unwrap();
    assert_eq!(*reader.received_limit.lock().unwrap().last().unwrap(), 101);
}

/// The home page's featured strip asks for four cards; the reader must be
/// asked for five (the look-ahead row), not for a five-hundred-row candidate
/// set it would then throw away.
#[tokio::test]
async fn a_small_page_size_reads_a_small_page() {
    let reader = FakeReader::new(
        (1..=30)
            .map(|i| summary(i, i as f64 * 10.0, None, Some(1)))
            .collect(),
    );
    let (page, _) = use_case(FakeGeocoder::default(), reader.clone())
        .execute(SearchInput {
            lat: Some(-25.4297),
            lon: Some(-49.2705),
            radius_m: Some(1000),
            page_size: Some(4),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(
        *reader.received_limit.lock().unwrap().last().unwrap(),
        5,
        "page_size 4 + one look-ahead row"
    );
    assert_eq!(page.items.len(), 4);
    assert!(page.next_cursor.is_some(), "more rows exist");
}

/// `Recommended` is a SQL sort like every other one now: the reader ranks and
/// limits, the use case passes the cursor down and mints the next one from the
/// key the reader computed. Nothing is re-ranked in memory, so no page can be
/// ordered differently from the query that produced it.
#[tokio::test]
async fn recommended_sort_pages_through_the_reader_like_the_other_sorts() {
    let items = vec![
        summary_keyed(3, 50.0, -0.72),
        summary_keyed(1, 100.0, -0.68),
        summary_keyed(2, 100.0, -0.61),
    ];
    let reader = FakeReader::new(items);
    let mut input = input();
    input.sort = Some("recommended".to_string());
    input.page_size = Some(2);
    let (page, _) = use_case(FakeGeocoder::default(), reader.clone())
        .execute(input.clone())
        .await
        .unwrap();
    assert_eq!(
        reader.received_apply_cursor.lock().unwrap().last().unwrap(),
        &true,
        "the reader applies the keyset predicate for recommended too"
    );
    assert_eq!(reader.received_limit.lock().unwrap().last().unwrap(), &3);
    assert_eq!(
        page.items.iter().map(|i| i.id).collect::<Vec<_>>(),
        vec![3, 1],
        "reader order is the page order"
    );
    let next = page.next_cursor.expect("a second page exists");
    assert_eq!(next.sort, bikesnest_application::Sort::Recommended);
    assert!(
        (next.v - (-0.68)).abs() < 1e-9,
        "the cursor anchors on the reader's own sort key, not a recomputed score"
    );
    assert_eq!(next.id, 1);

    // A cursor minted for one sort must not be replayed against another.
    let mut crossed = input;
    crossed.sort = Some("distance".to_string());
    crossed.cursor = Some(next.encode());
    let request = bikesnest_application::SearchRequest::new(
        GeoPoint::new(-23.5, -46.6).unwrap(),
        None,
        1000,
        Filters::default(),
        bikesnest_application::Sort::Distance,
        20,
        Some(&next.encode()),
    );
    assert!(request.cursor.is_none());
}

#[tokio::test]
async fn sql_sorts_pass_cursor_to_reader_and_build_next_cursor() {
    let items = vec![
        summary(1, 100.0, Some(4.0), Some(5)),
        summary(2, 300.0, Some(5.0), Some(1)),
        summary(3, 700.0, Some(3.0), Some(40)),
    ];
    let reader = FakeReader::new(items);
    let mut input = input();
    input.sort = Some("distance".to_string());
    input.page_size = Some(2);
    let (page, _) = use_case(FakeGeocoder::default(), reader.clone())
        .execute(input)
        .await
        .unwrap();
    // reader was asked for page_size+1 rows with cursor application enabled
    assert_eq!(reader.received_limit.lock().unwrap().last().unwrap(), &3);
    assert_eq!(
        reader.received_apply_cursor.lock().unwrap().last().unwrap(),
        &true
    );
    // page_size < items returned → next cursor present, anchored on item 2
    let next = page.next_cursor.expect("has next page");
    assert_eq!(next.sort.as_code(), "distance");
    let decoded = Cursor::decode(&next.encode()).unwrap();
    assert_eq!(decoded.id, page.items[1].id);
    assert!((decoded.v - page.items[1].distance_m).abs() < 1e-9);
}

#[tokio::test]
async fn cursor_roundtrip_and_mismatched_sort_are_handled() {
    let c = Cursor {
        sort: bikesnest_application::Sort::Rating,
        v: -4.2,
        id: 77,
    };
    let decoded = Cursor::decode(&c.encode()).unwrap();
    assert_eq!(decoded, c);
    assert!(Cursor::decode("garbage!!!").is_none());
    // A cursor for a different sort is dropped by SearchRequest (page 1).
    let request = bikesnest_application::SearchRequest::new(
        GeoPoint::new(-23.5, -46.6).unwrap(),
        Some("x".into()),
        1000,
        Filters::default(),
        bikesnest_application::Sort::Distance,
        20,
        Some(&c.encode()),
    );
    assert!(request.cursor.is_none());
}

#[tokio::test]
async fn filters_parse_from_input() {
    let input = SearchInput {
        query: Some("Rua XV de Novembro".to_string()),
        cost: Some("free".to_string()),
        types: Some("rack,bogus,secured,rack".to_string()),
        security: Some("cctv, well_lit".to_string()),
        open_now: true,
        ..Default::default()
    };
    let filters: Filters = input.filters();
    assert_eq!(filters.cost, Some(bikesnest_application::CostFilter::Free));
    // Parsing keeps unknown codes out; deduplication is SearchRequest's job.
    assert_eq!(
        filters.types,
        vec![ParkingType::Rack, ParkingType::Secured, ParkingType::Rack]
    );
    assert_eq!(filters.security_all, vec!["cctv", "well_lit"]);
    assert!(filters.open_now);

    // SearchRequest::new normalizes: dedup types, trim/sort security codes.
    let request = bikesnest_application::SearchRequest::new(
        GeoPoint::new(-23.5, -46.6).unwrap(),
        Some("x".into()),
        1000,
        filters,
        bikesnest_application::Sort::Distance,
        20,
        None,
    );
    assert_eq!(
        request.filters.types,
        vec![ParkingType::Rack, ParkingType::Secured]
    );
    assert_eq!(request.filters.security_all, vec!["cctv", "well_lit"]);
}

// ---------------------------------------------------------------------------
// GetParkingDetails
// ---------------------------------------------------------------------------

/// A filter code no catalog knows can never be confirmed on any location, so
/// keeping it would turn the whole search into "no results". A stale or
/// hand-edited URL degrades to the filters that still mean something.
#[tokio::test]
async fn unknown_security_codes_are_dropped_from_the_request() {
    let request = |codes: Vec<&str>| {
        bikesnest_application::SearchRequest::new(
            GeoPoint::new(-23.5, -46.6).unwrap(),
            None,
            1000,
            Filters {
                security_all: codes.into_iter().map(str::to_string).collect(),
                ..Filters::default()
            },
            bikesnest_application::Sort::Distance,
            20,
            None,
        )
    };

    assert_eq!(
        request(vec!["cctv", "laser_fence", "well_lit"])
            .filters
            .security_all,
        vec!["cctv", "well_lit"],
    );
    assert!(
        request(vec!["laser_fence"]).filters.security_all.is_empty(),
        "an all-unknown filter set becomes no filter, not an impossible one"
    );
    assert_eq!(
        request(vec![" cctv ", "cctv", ""]).filters.security_all,
        vec!["cctv"],
        "codes are trimmed and deduped"
    );
}

struct OneLocationReader(Option<ParkingLocation>);

#[async_trait::async_trait]
impl ParkingDetailsReader for OneLocationReader {
    async fn details(&self, _id: i64) -> Result<Option<ParkingLocation>, ReaderError> {
        Ok(self.0.clone())
    }
}

fn location(hours: bikesnest_domain::OpeningHours) -> ParkingLocation {
    ParkingLocation::new(
        42,
        "Estação Vila Mariana",
        "R. Domingos de Morais, 1000",
        None,
        ParkingType::Secured,
        Cost::Paid { price: None },
        GeoPoint::new(-23.5895, -46.6385).unwrap(),
        sp_tz(),
        hours,
        vec![],
        bikesnest_domain::ModerationState::Active,
        Rating::new(Some(4.5), 2).unwrap(),
        chrono::Utc::now(),
        chrono::Utc::now(),
        None,
        Some(chrono::Utc::now() - chrono::Duration::days(10)),
        1,
    )
    .unwrap()
}

#[tokio::test]
async fn details_view_computes_freshness_and_open_status() {
    // Open 09:00–18:00 on Mondays (SP local).
    let hours =
        bikesnest_domain::OpeningHours::weekly(vec![(1, TimeRange::new(hms(9, 0), hms(18, 0)))]);
    let uc = GetParkingDetails::new(
        Box::new(OneLocationReader(Some(location(hours)))),
        Default::default(),
    );
    let view = uc.execute(42).await.unwrap().unwrap();
    assert_eq!(view.freshness, bikesnest_domain::FreshnessCategory::Fresh);
    // Unknown hours would be Unknown; with schedule, depends on now — just check it computes.
    assert!(matches!(
        view.is_open_now,
        bikesnest_domain::OpenStatus::Open | bikesnest_domain::OpenStatus::Closed
    ));
}

#[tokio::test]
async fn details_of_unknown_id_is_none() {
    let uc = GetParkingDetails::new(Box::new(OneLocationReader(None)), Default::default());
    assert!(uc.execute(999).await.unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Browse mode: the box is the request, and it is validated here
// ---------------------------------------------------------------------------

fn browse_input(bbox: &str) -> SearchInput {
    SearchInput {
        bbox: Some(bbox.to_string()),
        ..Default::default()
    }
}

#[tokio::test]
async fn browse_rejects_every_kind_of_impossible_box() {
    // Each of these must be refused *before* the reader is asked anything: a
    // box the database cannot be trusted with is a 400, not an empty page.
    let cases = [
        ("", "no box at all"),
        ("-49.30,-25.45,-49.25", "three numbers"),
        ("-49.30,-25.45,-49.25,-25.41,7", "five numbers"),
        ("west,-25.45,-49.25,-25.41", "not a number"),
        ("-49.25,-25.45,-49.30,-25.41", "west east of east"),
        ("-49.30,-25.41,-49.25,-25.45", "south north of north"),
        ("-49.30,-25.45,-49.30,-25.41", "zero width"),
        ("-181.0,-25.45,-49.25,-25.41", "longitude off the globe"),
        ("-49.30,-91.0,-49.25,-25.41", "latitude off the globe"),
        ("-50.30,-25.45,-49.25,-25.41", "wider than the span limit"),
        ("-49.30,-26.45,-49.25,-25.41", "taller than the span limit"),
    ];
    for (bbox, why) in cases {
        let reader = FakeReader::default();
        let result = use_case(FakeGeocoder::default(), reader.clone())
            .browse(&browse_input(bbox))
            .await;
        assert!(
            matches!(result, Err(SearchError::InvalidBounds)),
            "{why} ({bbox}) must be InvalidBounds"
        );
        assert!(
            reader.received_bounds.lock().unwrap().is_empty(),
            "{why} must not reach the reader"
        );
    }
}

#[tokio::test]
async fn browse_refuses_a_cursor_because_it_has_no_pages() {
    let reader = FakeReader::default();
    let input = SearchInput {
        cursor: Some("whatever".to_string()),
        ..browse_input("-49.30,-25.45,-49.25,-25.41")
    };
    let result = use_case(FakeGeocoder::default(), reader.clone())
        .browse(&input)
        .await;
    assert!(matches!(result, Err(SearchError::BoundsNotPaginated)));
    assert!(reader.received_bounds.lock().unwrap().is_empty());
}

#[tokio::test]
async fn browse_passes_the_box_and_every_filter_to_the_reader() {
    let reader = FakeReader::new(vec![summary(1, 120.0, None, Some(1))]);
    let input = SearchInput {
        cost: Some("free".to_string()),
        types: Some("rack,locker".to_string()),
        security: Some("cctv".to_string()),
        open_now: true,
        ..browse_input(" -49.30 , -25.45 , -49.25 , -25.41 ")
    };
    let (bounds, page) = use_case(FakeGeocoder::default(), reader.clone())
        .browse(&input)
        .await
        .expect("a valid box");

    assert_eq!(page.total, 1);
    let seen = reader.received_bounds.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "exactly one reader call");
    // The box the reader got is the box the caller gets back — the view layer
    // frames the map with it, so they must not be two different boxes.
    assert_eq!(seen[0], bounds);
    assert_eq!(seen[0].west, -49.30);
    assert_eq!(seen[0].south, -25.45);
    assert_eq!(seen[0].east, -49.25);
    assert_eq!(seen[0].north, -25.41);
    assert_eq!(seen[0].limit, bikesnest_application::BROWSE_MARKER_CAP);
    assert_eq!(
        seen[0].filters.cost,
        Some(bikesnest_application::CostFilter::Free)
    );
    assert_eq!(
        seen[0].filters.types,
        vec![
            bikesnest_domain::ParkingType::Rack,
            bikesnest_domain::ParkingType::Locker
        ]
    );
    assert_eq!(seen[0].filters.security_all, vec!["cctv".to_string()]);
    assert!(seen[0].filters.open_now);
}

#[test]
fn a_browse_box_centres_and_grids_itself() {
    let bounds = BoundsQuery::parse("-49.30,-25.45,-49.25,-25.41", Filters::default(), 200)
        .expect("a valid box");
    let center = bounds.center();
    assert!((center.lat() - -25.43).abs() < 1e-9, "{center:?}");
    assert!((center.lon() - -49.275).abs() < 1e-9, "{center:?}");
    // The grid is the box's width over twelve columns, whatever the zoom.
    assert!((bounds.cell_deg() - 0.05 / 12.0).abs() < 1e-9);
}
