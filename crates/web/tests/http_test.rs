//! HTTP-layer tests: M0 health/readiness endpoints + M1 pages.
//!
//! Run via `#[db_test]` so they share the suite's runtime and migrated pool.
//! Page tests that assert against seeded search results use the committed-
//! fixture pattern (see crates/infrastructure/tests/parking_test.rs).

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use bikesnest_infrastructure::Db;
use bikesnest_test_support::{ParkingBuilder, db_test, pool, test_config};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// The public-page router: the real providers the test `Config` selects (fake
/// email/geocoder, in-memory limiter, the compose MinIO for media).
async fn test_app() -> axum::Router {
    let db = Db::from_pool(pool().await);
    bikesnest_web::app_router(std::sync::Arc::new(test_config()), db)
        .expect("test config builds every provider")
}

/// GET and return only the response headers (for security-header asserts).
async fn get_headers(uri: &str) -> HeaderMap {
    let app = test_app().await;
    let res = app
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    res.headers().clone()
}

async fn get(uri: &str) -> (StatusCode, String) {
    let app = test_app().await;
    // Pin the locale to English so assertions on English copy are deterministic
    // (default resolution falls back to pt-BR — ).
    let res = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("Accept-Language", "en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).to_string())
}

// ---------------------------------------------------------------------------
// M0: health / readiness
// ---------------------------------------------------------------------------

#[db_test]
async fn healthz_is_alive_without_dependencies(_tx: &mut TestTx) {
    let (status, _) = get("/healthz").await;
    assert_eq!(status, StatusCode::OK);
}

#[db_test]
async fn readyz_returns_ready_with_real_database(_tx: &mut TestTx) {
    let (status, body) = get("/readyz").await;
    assert_eq!(status, StatusCode::OK);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["status"], "ready");
    assert_eq!(json["database"], "up");
}

// ---------------------------------------------------------------------------
// Security headers
// ---------------------------------------------------------------------------

#[db_test]
async fn security_headers_present_on_public_page(_tx: &mut TestTx) {
    let headers = get_headers("/").await;
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(
        headers["referrer-policy"],
        "strict-origin-when-cross-origin"
    );
    assert_eq!(headers["x-frame-options"], "DENY");
    assert!(headers.contains_key("content-security-policy"));
    assert!(headers.contains_key("permissions-policy"));
}

#[db_test]
async fn security_headers_present_on_private_page(_tx: &mut TestTx) {
    // Account page redirects anonymous users, but the header set rides every response.
    let headers = get_headers("/login").await;
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(headers.contains_key("content-security-policy"));
}

#[db_test]
async fn security_headers_present_when_auth_short_circuits(_tx: &mut TestTx) {
    // A state-changing request without a CSRF cookie → the auth middleware returns
    // 403 *without* running the inner handler. The security-header middleware is
    // outermost and must still apply CSP/nosniff to that response.
    let app = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/logout")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::FORBIDDEN);
    assert!(
        res.headers().contains_key("content-security-policy"),
        "CSP must be present on a CSRF-403 response"
    );
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
}

#[db_test]
async fn csp_is_strict_no_unsafe_eval(_tx: &mut TestTx) {
    let headers = get_headers("/").await;
    let csp = headers["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.starts_with("default-src 'self'"), "csp: {csp}");
    assert!(csp.contains("script-src 'self'"), "csp: {csp}");
    assert!(!csp.contains("unsafe-eval"), "csp must forbid eval: {csp}");
    assert!(csp.contains("object-src 'none'"), "csp: {csp}");
    assert!(csp.contains("frame-ancestors 'none'"), "csp: {csp}");
}

// `img-src` honoring `CSP_MEDIA_HOSTS` is covered by
// `security::tests::csp_img_src_includes_media_hosts` (crates/web/src/security.rs),
// which builds `SecurityHeaders` directly instead of mutating the process
// environment for the duration of a live request (now that workspace lints —
// including `unsafe_code = "forbid"` — are enforced on this crate, an
// unguarded `std::env::set_var`/`remove_var` pair no longer compiles here).

#[db_test]
async fn hsts_absent_in_dev_without_tls(_tx: &mut TestTx) {
    // `TLS_ON` is unset in the test environment → no HSTS header.
    let headers = get_headers("/").await;
    assert!(!headers.contains_key("strict-transport-security"));
}

// ---------------------------------------------------------------------------
// SEO / indexing (//)
// ---------------------------------------------------------------------------

#[db_test]
async fn robots_txt_matches_crawl_policy(_tx: &mut TestTx) {
    let (status, body) = get("/robots.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("User-agent: *"));
    assert!(body.contains("Allow: /"));
    assert!(body.contains("Disallow: /account"));
    assert!(body.contains("Disallow: /admin"));
    assert!(body.contains("Disallow: /moderation"));
}

#[db_test]
async fn sitemap_includes_static_pages_and_active_parking(tx: &mut TestTx) {
    const MARK: &str = "fix-http-sitemap";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let conn = tx.executor();
    let loc = ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("Sitemap Fixture")
        .at(-33.920_000, -70.620_000)
        .create(&mut *conn)
        .await
        .unwrap();
    tx.commit_fixture().await;

    let (status, body) = get("/sitemap.xml").await;
    assert_eq!(status, StatusCode::OK);
    let base = "http://localhost:8080";
    assert!(
        body.contains(&format!("<loc>{base}/about</loc>")),
        "static page in sitemap"
    );
    assert!(
        body.contains(&format!("<loc>{base}/search</loc>")),
        "static page in sitemap"
    );
    assert!(
        body.contains(&format!("<loc>{base}/parking/{}</loc>", loc.id())),
        "active parking in sitemap"
    );

    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn public_page_has_canonical_description_and_hreflang(_tx: &mut TestTx) {
    let (status, body) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("rel=\"canonical\""),
        "home has canonical link"
    );
    assert!(
        body.contains("name=\"description\""),
        "home has meta description"
    );
    assert!(body.contains("property=\"og:title\""), "home has og:title");
    assert!(
        body.contains("hreflang=\"pt-BR\""),
        "hreflang pt-BR present"
    );
    assert!(body.contains("hreflang=\"en\""), "hreflang en present");
}

#[db_test]
async fn private_pages_are_noindex(_tx: &mut TestTx) {
    for path in ["/account", "/admin/users", "/moderation"] {
        let headers = get_headers(path).await;
        let v = headers["x-robots-tag"].to_str().unwrap();
        assert!(v.contains("noindex"), "{path} must be noindex, got: {v}");
    }
}

// ---------------------------------------------------------------------------
// M1: pages
// ---------------------------------------------------------------------------

#[db_test]
async fn home_renders_hero_and_search_form(_tx: &mut TestTx) {
    let (status, body) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("From destination to parked bike"),
        "hero headline"
    );
    assert!(body.contains(r#"action="/search""#), "search form");
}

#[db_test]
async fn skip_link_targets_main_content(_tx: &mut TestTx) {
    let (status, body) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains(r##"href="#content""##), "skip link");
    assert!(body.contains(r#"id="content""#), "main landmark");
}

#[db_test]
async fn about_renders_how_it_works(_tx: &mut TestTx) {
    let (status, body) = get("/about").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("How verification"));
}

#[db_test]
async fn search_without_destination_shows_guidance_not_error(_tx: &mut TestTx) {
    let (status, body) = get("/search").await;
    assert_eq!(status, StatusCode::OK, "user input is not a server error");
    assert!(body.contains("Type a destination"));
}

#[db_test]
async fn search_resolves_query_through_fake_geocoder(_tx: &mut TestTx) {
    let (status, body) = get("/search?q=Rua%20XV%20de%20Novembro").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Parking near"),
        "resolved destination headline"
    );
    assert!(body.contains("search-data"), "map payload embedded");
}

#[db_test]
async fn htmx_request_gets_fragment_without_full_page(_tx: &mut TestTx) {
    let (status, body) = get("/search?lat=-25.4284&lon=-49.2733&sort=distance").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("search-data"));
    // Full-page request vs fragment: send the header this time.
    let app = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/search?lat=-25.4284&lon=-49.2733&sort=distance")
                .header("HX-Request", "true")
                .header("Accept-Language", "en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(
        !html.contains("<!DOCTYPE"),
        "fragment must not be a full page"
    );
    assert!(
        html.contains("search-data"),
        "fragment still embeds map data"
    );
}

#[db_test]
async fn htmx_search_fragment_updates_result_count_out_of_band(_tx: &mut TestTx) {
    // The result count / destination heading live outside `#results` in
    // search.html, so the fragment response must carry `hx-swap-oob` copies
    // for htmx to patch them in place.
    let app = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/search?q=x")
                .header("HX-Request", "true")
                .header("Accept-Language", "en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains(r#"id="result-count""#), "fragment: {html}");
    assert!(html.contains("hx-swap-oob"), "fragment: {html}");
}

#[db_test]
async fn search_renders_committed_fixture_rows_with_filters(tx: &mut TestTx) {
    const MARK: &str = "fix-http-search";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let conn = tx.executor();
    // Two free racks and one paid locker, ~5.5 km from any other test patch.
    ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("HTTP Fixture Free A")
        .at(-33.900_000, -70.600_000)
        .create(&mut *conn)
        .await
        .unwrap();
    ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("HTTP Fixture Free B")
        .at(-33.900_300, -70.600_000)
        .create(&mut *conn)
        .await
        .unwrap();
    ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("HTTP Fixture Paid")
        .with_cost(bikesnest_domain::Cost::Paid { price: None })
        .at(-33.900_600, -70.600_000)
        .create(&mut *conn)
        .await
        .unwrap();
    tx.commit_fixture().await;

    let (_, body) = get("/search?lat=-33.900000&lon=-70.600000&radius=1000&sort=distance").await;
    assert!(
        body.contains("3 parking spots"),
        "all fixtures visible: {}",
        body.len()
    );
    assert!(body.contains("HTTP Fixture Free A"));

    // Cost filter narrows to the free ones.
    let (_, body) =
        get("/search?lat=-33.900000&lon=-70.600000&radius=1000&sort=distance&cost=free").await;
    assert!(body.contains("2 parking spots"));
    assert!(body.contains("HTTP Fixture Free B"));
    assert!(!body.contains("HTTP Fixture Paid"));

    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
}

/// Stored-XSS regression: a user-controlled `name`/`address` containing
/// `</script>` must not break out of the `<script type="application/json"
/// id="search-data">` embed. The map payload escapes `<`/`>`/`&`/U+2028/U+2029
/// so `JSON.parse` round-trips the original value but no literal `</script>`
/// survives.
#[db_test]
async fn search_map_payload_is_html_safe_for_ugc_names(tx: &mut TestTx) {
    const MARK: &str = "fix-http-xss";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let conn = tx.executor();
    let payload_attack = "</script><img src=x onerror=alert(1)>";
    ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name(payload_attack)
        .at(-33.910_000, -70.610_000)
        .create(&mut *conn)
        .await
        .unwrap();
    tx.commit_fixture().await;

    let (_, body) = get("/search?lat=-33.910000&lon=-70.610000&radius=1000&sort=distance").await;

    // The escaped JSON must be present, so the browser's JSON.parse gets `<` back.
    assert!(
        body.contains(r"\u003c/script\u003e"),
        "map payload must escape the closing script tag"
    );
    // The raw contiguous attack sequence must be gone from the whole response.
    assert!(
        !body.contains(payload_attack),
        "raw attack sequence must not appear"
    );
    // Grab the search-data block and parse it back — the original value survives.
    let marker = "<script type=\"application/json\" id=\"search-data\">";
    let start = body.find(marker).expect("search-data block present");
    let rest = &body[start + marker.len()..];
    let end = rest.find("</script>").unwrap_or(rest.len());
    let json_block = &rest[..end];
    assert!(json_block.contains(r"\u003cimg src=x onerror=alert(1)\u003e"));
    let parsed: serde_json::Value = serde_json::from_str(json_block).expect("valid JSON block");
    let round_trip = serde_json::to_string(&parsed).unwrap();
    assert!(
        round_trip.contains(payload_attack),
        "JSON.parse must round-trip to the original value"
    );

    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
}

// --- Browse mode (WP20): ?bbox= ---------------------------------------------

/// A box around the fixture below, as the map writes it: `west,south,east,north`.
const BROWSE_BBOX: &str = "-49.30,-25.45,-49.25,-25.41";

/// The JSON the map reads, lifted out of the page's `#search-data` island.
fn search_data_block(body: &str) -> String {
    let marker = "<script type=\"application/json\" id=\"search-data\">";
    let start = body.find(marker).expect("search-data block present");
    let rest = &body[start + marker.len()..];
    let end = rest.find("</script>").unwrap_or(rest.len());
    rest[..end].to_string()
}

#[db_test]
async fn browsing_a_box_lists_numbered_cards_and_no_next_page(tx: &mut TestTx) {
    const MARK: &str = "fix-http-browse";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let conn = tx.executor();
    // Inside the box, so the assertions below hold on an otherwise empty
    // database as well as on a seeded one.
    ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("Browse Box Fixture")
        .at(-25.430_000, -49.275_000)
        .create(&mut *conn)
        .await
        .unwrap();
    tx.commit_fixture().await;

    let (status, body) = get(&format!("/search?bbox={BROWSE_BBOX}")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(is_document(&body), "a bbox URL is a page, not a fragment");
    assert!(body.contains("Parking in this area"), "browse heading");
    assert!(body.contains("Browse Box Fixture"), "the fixture is listed");
    // Distances have no destination to be from, so the list says what they are
    // measured from instead.
    assert!(
        body.contains("Distances are measured from the centre of the map."),
        "from-centre note"
    );
    // Numbered cards, and the same numbers in the map payload.
    assert!(body.contains(r#"aria-label="Spot 1""#), "card number badge");
    let json_block = search_data_block(&body);
    let parsed: serde_json::Value = serde_json::from_str(&json_block).expect("valid JSON block");
    let first = &parsed["items"][0];
    assert_eq!(first["n"], 1, "markers carry the card's number: {parsed}");
    assert!(
        first["href"]
            .as_str()
            .expect("a marker links to its details page")
            .starts_with("/parking/"),
        "{parsed}"
    );
    assert_eq!(
        parsed["bbox"].as_array().map(Vec::len),
        Some(4),
        "the map frames the box the server answered for: {parsed}"
    );
    // Browse is not paginated.
    assert!(!body.contains("Next page"), "browse has no next page");

    // The same URL as an htmx fragment: the results list, not a document.
    let app = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri(format!("/search?bbox={BROWSE_BBOX}"))
                .header("HX-Request", "true")
                .header("HX-Target", "results")
                .header("Accept-Language", "en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let fragment = res.into_body().collect().await.unwrap().to_bytes();
    let fragment = String::from_utf8_lossy(&fragment).to_string();
    assert!(!fragment.contains("<!DOCTYPE"), "fragment: {fragment}");
    assert!(
        fragment.contains("Browse Box Fixture"),
        "fragment: {fragment}"
    );
    assert!(
        fragment.contains("Parking in this area"),
        "the out-of-band heading names the area too"
    );

    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn an_unusable_box_is_a_400_with_a_notice_not_a_500(_tx: &mut TestTx) {
    // Inside out, off the globe, wider than the span limit, not a box at all.
    for bbox in [
        "-49.25,-25.45,-49.30,-25.41",
        "-49.30,-95.0,-49.25,-25.41",
        "-52.00,-25.45,-49.25,-25.41",
        "not-a-box",
    ] {
        let (status, body) = get(&format!("/search?bbox={bbox}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "bbox={bbox}");
        assert!(
            body.contains("That map area can&#39;t be searched"),
            "bbox={bbox} must explain itself: {body}"
        );
        assert!(is_document(&body), "bbox={bbox} renders a styled page");
    }
}

#[db_test]
async fn a_cursor_on_a_browse_url_is_refused(_tx: &mut TestTx) {
    let (status, body) = get(&format!("/search?bbox={BROWSE_BBOX}&cursor=abc")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.contains("Browsing the map has no pages"),
        "the notice says why: {body}"
    );
}

#[db_test]
async fn a_destination_wins_over_a_box(_tx: &mut TestTx) {
    // A hand-edited URL carrying both: the destination is what the viewer
    // asked for, so it is a radius search — not a browse.
    let (status, body) = get(&format!(
        "/search?q=Rua%20XV%20de%20Novembro&bbox={BROWSE_BBOX}"
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Parking near"), "radius search headline");
    assert!(!body.contains("Parking in this area"));
}

#[db_test]
async fn every_entry_point_offers_the_map(_tx: &mut TestTx) {
    // The empty prompt: a way in that needs no destination typed.
    let (status, body) = get("/search").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Explore the map"),
        "the prompt offers the map"
    );
    assert!(
        body.contains("/search?bbox=-49.2905,-25.4497,-49.2505,-25.4097"),
        "…pointed at the centre box: {body}"
    );
    // The home page's explore link is that same box, not one hard-coded street.
    let (status, body) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("/search?bbox=-49.2905,-25.4497,-49.2505,-25.4097"),
        "home explore link"
    );
    assert!(
        !body.contains("/search?q=Rua+XV+de+Novembro"),
        "the hard-coded street link is gone"
    );
}

#[db_test]
async fn parking_details_renders_full_page_and_404_for_unknown(tx: &mut TestTx) {
    // Unknown id → styled 404 page.
    let (status, body) = get("/parking/999999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.contains("does not exist"));

    // Self-contained committed fixture (no dependency on `seed-mock`).
    const MARK: &str = "fix-http-details";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let conn = tx.executor();
    let created = ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("Details Fixture")
        .at(-25.4300, -49.2700)
        .create(&mut *conn)
        .await
        .unwrap();
    tx.commit_fixture().await;

    let (status, body) = get(&format!("/parking/{}", created.id())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("Key facts"));
    assert!(body.contains("Opening hours"));
    assert!(
        body.contains("Open in Google Maps"),
        "external navigation link"
    );
    assert!(body.contains("Security attributes"));

    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn never_verified_location_shows_the_freshness_label_once(tx: &mut TestTx) {
    // Problem #4 regression: a never-verified location's freshness card must
    // not render the "never verified" copy twice — `freshness_label` and
    // `verified_label` both resolve to the same string when there's no
    // `last_verified_at`, so the template must collapse them into one.
    const MARK: &str = "fix-http-never-verified";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let conn = tx.executor();
    let created = ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("Never Verified Fixture")
        .at(-25.4300, -49.2700)
        .never_verified()
        .create(&mut *conn)
        .await
        .unwrap();
    tx.commit_fixture().await;

    let (status, body) = get(&format!("/parking/{}", created.id())).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("Never verified · Never verified"),
        "freshness card must not repeat the never-verified label"
    );
    assert!(
        body.contains("Never verified"),
        "freshness card should still show the label once"
    );

    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// M2: accounts & authentication
// ---------------------------------------------------------------------------

use bikesnest_infrastructure::{FakeEmailProvider, FakeOAuthProvider};
use bikesnest_test_support::TestPasswordHasher;
use bikesnest_web::{RouterDeps, app_router_with};

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

async fn auth_app() -> (axum::Router, FakeEmailProvider) {
    // The wider M2/M3/... suite exercises the fake OAuth flow's plumbing, so
    // it builds with Google sign-in enabled; WP1's own tests below build with
    // `auth_app_opts(false)` to cover the disabled (default) product state.
    auth_app_opts(true).await
}

/// Like [`auth_app`], but with the Google sign-in feature flag set explicitly
/// (product decision: disabled by default until a real OAuth provider exists).
async fn auth_app_opts(google_oauth_enabled: bool) -> (axum::Router, FakeEmailProvider) {
    let email = FakeEmailProvider::with_root(None);
    let db = Db::from_pool(pool().await);
    let config = bikesnest_infrastructure::Config {
        google_oauth_enabled,
        ..test_config()
    };
    let deps = RouterDeps {
        email: std::sync::Arc::new(email.clone()),
        oauth: Some(FakeOAuthProvider::new(
            "oauth.user@example.com",
            "sub-oauth-1",
        )),
        hasher: TestPasswordHasher,
        rate_limiter: Box::new(bikesnest_infrastructure::InMemoryRateLimiter::new()),
        storage: std::sync::Arc::new(bikesnest_test_support::TestObjectStorage::new()),
    };
    let app = app_router_with(std::sync::Arc::new(config), db, deps);
    (app, email)
}

/// Like [`auth_app`], but also hands back the concrete storage double so a
/// test can read object bytes directly — there is no `/media` route to fetch
/// them through (media is served via direct S3 presigned URLs; the app is
/// never a media proxy).
async fn auth_app_with_storage() -> (
    axum::Router,
    FakeEmailProvider,
    std::sync::Arc<bikesnest_test_support::TestObjectStorage>,
) {
    let email = FakeEmailProvider::with_root(None);
    let db = Db::from_pool(pool().await);
    let config = test_config();
    let storage = std::sync::Arc::new(bikesnest_test_support::TestObjectStorage::new());
    let deps = RouterDeps {
        email: std::sync::Arc::new(email.clone()),
        oauth: Some(FakeOAuthProvider::new(
            "oauth.user@example.com",
            "sub-oauth-1",
        )),
        hasher: TestPasswordHasher,
        rate_limiter: Box::new(bikesnest_infrastructure::InMemoryRateLimiter::new()),
        storage: storage.clone(),
    };
    let app = app_router_with(std::sync::Arc::new(config), db, deps);
    (app, email, storage)
}

async fn get_c(app: &axum::Router, uri: &str, cookie: Option<&str>) -> (StatusCode, String) {
    let mut b = Request::builder().uri(uri).header("Accept-Language", "en");
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).to_string())
}

/// Which page to GET to obtain the anonymous double-submit CSRF cookie for a
/// given POST route (the form with the hidden `csrf` lives there).
fn anon_source_for(uri: &str) -> Option<&str> {
    if uri.starts_with("/password-reset/new") {
        Some("/password-reset/new")
    } else if uri.starts_with("/password-reset") {
        Some("/password-reset")
    } else if uri.starts_with("/verify-email/resend") {
        Some("/verify-email")
    } else if uri.starts_with("/register") {
        Some("/register")
    } else if uri.starts_with("/login") {
        Some("/login")
    } else {
        None
    }
}

/// GET `page_uri` and return `(csrf cookie line, token)` — the double-submit
/// cookie the CSRF middleware requires on anonymous POSTs.
async fn anon_csrf(app: &axum::Router, page_uri: &str) -> Option<(String, String)> {
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(page_uri)
                .header("Accept-Language", "en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let sc = res
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)?;
    if !sc.starts_with("csrf=") {
        return None;
    }
    let cookie_line = sc.split(';').next().unwrap().to_string();
    let token = cookie_line.split('=').nth(1).unwrap().to_string();
    Some((cookie_line, token))
}

/// The headers htmx 4 puts on a request whose target is a real element (see
/// `#createCoreHeaders` + the `HX-Request-Type` assignment in
/// web/static/vendor/htmx.js). WP10: the fragment endpoints answer a request
/// *without* them with a 303 to the page, so every test that asserts on a
/// partial has to send them.
const HX_FRAGMENT: &[(&str, &str)] = &[("HX-Request", "true"), ("HX-Request-Type", "partial")];

async fn post_form(
    app: &axum::Router,
    uri: &str,
    fields: &[(&str, &str)],
    cookie: Option<&str>,
) -> (StatusCode, String, Option<String>) {
    post_form_h(app, uri, fields, cookie, &[]).await
}

/// [`post_form`] as htmx issues it for a fragment swap.
async fn post_form_hx(
    app: &axum::Router,
    uri: &str,
    fields: &[(&str, &str)],
    cookie: Option<&str>,
) -> (StatusCode, String, Option<String>) {
    post_form_h(app, uri, fields, cookie, HX_FRAGMENT).await
}

async fn post_form_h(
    app: &axum::Router,
    uri: &str,
    fields: &[(&str, &str)],
    cookie: Option<&str>,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, String, Option<String>) {
    let mut all_fields: Vec<(String, String)> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut req_cookie = cookie.map(str::to_string);

    // Anonymous POSTs carry the double-submit CSRF cookie + matching form field.
    if cookie.is_none()
        && let Some(src) = anon_source_for(uri)
        && let Some((cookie_line, token)) = anon_csrf(app, src).await
    {
        req_cookie = Some(cookie_line);
        all_fields.push(("csrf".to_string(), token));
    }

    let body = all_fields
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("Accept-Language", "en");
    for (k, v) in extra_headers {
        b = b.header(*k, *v);
    }
    if let Some(c) = req_cookie {
        b = b.header("cookie", c);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        String::from_utf8_lossy(&body).to_string(),
        set_cookie,
    )
}

#[db_test]
async fn register_verify_login_account_logout(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "flow@example.com";
    cleanup_user(EMAIL).await;

    // Register → redirect to login?registered=1, verification email captured.
    let (s, _, _) = post_form(
        &app,
        "/register",
        &[
            ("email", EMAIL),
            ("display_name", "Flow"),
            ("password", "password123"),
        ],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let token = email
        .token_for("/verify-email")
        .expect("verification email captured");
    assert!(!token.is_empty());

    // Pending account logs in but /account shows the unverified banner.
    let (s_login, _, cookie) = post_form(
        &app,
        "/login",
        &[("email", EMAIL), ("password", "password123")],
        None,
    )
    .await;
    assert_eq!(
        s_login,
        StatusCode::SEE_OTHER,
        "login redirects to /account"
    );
    assert!(
        cookie.as_deref().unwrap_or("").contains("session_id="),
        "login sets a session cookie"
    );
    let (_, account_before) = get_c(&app, "/account", cookie.as_deref()).await;
    assert!(
        account_before.contains("Verify your email to contribute"),
        "unverified banner present"
    );
    assert!(account_before.contains(EMAIL));

    // Verify via the email link, then log in again (verified).
    let (s, _) = get_c(&app, &format!("/verify-email?token={token}"), None).await;
    assert_eq!(s, StatusCode::SEE_OTHER, "verify redirects to login");
    let (_, _, cookie2) = post_form(
        &app,
        "/login",
        &[("email", EMAIL), ("password", "password123")],
        None,
    )
    .await;
    let cookie = cookie2.unwrap().split(';').next().unwrap().to_string();
    let (_, account_after) = get_c(&app, "/account", Some(&cookie)).await;
    assert!(
        !account_after.contains("Verify your email to contribute"),
        "banner gone after verification"
    );

    // Logout clears the session → /account redirects to login.
    let csrf = extract_csrf(&account_after);
    assert!(!csrf.is_empty(), "account page embeds the CSRF token");
    let (s, _, _) = post_form(&app, "/logout", &[("csrf", &csrf)], Some(&cookie)).await;
    assert!(matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND));
    let (s, _) = get_c(&app, "/account", Some(&cookie)).await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
        "logged-out user is redirected"
    );

    let _ = tx;
    cleanup_user(EMAIL).await;
}

#[db_test]
async fn resend_verification_from_account_page_succeeds(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _email) = auth_app().await;
    const EMAIL: &str = "resend-account@example.com";
    let cookie = unverified_cookie(&app, EMAIL).await;

    let (_, account_body) = get_c(&app, "/account", Some(&cookie)).await;
    assert!(
        account_body.contains("Verify your email to contribute"),
        "unverified banner present"
    );
    let csrf = extract_csrf(&account_body);
    assert!(!csrf.is_empty(), "account page embeds the CSRF token");

    // The account-page resend form now carries the session's CSRF token
    // (previously missing, which made this authenticated POST fail CSRF with
    // 403 instead of succeeding like the anonymous /verify-email form).
    let (s, _, _) = post_form(
        &app,
        "/verify-email/resend",
        &[("csrf", &csrf), ("email", EMAIL)],
        Some(&cookie),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::SEE_OTHER,
        "resend-verification POST from /account must succeed, not 403"
    );

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn privacy_public_pages_gating_and_export_flow(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "privacy-web@example.com";
    cleanup_user(EMAIL).await;

    // Public legal pages render (200), even with placeholder content.
    let (s, body) = get_c(&app, "/privacy", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Privacy policy"));
    let (s, _) = get_c(&app, "/terms", None).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = get_c(&app, "/cookies", None).await;
    assert_eq!(s, StatusCode::OK);

    // : the sign-up form links the terms + privacy policy next to the button.
    let (s, body) = get_c(&app, "/register", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("href=\"/terms\"") && body.contains("href=\"/privacy\""),
        "register page must link the policies"
    );
    assert!(body.contains("18 or older"));

    // Authenticated+admin surfaces redirect anonymous users to login.
    let (s, _) = get_c(&app, "/account/privacy", None).await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
        "privacy hub requires auth"
    );
    let (s, _) = get_c(&app, "/admin/privacy-requests", None).await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
        "admin queue requires login"
    );

    // Register -> verify -> login.
    post_form(
        &app,
        "/register",
        &[("email", EMAIL), ("password", "password123")],
        None,
    )
    .await;
    let token = email
        .token_for("/verify-email")
        .expect("verification email captured");
    get_c(&app, &format!("/verify-email?token={token}"), None).await;
    let (_, _, cookie) = post_form(
        &app,
        "/login",
        &[("email", EMAIL), ("password", "password123")],
        None,
    )
    .await;
    let cookie = cookie.unwrap().split(';').next().unwrap().to_string();

    // C6 hub renders with a CSRF token.
    let (s, body) = get_c(&app, "/account/privacy", Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Export your data"));
    let csrf = extract_csrf(&body);

    // Request an export: POST -> 303 redirect to /account/export/{id}, with the
    // single-use token in a path-scoped cookie rather than the URL.
    let req = Request::builder()
        .method("POST")
        .uri("/account/privacy/export")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", &cookie)
        .header("Accept-Language", "en")
        .body(Body::from(format!("csrf={csrf}")))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        res.status(),
        StatusCode::SEE_OTHER,
        "export request redirects"
    );
    let loc = res
        .headers()
        .get(axum::http::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        loc.starts_with("/account/export/"),
        "redirect carries the export link"
    );
    // The download token must NOT be in the URL: a redirect target is recorded
    // in the browser's history, leaks through `Referer`, and lands in every
    // proxy and access log on the way. It comes back in a path-scoped
    // HttpOnly cookie instead.
    assert!(
        !loc.contains("token="),
        "the export token must not travel in the query string: {loc}"
    );
    let export_cookie = res
        .headers()
        .get_all(axum::http::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("export_"))
        .expect("the response must set the export token cookie")
        .to_string();
    assert!(
        export_cookie.contains("HttpOnly")
            && export_cookie.contains("Secure")
            && export_cookie.contains("SameSite=Lax")
            && export_cookie.contains(&format!("Path={loc}")),
        "export cookie must be HttpOnly, Secure, SameSite=Lax and scoped to \
         the export's own path: {export_cookie}"
    );
    // Both cookies, as a browser on that path would send them.
    let export_pair = export_cookie.split(';').next().unwrap().to_string();
    let with_export = format!("{cookie}; {export_pair}");

    // The C7 page renders the export status (Ready) and the download link —
    // with no token in the href, because the browser attaches the cookie.
    let (s, body) = get_c(&app, &loc, Some(&with_export)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Ready") || body.contains("Downloaded") || body.contains("Expired"));
    let download_uri = format!("{loc}/download");
    assert!(
        body.contains(&format!("href=\"{download_uri}\"")),
        "export page must render a tokenless download link: {body}"
    );

    // Without the export cookie there is no token at all → the download is
    // refused (the owner session alone is not enough).
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&download_uri)
                .header("cookie", &cookie)
                .header("Accept-Language", "en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        res.status().is_client_error() || res.status().is_redirection(),
        "a download with no token must not succeed, got {}",
        res.status()
    );
    assert_ne!(res.status(), StatusCode::OK);

    // With the cookie, the single-use download returns JSON with attachment
    // headers.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(&download_uri)
                .header("cookie", &with_export)
                .header("Accept-Language", "en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        StatusCode::OK,
        "download succeeds for the owner carrying the export cookie"
    );
    assert_eq!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/json; charset=utf-8"),
    );

    // A non-admin (this user is USER only) gets 403 on the admin queue.
    let (s, _) = get_c(&app, "/admin/privacy-requests", Some(&cookie)).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "non-admin blocked from the privacy-request queue"
    );

    let _ = tx;
    cleanup_user(EMAIL).await;
}

async fn cleanup_user(email: &str) {
    sqlx::query("DELETE FROM users WHERE email = $1")
        .bind(email)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn login_failure_body_is_identical_for_unknown_and_existing(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, _) = auth_app().await;
    cleanup_user("known@example.com").await;
    post_form(
        &app,
        "/register",
        &[("email", "known@example.com"), ("password", "password123")],
        None,
    )
    .await;

    let (s_known, b_known, _) = post_form(
        &app,
        "/login",
        &[("email", "known@example.com"), ("password", "wrong")],
        None,
    )
    .await;
    let (s_unknown, b_unknown, _) = post_form(
        &app,
        "/login",
        &[("email", "ghost@example.com"), ("password", "whatever")],
        None,
    )
    .await;
    assert_eq!(s_known, s_unknown, "same status for known and unknown");
    assert!(
        b_known.contains("Email or password is incorrect"),
        "generic message"
    );
    assert!(b_unknown.contains("Email or password is incorrect"));
    // No-account-existence leakage: neither response echoes the submitted
    // email, so an attacker learns nothing from the body. (The pages differ only
    // by the per-request CSRF token, which is unrelated to account existence.)
    assert!(!b_known.contains("known@example.com"));
    assert!(!b_unknown.contains("ghost@example.com"));
    let _ = tx;
    cleanup_user("known@example.com").await;
}

#[db_test]
async fn admin_users_denied_for_anonymous_and_non_admin(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    cleanup_user("admin-user@example.com").await;

    // Anonymous → redirected to login.
    let (s, _) = get_c(&app, "/admin/users", None).await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
        "anonymous redirected: {s}"
    );

    // Logged-in non-admin → 403.
    post_form(
        &app,
        "/register",
        &[
            ("email", "admin-user@example.com"),
            ("password", "password123"),
        ],
        None,
    )
    .await;
    let (_, _, cookie) = post_form(
        &app,
        "/login",
        &[
            ("email", "admin-user@example.com"),
            ("password", "password123"),
        ],
        None,
    )
    .await;
    let (s, _) = get_c(&app, "/admin/users", cookie.as_deref()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "non-admin forbidden");
    let _ = tx;
    cleanup_user("admin-user@example.com").await;
}

#[db_test]
async fn admin_can_grant_role_and_audit_is_written(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    cleanup_user("root@example.com").await;
    cleanup_user("target@example.com").await;

    // Seed a logged-in admin + a target user.
    post_form(
        &app,
        "/register",
        &[("email", "root@example.com"), ("password", "password123")],
        None,
    )
    .await;
    let (_, _, admin_cookie) = post_form(
        &app,
        "/login",
        &[("email", "root@example.com"), ("password", "password123")],
        None,
    )
    .await;
    let admin_cookie = admin_cookie.unwrap().split(';').next().unwrap().to_string();
    post_form(
        &app,
        "/register",
        &[("email", "target@example.com"), ("password", "password123")],
        None,
    )
    .await;

    let (root_id,): (i64,) =
        sqlx::query_as("SELECT id FROM users WHERE email = 'root@example.com'")
            .fetch_one(&pool().await)
            .await
            .unwrap();
    let (target_id,): (i64,) =
        sqlx::query_as("SELECT id FROM users WHERE email = 'target@example.com'")
            .fetch_one(&pool().await)
            .await
            .unwrap();
    // Granting ADMIN changes a system-wide set that the last-admin guard reads,
    // and other test binaries assert on it. Claim the shared lock first.
    bikesnest_test_support::hold_admin_set_lock_for_process(&pool().await).await;
    sqlx::query("INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'ADMIN', NULL)")
        .bind(root_id)
        .execute(&pool().await)
        .await
        .unwrap();

    // Admin opens the user list.
    let (s, body) = get_c(&app, "/admin/users", Some(&admin_cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("target@example.com"));

    // Grant MODERATOR. Need the CSRF token from the page.
    let csrf = extract_csrf(&body);
    let (s, _, _) = post_form(
        &app,
        &format!("/admin/users/{target_id}/role"),
        &[("csrf", &csrf), ("action", "grant"), ("role", "MODERATOR")],
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);

    let (audit_count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_events WHERE action = 'role.granted' AND target_id = $1",
    )
    .bind(target_id.to_string())
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(audit_count, 1, "granted role is audited");

    let (role_count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM user_roles WHERE user_id = $1 AND role = 'MODERATOR'")
            .bind(target_id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(role_count, 1);

    let _ = tx;
    cleanup_user("root@example.com").await;
    cleanup_user("target@example.com").await;
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf" content=""#;
    let start = html.find(marker).map(|i| i + marker.len()).unwrap_or(0);
    html[start..].split('"').next().unwrap_or("").to_string()
}

#[db_test]
async fn csrf_required_on_authenticated_post(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    cleanup_user("csrf@example.com").await;
    post_form(
        &app,
        "/register",
        &[("email", "csrf@example.com"), ("password", "password123")],
        None,
    )
    .await;
    let (_, _, cookie) = post_form(
        &app,
        "/login",
        &[("email", "csrf@example.com"), ("password", "password123")],
        None,
    )
    .await;
    let cookie = cookie.unwrap().split(';').next().unwrap().to_string();

    // POST /logout with a valid session but NO CSRF token → 403.
    let (s, _, _) = post_form(&app, "/logout", &[], Some(&cookie)).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "missing CSRF token on authenticated POST is forbidden"
    );

    // With the correct CSRF token (from /account) it succeeds.
    let (_, account) = get_c(&app, "/account", Some(&cookie)).await;
    let csrf = extract_csrf(&account);
    let (s, _, _) = post_form(&app, "/logout", &[("csrf", &csrf)], Some(&cookie)).await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
        "logout with CSRF succeeds"
    );
    let _ = tx;
    cleanup_user("csrf@example.com").await;
}

#[db_test]
async fn suspended_account_is_blocked_at_login_with_generic_error(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, _) = auth_app().await;
    cleanup_user("suspend@example.com").await;
    post_form(
        &app,
        "/register",
        &[
            ("email", "suspend@example.com"),
            ("password", "password123"),
        ],
        None,
    )
    .await;

    sqlx::query("UPDATE users SET account_state = 'SUSPENDED' WHERE email = 'suspend@example.com'")
        .execute(&pool().await)
        .await
        .unwrap();

    let (_, body, _) = post_form(
        &app,
        "/login",
        &[
            ("email", "suspend@example.com"),
            ("password", "password123"),
        ],
        None,
    )
    .await;
    assert!(
        body.contains("Email or password is incorrect"),
        "suspended logged out with generic message"
    );
    let _ = tx;
    cleanup_user("suspend@example.com").await;
}

#[db_test]
async fn anonymous_post_without_csrf_cookie_is_forbidden(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    // A POST with no `csrf` cookie (here the session cookie alone) is rejected on
    // the anonymous path — SameSite=Lax alone is not treated as CSRF-safe.
    let (s, _, _) = post_form(
        &app,
        "/login",
        &[("email", "x@example.com"), ("password", "password123")],
        Some("session_id=some-invalid-session"),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "anonymous POST without csrf cookie is forbidden"
    );
    let _ = tx;
}

#[db_test]
async fn csrf_header_path_is_accepted(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    cleanup_user("hdr@example.com").await;
    post_form(
        &app,
        "/register",
        &[("email", "hdr@example.com"), ("password", "password123")],
        None,
    )
    .await;
    let (_, _, cookie) = post_form(
        &app,
        "/login",
        &[("email", "hdr@example.com"), ("password", "password123")],
        None,
    )
    .await;
    let cookie = cookie.unwrap().split(';').next().unwrap().to_string();
    let (_, account) = get_c(&app, "/account", Some(&cookie)).await;
    let csrf = extract_csrf(&account);

    // Authenticated POST via the X-CSRF-Token HEADER (the htmx path) is accepted.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/logout")
                .header("cookie", &cookie)
                .header("accept-language", "en")
                .header("x-csrf-token", &csrf)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        matches!(res.status(), StatusCode::SEE_OTHER | StatusCode::FOUND),
        "header-path CSRF accepted"
    );
    let _ = tx;
    cleanup_user("hdr@example.com").await;
}

// ---------------------------------------------------------------------------
// WP1: Google sign-in disabled by default (product decision: disable, do not
// implement real OAuth). `FakeOAuthProvider` still exists for opt-in dev use,
// but the routes are unregistered unless `GOOGLE_OAUTH_ENABLED` is set.
// ---------------------------------------------------------------------------

#[db_test]
async fn google_oauth_disabled_by_default_returns_404(_tx: &mut TestTx) {
    let app = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/auth/google")
                .header("Accept-Language", "en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
    assert!(
        res.headers().get("set-cookie").is_none(),
        "a disabled/unregistered route must not set any cookie"
    );
}

#[db_test]
async fn google_oauth_disabled_callback_returns_404(_tx: &mut TestTx) {
    let (status, _) = get("/auth/google/callback?code=x&state=y").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[db_test]
async fn login_page_hides_google_link_when_disabled(_tx: &mut TestTx) {
    let (status, body) = get("/login").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("href=\"/auth/google\""),
        "no live Google link when the feature is disabled"
    );
    assert!(
        body.contains("Coming soon"),
        "disabled button shows the coming-soon copy"
    );
}

#[db_test]
async fn google_oauth_enabled_flag_still_redirects(_tx: &mut TestTx) {
    // Protects the future real-integration plumbing: with the flag explicitly
    // on, `/auth/google` is registered and behaves as before (redirect to the
    // provider's authorize URL — the fake's consent stub in this test build).
    let (app, _) = auth_app_opts(true).await;
    let (status, _) = get_c(&app, "/auth/google", None).await;
    assert!(
        matches!(
            status,
            StatusCode::SEE_OTHER | StatusCode::FOUND | StatusCode::TEMPORARY_REDIRECT
        ),
        "enabled Google route redirects, got {status}"
    );
}

// ---------------------------------------------------------------------------
// M3 community routes
// ---------------------------------------------------------------------------

async fn cleanup_user_contributions(email: &str) {
    sqlx::query(
        "DELETE FROM parking_location WHERE creator_id = (SELECT id FROM users WHERE email = $1)",
    )
    .bind(email)
    .execute(&pool().await)
    .await
    .unwrap();
    sqlx::query("DELETE FROM users WHERE email = $1")
        .bind(email)
        .execute(&pool().await)
        .await
        .unwrap();
}

async fn unverified_cookie(app: &axum::Router, addr: &str) -> String {
    cleanup_user_contributions(addr).await;
    post_form(
        app,
        "/register",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    let (_, _, cookie) = post_form(
        app,
        "/login",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    cookie.unwrap().split(';').next().unwrap().to_string()
}

async fn verified_cookie(
    app: &axum::Router,
    email: &bikesnest_infrastructure::FakeEmailProvider,
    addr: &str,
) -> String {
    cleanup_user_contributions(addr).await;
    post_form(
        app,
        "/register",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    let token = email
        .token_for("/verify-email")
        .expect("verification email captured");
    get_c(app, &format!("/verify-email?token={token}"), None).await;
    let (_, _, cookie) = post_form(
        app,
        "/login",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    cookie.unwrap().split(';').next().unwrap().to_string()
}

#[db_test]
async fn community_routes_redirect_anonymous(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    for uri in [
        "/parking/new",
        "/parking/1/edit",
        "/parking/1/review",
        "/account/favorites",
        "/account/contributions",
    ] {
        let (s, _) = get_c(&app, uri, None).await;
        assert!(
            matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
            "{uri} redirects anonymous: {s}"
        );
    }
    let _ = tx;
}

#[db_test]
async fn add_location_requires_verified(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    let cookie = unverified_cookie(&app, "unverified-contrib@example.com").await;
    let (s, _) = get_c(&app, "/parking/new", Some(&cookie)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "unverified cannot open add form");

    // Even with a forged CSRF, an unverified POST is rejected with 403.
    let (s, _, _) = post_form(
        &app,
        "/parking/new",
        &[
            ("csrf", "bogus"),
            ("name", "X"),
            ("address", "Y"),
            ("parking_type", "rack"),
            ("lat", "-25.42"),
            ("lon", "-49.27"),
            ("timezone", "America/Sao_Paulo"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "unverified POST denied");
    let _ = tx;
    cleanup_user_contributions("unverified-contrib@example.com").await;
}

#[db_test]
async fn verified_user_adds_a_location_and_sees_details(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "contrib-add@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;

    let (s, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK, "verified user opens add form");
    let csrf = extract_csrf(&form);
    assert!(!csrf.is_empty());

    let (s, _, _) = post_form(
        &app,
        "/parking/new",
        &[
            ("csrf", &csrf),
            ("name", "Estação Centro Added"),
            ("address", "Rua X, 1"),
            ("parking_type", "rack"),
            ("cost_kind", "unknown"),
            ("lat", "-25.4284"),
            ("lon", "-49.2733"),
            ("timezone", "America/Sao_Paulo"),
            ("sec_well_lit", "yes"),
            ("confirm", "1"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::SEE_OTHER,
        "add redirects to the new location: {s}"
    );

    // The location is persisted with creator attribution + version 1.
    let (id,): (i64,) = sqlx::query_as(
        "SELECT id FROM parking_location WHERE name = 'Estação Centro Added' ORDER BY id DESC LIMIT 1",
    ).fetch_one(&pool().await).await.unwrap();
    assert!(id > 0);
    let (version,): (i64,) = sqlx::query_as("SELECT version FROM parking_location WHERE id = $1")
        .bind(id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(version, 1);

    // The P3 details page renders.
    let (s, body) = get_c(&app, &format!("/parking/{id}"), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Estação Centro Added"));

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn favorite_toggle_and_list_work_for_authenticated_user(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const MARK: &str = "fix-http-fav";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    // Fixture location (committed) so the favorite repo can reference it.
    let loc = ParkingBuilder::new()
        .with_name("Favorite Target")
        .with_fixture_tag(MARK)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    let loc_id = loc.id();

    let (app, _) = auth_app().await;
    let cookie = unverified_cookie(&app, "fav-user@example.com").await;

    // GET the details page to grab CSRF, then toggle favorite (auth-only).
    let (s, page) = get_c(&app, &format!("/parking/{loc_id}"), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    let csrf = extract_csrf(&page);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/parking/{loc_id}/favorite"),
        &[("csrf", &csrf)],
        Some(&cookie),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "favorite toggle succeeds for authenticated user"
    );

    // C4 lists the favorited location.
    let (s, body) = get_c(&app, "/account/favorites", Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("Favorite Target"),
        "favorites page lists the spot"
    );

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("fav-user@example.com").await;
}

// ---------------------------------------------------------------------------
// Added coverage — edit prefill/data-loss, revision in C5,
// pin-move→PENDING, review aggregate, rate-limit→429, identity absence.
// ---------------------------------------------------------------------------

/// POST /parking/new for a verified session; returns the created location id.
async fn add_location(
    app: &axum::Router,
    cookie: &str,
    csrf: &str,
    name: &str,
    extra: &[(&str, &str)],
) -> i64 {
    let mut fields: Vec<(String, String)> = vec![
        ("csrf".into(), csrf.into()),
        ("name".into(), name.into()),
        ("address".into(), "Rua X, 1".into()),
        ("parking_type".into(), "rack".into()),
        // Serrra da Cantareira area — well away from the Curitiba seed data.
        ("lat".into(), "-23.4".into()),
        ("lon".into(), "-46.6".into()),
        ("timezone".into(), "America/Sao_Paulo".into()),
    ];
    // cost_kind defaults to unknown; the caller may override it via `extra`.
    if !extra.iter().any(|(k, _)| *k == "cost_kind") {
        fields.push(("cost_kind".into(), "unknown".into()));
    }
    // `confirm=1` skips the pre-create duplicate interstitial: this helper's
    // job is "a spot now exists", and without it a submission that resembles
    // one an earlier test left behind answers 200 with the interstitial
    // instead of the 303 the callers assert on.
    if !extra.iter().any(|(k, _)| *k == "confirm") {
        fields.push(("confirm".into(), "1".into()));
    }
    fields.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let refs: Vec<(&str, &str)> = fields
        .iter()
        .map(|(k, v)| (k.as_str(), v.as_str()))
        .collect();
    let (s, _, _) = post_form(app, "/parking/new", &refs, Some(cookie)).await;
    assert_eq!(s, StatusCode::SEE_OTHER, "add should redirect: {s}");
    let (id,): (i64,) =
        sqlx::query_as("SELECT id FROM parking_location WHERE name = $1 ORDER BY id DESC LIMIT 1")
            .bind(name)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    id
}

#[db_test]
async fn edit_preserves_cost_security_and_hours(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "edit-prefill@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    // Create with a paid price + a security attribute + all-day hours.
    let id = add_location(
        &app,
        &cookie,
        &csrf,
        "Prefill Spot",
        &[
            ("cost_kind", "paid"),
            ("price", "1"),
            ("price_currency", "BRL"),
            ("price_unit", "hour"),
            ("sec_well_lit", "yes"),
            ("h_mon_state", "all_day"),
            ("h_tue_state", "all_day"),
            ("h_wed_state", "all_day"),
            ("h_thu_state", "all_day"),
            ("h_fri_state", "all_day"),
            ("h_sat_state", "all_day"),
            ("h_sun_state", "all_day"),
        ],
    )
    .await;

    // The edit page must pre-fill those values.
    let (s, edit_html) = get_c(&app, &format!("/parking/{id}/edit"), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        edit_html.contains(r#"value="paid" selected"#),
        "cost pre-filled, not defaulted to free"
    );
    assert!(
        edit_html.contains(r#"name="sec_well_lit" value="yes" class="peer sr-only" checked"#),
        "the security radio pre-selects yes: {edit_html}"
    );
    assert!(
        edit_html.contains(r#"value="all_day" selected"#),
        "the hours editor pre-selects the stored state"
    );

    // Editing only the name must NOT reset cost/security/hours.
    let edit_csrf = extract_csrf(&edit_html);
    let (s, _, _) = post_form(
        &app,
        &format!("/parking/{id}/edit"),
        &[
            ("csrf", &edit_csrf),
            ("version", "1"),
            ("name", "Prefill Spot Renamed"),
            ("address", "Rua X, 1"),
            ("parking_type", "rack"),
            ("cost_kind", "paid"),
            ("price", "1"),
            ("price_currency", "BRL"),
            ("price_unit", "hour"),
            ("sec_well_lit", "yes"),
            ("h_mon_state", "all_day"),
            ("h_tue_state", "all_day"),
            ("h_wed_state", "all_day"),
            ("h_thu_state", "all_day"),
            ("h_fri_state", "all_day"),
            ("h_sat_state", "all_day"),
            ("h_sun_state", "all_day"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);

    let (name, cost_kind, price_cents, version): (String, String, Option<i64>, i64) =
        sqlx::query_as(
            "SELECT name, cost_kind, price_cents, version FROM parking_location WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(name, "Prefill Spot Renamed");
    assert_eq!(cost_kind, "paid", "cost not reset to free");
    assert_eq!(price_cents, Some(100), "price preserved");
    assert_eq!(version, 2, "optimistic bump");
    let (sec,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM parking_security WHERE location_id = $1 AND state = 1 AND feature_code = 'well_lit'")
        .bind(id).fetch_one(&pool().await).await.unwrap();
    assert_eq!(sec, 1, "security preserved");

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn edit_writes_revision_visible_in_contributions(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "edit-c5@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "Revision Spot", &[]).await;

    let (_, edit_html) = get_c(&app, &format!("/parking/{id}/edit"), Some(&cookie)).await;
    let edit_csrf = extract_csrf(&edit_html);
    let (s, _, _) = post_form(
        &app,
        &format!("/parking/{id}/edit"),
        &[
            ("csrf", &edit_csrf),
            ("version", "1"),
            ("name", "Revision Spot 2"),
            ("address", "Rua X, 1"),
            ("parking_type", "rack"),
            ("cost_kind", "unknown"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);

    // The revision row is recorded and C5 shows the edit.
    let (rev,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM parking_revision WHERE location_id = $1 AND change_kind = 'edit'",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(rev, 1, "edit revision recorded");
    let (s, body) = get_c(&app, "/account/contributions", Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Revision Spot 2"), "C5 lists the edited spot");
    assert!(body.contains("Edited"), "C5 shows the edit kind");

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn proposing_a_move_creates_pending_proposal(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "proposal@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "Move Spot", &[]).await;

    let (_, edit_html) = get_c(&app, &format!("/parking/{id}/edit"), Some(&cookie)).await;
    let ecsrf = extract_csrf(&edit_html);
    let (s, _, _) = post_form(
        &app,
        &format!("/parking/{id}/proposal"),
        &[
            ("csrf", &ecsrf),
            ("kind", "move_location"),
            ("lat", "-25.0"),
            ("lon", "-49.0"),
            ("timezone", "America/Sao_Paulo"),
            ("reason", "moved nearby"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);

    // A PENDING proposal exists; the location is unchanged (still version 1).
    let (status,): (String,) = sqlx::query_as(
        "SELECT status FROM parking_proposal WHERE location_id = $1 AND kind = 'move_location'",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(status, "PENDING");
    let (version,): (i64,) = sqlx::query_as("SELECT version FROM parking_location WHERE id = $1")
        .bind(id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(version, 1, "proposal causes no live change");

    // Following the redirect shows the "will be reviewed" confirmation.
    let (s, body) = get_c(&app, &format!("/parking/{id}?proposed=1"), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("will be reviewed by a moderator"),
        "proposal confirmation shown"
    );

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn review_create_updates_aggregate(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "review-agg@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "Review Spot", &[]).await;

    let (_, review_form) = get_c(&app, &format!("/parking/{id}/review"), Some(&cookie)).await;
    let rcsrf = extract_csrf(&review_form);
    let rbody = multipart_review("4", "Great rack", "----bikesnestphoto");
    let (s, _) = post_multipart(
        &app,
        &format!("/parking/{id}/review"),
        rbody,
        &cookie,
        Some(&rcsrf),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);

    let (count, avg): (i32, Option<f64>) = sqlx::query_as(
        "SELECT rating_count, rating_avg::float8 FROM parking_location WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert!(
        (avg.unwrap() - 4.0).abs() < 0.001,
        "rating aggregate updated"
    );
    let (rev,): (i64,) = sqlx::query_as("SELECT count(*) FROM review_revision WHERE review_id IN (SELECT id FROM review WHERE location_id = $1)")
        .bind(id).fetch_one(&pool().await).await.unwrap();
    assert_eq!(rev, 1, "review revision recorded");

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn parking_create_is_rate_limited(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "ratelimit@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    // parking-create: 5/day per user. Post 5, then the 6th → 429.
    // Each add carries `confirm=1`, so the pre-create duplicate interstitial
    // never stands in for the redirect (or for the 429).
    for i in 0..5 {
        let lat = -23.4 - i as f64 * 0.01;
        let name = format!("RateSpot{n}{i}", n = i); // distinct low-similarity names
        let (s, _, _) = post_form(
            &app,
            "/parking/new",
            &[
                ("csrf", &csrf),
                ("name", &name),
                ("address", "Rua X, 1"),
                ("parking_type", "rack"),
                ("cost_kind", "unknown"),
                ("lat", &lat.to_string()),
                ("lon", "-46.6"),
                ("timezone", "America/Sao_Paulo"),
                ("confirm", "1"),
            ],
            Some(&cookie),
        )
        .await;
        assert_eq!(s, StatusCode::SEE_OTHER, "add {i} should succeed");
    }
    let (s, _, _) = post_form(
        &app,
        "/parking/new",
        &[
            ("csrf", &csrf),
            ("name", "RateSpotFinal"),
            ("address", "Rua X, 1"),
            ("parking_type", "rack"),
            ("cost_kind", "unknown"),
            ("lat", "-23.4"),
            ("lon", "-46.6"),
            ("timezone", "America/Sao_Paulo"),
            ("confirm", "1"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS, "6th add is rate-limited");

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn no_identity_leak_in_rendered_html(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "identity@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "Identity Spot", &[]).await;

    let (user_id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(EMAIL)
        .fetch_one(&pool().await)
        .await
        .unwrap();

    // The details page, favorite/verify state and C5 must never render the
    // contributor's email, user id, or the OAuth subject (only counts/labels).
    let uris: Vec<String> = vec![
        format!("/parking/{id}"),
        "/account/contributions".to_string(),
        "/account/favorites".to_string(),
    ];
    for uri in uris {
        let (s, body) = get_c(&app, &uri, Some(&cookie)).await;
        assert_eq!(s, StatusCode::OK);
        assert!(!body.contains(EMAIL), "email leaked on {uri}");
        assert!(
            !body.contains(&format!(">{user_id}<")),
            "user id leaked on {uri}"
        );
    }

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn multiple_security_values_and_major_unit_price(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "multi-sec@example.com";
    const MARK: &str = "fix-http-multisec";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    // TWO security attributes (the source of the "duplicate field" crash) plus a
    // major-unit price ("1.50" must store as 150 cents, not raw text/cents).
    let id = add_location(
        &app,
        &cookie,
        &csrf,
        "Multi Security Spot",
        &[
            ("cost_kind", "paid"),
            ("price", "1.50"),
            ("price_currency", "BRL"),
            ("price_unit", "hour"),
            ("sec_well_lit", "yes"),
            ("sec_cctv", "yes"),
        ],
    )
    .await;

    let (cost_kind, price_cents, version): (String, Option<i64>, i64) = sqlx::query_as(
        "SELECT cost_kind, price_cents, version FROM parking_location WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(cost_kind, "paid");
    assert_eq!(price_cents, Some(150), "1.50 majors -> 150 cents");
    assert_eq!(version, 1);

    // Both security attributes recorded as YES.
    let (yes,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM parking_security WHERE location_id = $1 AND state = 1 AND feature_code IN ('well_lit','cctv')")
        .bind(id).fetch_one(&pool().await).await.unwrap();
    assert_eq!(yes, 2, "both security attributes stored");

    // Editing preserves both (no reset) and the price round-trips as majors.
    let (_, edit_html) = get_c(&app, &format!("/parking/{id}/edit"), Some(&cookie)).await;
    let ecsrf = extract_csrf(&edit_html);
    let (s, _, _) = post_form(
        &app,
        &format!("/parking/{id}/edit"),
        &[
            ("csrf", &ecsrf),
            ("version", "1"),
            ("name", "Multi Security Spot"),
            ("address", "Rua X, 1"),
            ("parking_type", "rack"),
            ("cost_kind", "paid"),
            ("price", "1.50"),
            ("price_currency", "BRL"),
            ("price_unit", "hour"),
            ("sec_well_lit", "yes"),
            ("sec_cctv", "yes"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let (yes,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM parking_security WHERE location_id = $1 AND state = 1 AND feature_code IN ('well_lit','cctv')")
        .bind(id).fetch_one(&pool().await).await.unwrap();
    assert_eq!(yes, 2, "security preserved through edit");

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(EMAIL).await;
}

// ---------------------------------------------------------------------------
// M4 photos — upload → queue → moderate → publish (//)
// ---------------------------------------------------------------------------

/// The origin `TestObjectStorage::presigned_get` signs every URL under (see
/// `bikesnest_infrastructure::TEST_MEDIA_ORIGIN`) — the same origin
/// `Config::for_tests` puts in `security.media_hosts`, so a gallery `<img
/// src>` found here and the CSP's `img-src` allowlist agree by construction.
const MEDIA_ORIGIN: &str = bikesnest_infrastructure::TEST_MEDIA_ORIGIN;

fn tiny_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(32, 32, image::Rgb([20, 40, 60]));
    let mut b = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut b)
        .encode_image(&img)
        .unwrap();
    b
}

/// Build a multipart body with a `photo` file field and optional `alt` text.
fn multipart_upload(jpeg: &[u8], alt: Option<&str>, boundary: &str) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"photo\"; filename=\"photo.jpg\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/jpeg\r\n\r\n");
    body.extend_from_slice(jpeg);
    body.extend_from_slice(b"\r\n");
    if let Some(alt) = alt {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(b"Content-Disposition: form-data; name=\"alt\"\r\n\r\n");
        body.extend_from_slice(alt.as_bytes());
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// Build a multipart review body (rating + body fields) using the test boundary.
fn multipart_review(rating: &str, body: &str, boundary: &str) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    b.extend_from_slice(b"Content-Disposition: form-data; name=\"rating\"\r\n\r\n");
    b.extend_from_slice(rating.as_bytes());
    b.extend_from_slice(b"\r\n");
    b.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    b.extend_from_slice(b"Content-Disposition: form-data; name=\"body\"\r\n\r\n");
    b.extend_from_slice(body.as_bytes());
    b.extend_from_slice(b"\r\n");
    b.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    b
}

async fn post_multipart(
    app: &axum::Router,
    uri: &str,
    body: Vec<u8>,
    cookie: &str,
    csrf: Option<&str>,
) -> (StatusCode, String) {
    post_multipart_h(app, uri, body, cookie, csrf, &[]).await
}

/// [`post_multipart`] as htmx issues it for a fragment swap.
async fn post_multipart_hx(
    app: &axum::Router,
    uri: &str,
    body: Vec<u8>,
    cookie: &str,
    csrf: Option<&str>,
) -> (StatusCode, String) {
    post_multipart_h(app, uri, body, cookie, csrf, HX_FRAGMENT).await
}

async fn post_multipart_h(
    app: &axum::Router,
    uri: &str,
    body: Vec<u8>,
    cookie: &str,
    csrf: Option<&str>,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, String) {
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header("accept", "text/html")
        .header("Accept-Language", "en")
        .header(
            "content-type",
            "multipart/form-data; boundary=----bikesnestphoto",
        )
        .header("cookie", cookie);
    for (k, v) in extra_headers {
        b = b.header(*k, *v);
    }
    if let Some(c) = csrf {
        b = b.header("X-CSRF-Token", c);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).to_string())
}

fn has_exif_marker(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return false;
    }
    let mut i = 2;
    while i + 1 < bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xDA {
            break;
        }
        if i + 3 >= bytes.len() {
            break;
        }
        let len = (u16::from(bytes[i + 2]) << 8) | u16::from(bytes[i + 3]);
        if marker == 0xE1 {
            return true;
        }
        i += 2 + len as usize;
    }
    false
}

/// A committed fixture location (no photos) for photo tests.
async fn fixture_location(tx: &mut bikesnest_test_support::TestTx, mark: &str, name: &str) -> i64 {
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(mark)
        .execute(&pool().await)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name(name)
        .with_fixture_tag(mark)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    loc.id()
}

async fn moderator_cookie(
    app: &axum::Router,
    email: &bikesnest_infrastructure::FakeEmailProvider,
    addr: &str,
) -> String {
    cleanup_user_contributions(addr).await;
    post_form(
        app,
        "/register",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    let token = email
        .token_for("/verify-email")
        .expect("moderator verification email");
    get_c(app, &format!("/verify-email?token={token}"), None).await;
    let (uid,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(addr)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'MODERATOR', NULL) ON CONFLICT DO NOTHING",
    )
    .bind(uid).execute(&pool().await).await.unwrap();
    let (_, _, cookie) = post_form(
        app,
        "/login",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    cookie.unwrap().split(';').next().unwrap().to_string()
}

#[db_test]
async fn photo_upload_requires_verified(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    let loc = fixture_location(tx, "photo-verify-gate", "Photo Verify Gate").await;
    let cookie = unverified_cookie(&app, "photo-unverified@example.com").await;
    let (s, page) = get_c(&app, &format!("/parking/{loc}"), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    let csrf = extract_csrf(&page);

    let (ctype, body) = multipart_upload(&tiny_jpeg(), None, "----bikesnestphoto");
    let _ = ctype;
    let (s, _) = post_multipart_hx(
        &app,
        &format!("/parking/{loc}/photo"),
        body,
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "unverified upload blocked");
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-verify-gate'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("photo-unverified@example.com").await;
}

#[db_test]
async fn photo_upload_missing_csrf_header_is_forbidden(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "photo-csrf-gate", "Photo Csrf Gate").await;
    let cookie = verified_cookie(&app, &email, "photo-csrf@example.com").await;
    // No X-CSRF-Token header on a multipart POST → the middleware cannot read a
    // body csrf field and rejects with 403.
    let (_, body) = multipart_upload(&tiny_jpeg(), None, "----bikesnestphoto");
    let (s, _) = post_multipart(&app, &format!("/parking/{loc}/photo"), body, &cookie, None).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "multipart without CSRF header denied"
    );
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-csrf-gate'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("photo-csrf@example.com").await;
}

#[db_test]
async fn verified_upload_enters_queue_not_gallery(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "photo-queue-gate", "Photo Queue Gate").await;
    let cookie = verified_cookie(&app, &email, "photo-uploader@example.com").await;
    let (s, page) = get_c(&app, &format!("/parking/{loc}"), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    let csrf = extract_csrf(&page);

    let (_, body) = multipart_upload(&tiny_jpeg(), Some("A first photo"), "----bikesnestphoto");
    let (s, _) = post_multipart_hx(
        &app,
        &format!("/parking/{loc}/photo"),
        body,
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "verified upload succeeds: {s}");

    // Row is PENDING_REVIEW with a thumbnail; not yet visible publicly.
    let (state, thumb): (String, Option<String>) = sqlx::query_as(
        "SELECT moderation_state, thumbnail_key FROM parking_photo WHERE location_id = $1",
    )
    .bind(loc)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(state, "PENDING_REVIEW");
    assert!(thumb.is_some(), "thumbnail derivative recorded");

    // The gallery (public) does not show a pending photo.
    let (s, gallery) = get_c(&app, &format!("/parking/{loc}"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !gallery.contains(&format!("{MEDIA_ORIGIN}/uploads/")),
        "pending photo not in gallery"
    );
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-queue-gate'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("photo-uploader@example.com").await;
}

#[db_test]
async fn moderation_routes_require_moderator_role(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    // Anonymous is redirected to login (require_role → require_user → redirect).
    let (s, _) = get_c(&app, "/moderation/photos", None).await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
        "anonymous redirected to login: {s}"
    );

    // A verified non-moderator is blocked from the queue and from approving.
    let cookie = verified_cookie(&app, &email, "photo-nonmod@example.com").await;
    let (s, _) = get_c(&app, "/moderation/photos", Some(&cookie)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "non-moderator cannot open queue");
    let (s, _, _) = post_form_hx(
        &app,
        "/moderation/photos/parking/1/approve",
        &[("csrf", "bogus")],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "non-moderator cannot approve");
    let _ = tx;
    cleanup_user_contributions("photo-nonmod@example.com").await;
}

#[db_test]
async fn moderator_approve_publishes_to_gallery(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "photo-approve", "Photo Approve").await;
    let uploader = verified_cookie(&app, &email, "photo-approve-up@example.com").await;
    let (_, page) = get_c(&app, &format!("/parking/{loc}"), Some(&uploader)).await;
    let csrf = extract_csrf(&page);
    let (_, body) = multipart_upload(&tiny_jpeg(), None, "----bikesnestphoto");
    post_multipart(
        &app,
        &format!("/parking/{loc}/photo"),
        body,
        &uploader,
        Some(&csrf),
    )
    .await;
    let (id,): (i64,) = sqlx::query_as("SELECT id FROM parking_photo WHERE location_id = $1")
        .bind(loc)
        .fetch_one(&pool().await)
        .await
        .unwrap();

    // The moderator opens the queue, grabs CSRF, and approves.
    let mod_cookie = moderator_cookie(&app, &email, "photo-moderator@example.com").await;
    let (s, queue) = get_c(&app, "/moderation/photos", Some(&mod_cookie)).await;
    assert_eq!(s, StatusCode::OK);
    let mcs = extract_csrf(&queue);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/photos/parking/{id}/approve"),
        &[("csrf", &mcs)],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "moderator approves: {s}");

    let (state,): (String,) =
        sqlx::query_as("SELECT moderation_state FROM parking_photo WHERE id = $1")
            .bind(id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "APPROVED");

    // Now the gallery serves the derivative.
    let (_, gallery) = get_c(&app, &format!("/parking/{loc}"), None).await;
    assert!(
        gallery.contains(&format!("{MEDIA_ORIGIN}/uploads/")),
        "approved photo appears in gallery"
    );
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-approve'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("photo-approve-up@example.com").await;
    cleanup_user_contributions("photo-moderator@example.com").await;
}

#[db_test]
async fn moderator_reject_deletes_object_and_hides(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email, storage) = auth_app_with_storage().await;
    let loc = fixture_location(tx, "photo-reject", "Photo Reject").await;
    let uploader = verified_cookie(&app, &email, "photo-reject-up@example.com").await;
    let (_, page) = get_c(&app, &format!("/parking/{loc}"), Some(&uploader)).await;
    let csrf = extract_csrf(&page);
    let (_, body) = multipart_upload(&tiny_jpeg(), None, "----bikesnestphoto");
    post_multipart(
        &app,
        &format!("/parking/{loc}/photo"),
        body,
        &uploader,
        Some(&csrf),
    )
    .await;
    let (id, full, thumb): (i64, String, Option<String>) = sqlx::query_as(
        "SELECT id, storage_key, thumbnail_key FROM parking_photo WHERE location_id = $1",
    )
    .bind(loc)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    let thumb = thumb.expect("upload writes a thumbnail");
    assert!(storage.contains(&full) && storage.contains(&thumb));

    let mod_cookie = moderator_cookie(&app, &email, "photo-reject-mod@example.com").await;
    let (_, queue) = get_c(&app, "/moderation/photos", Some(&mod_cookie)).await;
    let mcs = extract_csrf(&queue);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/photos/parking/{id}/reject"),
        &[("csrf", &mcs), ("reason", "unclear image")],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "moderator rejects: {s}");

    let (state, reason): (String, Option<String>) = sqlx::query_as(
        "SELECT moderation_state, rejection_reason FROM parking_photo WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(state, "REJECTED");
    assert_eq!(reason.as_deref(), Some("unclear image"));

    // The stored derivatives were deleted from the object store. (This used to
    // check for the absence of two paths under a local `media/` directory —
    // which stopped existing when media moved to S3, so the assertion passed
    // vacuously. It now asks the store the row's own keys.)
    assert!(!storage.contains(&full), "full derivative must be deleted");
    assert!(!storage.contains(&thumb), "thumbnail must be deleted");
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-reject'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("photo-reject-up@example.com").await;
    cleanup_user_contributions("photo-reject-mod@example.com").await;
}

#[db_test]
async fn moderation_queue_hides_uploader_identity(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "photo-ident", "Photo Identity").await;
    const EMAIL: &str = "photo-ident-up@example.com";
    let uploader = verified_cookie(&app, &email, EMAIL).await;
    let (_, page) = get_c(&app, &format!("/parking/{loc}"), Some(&uploader)).await;
    let csrf = extract_csrf(&page);
    let (_, body) = multipart_upload(&tiny_jpeg(), Some("ident photo"), "----bikesnestphoto");
    post_multipart_hx(
        &app,
        &format!("/parking/{loc}/photo"),
        body,
        &uploader,
        Some(&csrf),
    )
    .await;

    let mod_cookie = moderator_cookie(&app, &email, "photo-ident-mod@example.com").await;
    let (s, queue) = get_c(&app, "/moderation/photos", Some(&mod_cookie)).await;
    assert_eq!(s, StatusCode::OK);
    // "Contributor #id" is shown; the uploader's email / OAuth subject is not.
    assert!(
        queue.contains("Contributor"),
        "queue anonymizes the uploader"
    );
    assert!(!queue.contains(EMAIL), "uploader email never rendered");
    assert!(!queue.contains("sub-oauth"), "OAuth subject never rendered");
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-ident'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(EMAIL).await;
    cleanup_user_contributions("photo-ident-mod@example.com").await;
}

#[db_test]
async fn served_derivative_has_no_exif(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email, storage) = auth_app_with_storage().await;
    let loc = fixture_location(tx, "photo-exif", "Photo Exif").await;
    let uploader = verified_cookie(&app, &email, "photo-exif-up@example.com").await;
    let (_, page) = get_c(&app, &format!("/parking/{loc}"), Some(&uploader)).await;
    let csrf = extract_csrf(&page);
    let (_, body) = multipart_upload(&tiny_jpeg(), None, "----bikesnestphoto");
    post_multipart_hx(
        &app,
        &format!("/parking/{loc}/photo"),
        body,
        &uploader,
        Some(&csrf),
    )
    .await;
    let (id,): (i64,) = sqlx::query_as("SELECT id FROM parking_photo WHERE location_id = $1")
        .bind(loc)
        .fetch_one(&pool().await)
        .await
        .unwrap();

    // Approve so it is served.
    let mod_cookie = moderator_cookie(&app, &email, "photo-exif-mod@example.com").await;
    let (_, queue) = get_c(&app, "/moderation/photos", Some(&mod_cookie)).await;
    let mcs = extract_csrf(&queue);
    post_form(
        &app,
        &format!("/moderation/photos/parking/{id}/approve"),
        &[("csrf", &mcs)],
        Some(&mod_cookie),
    )
    .await;

    // Read the presigned derivative's object key straight out of the gallery
    // `<img src>` and fetch its bytes from the storage double directly — real
    // presigned URLs point straight at the bucket (no app media proxy), so
    // there is no route in the app itself to fetch them through.
    let (_, gallery) = get_c(&app, &format!("/parking/{loc}"), None).await;
    let anchor = format!(r#"src="{MEDIA_ORIGIN}/uploads/"#);
    let start = gallery.find(&anchor).expect("media thumb URL present");
    let after = &gallery[start + anchor.len()..];
    let url_tail = &after[..after.find('"').expect("closing quote")];
    // Askama escapes `&` in the src attribute; the key itself precedes the
    // (escaped) `?exp=...&sig=...` query string, so a plain split suffices.
    let key = format!("uploads/{url_tail}")
        .split('?')
        .next()
        .expect("key before query string")
        .to_string();
    let bytes = storage.get_bytes(&key).expect("derivative bytes stored");
    assert!(
        !has_exif_marker(&bytes),
        "served derivative has no EXIF/APP1"
    );
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-exif'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("photo-exif-up@example.com").await;
    cleanup_user_contributions("photo-exif-mod@example.com").await;
}

#[db_test]
async fn photo_upload_is_rate_limited(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "photo-rl", "Photo Rate Limit").await;
    let cookie = verified_cookie(&app, &email, "photo-rl-up@example.com").await;
    let (_, page) = get_c(&app, &format!("/parking/{loc}"), Some(&cookie)).await;
    let csrf = extract_csrf(&page);
    let jpeg = tiny_jpeg();
    for _ in 0..10 {
        let (_, body) = multipart_upload(&jpeg, None, "----bikesnestphoto");
        let (s, _) = post_multipart_hx(
            &app,
            &format!("/parking/{loc}/photo"),
            body,
            &cookie,
            Some(&csrf),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "day-1 upload allowed: {s}");
    }
    let (_, body) = multipart_upload(&jpeg, None, "----bikesnestphoto");
    let (s, _) = post_multipart_hx(
        &app,
        &format!("/parking/{loc}/photo"),
        body,
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::TOO_MANY_REQUESTS,
        "11th same-day upload rate-limited"
    );
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-rl'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("photo-rl-up@example.com").await;
}

async fn admin_cookie(
    app: &axum::Router,
    email: &bikesnest_infrastructure::FakeEmailProvider,
    addr: &str,
) -> String {
    cleanup_user_contributions(addr).await;
    post_form(
        app,
        "/register",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    let token = email
        .token_for("/verify-email")
        .expect("admin verification email");
    get_c(app, &format!("/verify-email?token={token}"), None).await;
    let (uid,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(addr)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    // See the note in `hold_admin_set_lock_for_process`: the ADMIN set is
    // shared with the tests that assert on "never zero admins".
    bikesnest_test_support::hold_admin_set_lock_for_process(&pool().await).await;
    sqlx::query(
        "INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'ADMIN', NULL) ON CONFLICT DO NOTHING",
    )
    .bind(uid).execute(&pool().await).await.unwrap();
    let (_, _, cookie) = post_form(
        app,
        "/login",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    cookie.unwrap().split(';').next().unwrap().to_string()
}

#[db_test]
async fn admin_can_access_moderation_queue(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let admin = admin_cookie(&app, &email, "photo-admin@example.com").await;
    let (s, body) = get_c(&app, "/moderation/photos", Some(&admin)).await;
    assert_eq!(
        s,
        StatusCode::OK,
        "admin (without Moderator role) can open the queue"
    );
    assert!(body.contains("Photo moderation"), "queue page renders");
    let _ = tx;
    cleanup_user_contributions("photo-admin@example.com").await;
}

/// WP11: the dashboard renders four numeric count tiles wired to
/// `queue_counts()`. This only checks the page actually renders four real,
/// non-negative numbers (not a placeholder, and not zero from a broken
/// query) — asserting *exact* values here would be racy, since
/// `queue_counts()` reads global tables the whole suite shares (a sibling
/// test's own fixture churn can move them mid-test). The exact-delta
/// assertion lives in the infrastructure `#[db_test]`
/// `queue_counts_on_reflects_an_exact_delta_race_free`, which takes both
/// reads on one isolated connection instead.
#[db_test]
async fn moderation_dashboard_renders_four_numeric_tiles(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let moderator = moderator_cookie(&app, &email, "dash-tiles-mod@example.com").await;

    let (status, body) = get_c(&app, "/moderation", Some(&moderator)).await;
    assert_eq!(status, StatusCode::OK);

    // Each of the four stat tiles in moderation_dashboard.html shares this
    // exact class list; parse the digits out of each one.
    let anchor = r#"class="mt-2 font-display text-3xl font-bold text-fg">"#;
    let mut tiles = Vec::new();
    let mut rest = body.as_str();
    while let Some(start) = rest.find(anchor) {
        let after = &rest[start + anchor.len()..];
        let end = after.find('<').expect("tile value closed by a tag");
        let digits = &after[..end];
        let n: i64 = digits
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("dashboard tile is not a plain integer: {digits:?}"));
        tiles.push(n);
        rest = &after[end..];
    }
    assert_eq!(tiles.len(), 4, "dashboard renders exactly four count tiles");
    assert!(
        tiles.iter().all(|&n| n >= 0),
        "every count tile is non-negative: {tiles:?}"
    );

    let _ = tx;
    cleanup_user_contributions("dash-tiles-mod@example.com").await;
}

/// WP11: a full page (== the limit) renders the "load more" keyset-pagination
/// control; a fixture of `limit + 1` rows guarantees the first page is full.
#[db_test]
async fn moderation_reports_queue_shows_load_more_when_full(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    let moderator = moderator_cookie(&app, &email, "reports-more-mod@example.com").await;
    const REPORTER: &str = "reports-more-reporter@example.com";
    let _ = verified_cookie(&app, &email, REPORTER).await;
    let (uid,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(REPORTER)
        .fetch_one(&pool().await)
        .await
        .unwrap();

    // DEFAULT_PAGE_LIMIT is 50; 51 distinct-target OPEN reports (the dedupe
    // index only constrains repeats of the same target) guarantee a full
    // first page.
    for target_id in 9_000_000i64..9_000_051 {
        sqlx::query(
            "INSERT INTO report (reporter_id, target_type, target_id, reason, state) \
             VALUES ($1, 'parking', $2, 'other', 'OPEN')",
        )
        .bind(uid)
        .bind(target_id)
        .execute(&pool().await)
        .await
        .unwrap();
    }

    let (status, body) = get_c(&app, "/moderation/reports?state=OPEN", Some(&moderator)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Load more"),
        "a full page must render the load-more control"
    );
    assert!(
        body.contains("after_id="),
        "the load-more link carries a keyset cursor"
    );

    let _ = tx;
    sqlx::query("DELETE FROM report WHERE reporter_id = $1")
        .bind(uid)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("reports-more-mod@example.com").await;
    cleanup_user_contributions(REPORTER).await;
}

#[db_test]
async fn photo_upload_alt_too_long_is_bad_request(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "photo-alt-long", "Photo Alt Long").await;
    let cookie = verified_cookie(&app, &email, "photo-alt-long-up@example.com").await;
    let (_, page) = get_c(&app, &format!("/parking/{loc}"), Some(&cookie)).await;
    let csrf = extract_csrf(&page);
    let long_alt = "a".repeat(501);
    let (_, body) = multipart_upload(&tiny_jpeg(), Some(&long_alt), "----bikesnestphoto");
    let (s, _) = post_multipart_hx(
        &app,
        &format!("/parking/{loc}/photo"),
        body,
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "over-long caption rejected as 400"
    );
    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'photo-alt-long'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("photo-alt-long-up@example.com").await;
}

// ---------------------------------------------------------------------------
// M5 moderation & reporting — end-to-end (report → claim → resolve → hide;
// invalidate/restore parking; suspend/restore; audit viewer gating; the
// self-resolve guard; D3 multipart review-photo attach).
// ---------------------------------------------------------------------------

async fn last_report_id(reporter_email: &str, target_type: &str, target_id: i64) -> i64 {
    let (id,): (i64,) = sqlx::query_as(
        "SELECT id FROM report WHERE reporter_id = (SELECT id FROM users WHERE email = $1) \
         AND target_type = $2 AND target_id = $3 ORDER BY id DESC LIMIT 1",
    )
    .bind(reporter_email)
    .bind(target_type)
    .bind(target_id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    id
}

#[db_test]
async fn report_review_flow_claim_resolve_hides_and_audits(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    const UPLOADER: &str = "m5-up@example.com";
    const REPORTER: &str = "m5-reporter@example.com";
    const MOD: &str = "m5-mod@example.com";
    let loc = fixture_location(tx, "m5-report-loc", "M5 Report Loc").await;

    // Uploader (verified) writes a review (D3 multipart).
    let uploader = verified_cookie(&app, &email, UPLOADER).await;
    let (_, rev_form) = get_c(&app, &format!("/parking/{loc}/review"), Some(&uploader)).await;
    let rcsrf = extract_csrf(&rev_form);
    let rbody = multipart_review("5", "Great secured rack", "----bikesnestphoto");
    let (s, _) = post_multipart(
        &app,
        &format!("/parking/{loc}/review"),
        rbody,
        &uploader,
        Some(&rcsrf),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "review created: {s}");
    let (review_id,): (i64,) =
        sqlx::query_as("SELECT id FROM review WHERE location_id = $1 ORDER BY id DESC LIMIT 1")
            .bind(loc)
            .fetch_one(&pool().await)
            .await
            .unwrap();

    // Reporter (authenticated, brand-new — not verified) reports the review.
    let reporter = unverified_cookie(&app, REPORTER).await;
    let (_, rep_page) = get_c(&app, &format!("/parking/{loc}"), Some(&reporter)).await;
    let csrf = extract_csrf(&rep_page);
    let (s, _, _) = post_form_hx(
        &app,
        "/reports",
        &[
            ("csrf", &csrf),
            ("target_type", "review"),
            ("target_id", &review_id.to_string()),
            ("reason", "spam"),
            ("description", "Looks fake"),
        ],
        Some(&reporter),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "report submitted: {s}");
    let report_id = last_report_id(REPORTER, "review", review_id).await;

    let (state,): (String,) = sqlx::query_as("SELECT state FROM report WHERE id = $1")
        .bind(report_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(state, "OPEN");
    let (created_audits,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_events WHERE action = 'report.created' AND target_id = $1",
    )
    .bind(report_id.to_string())
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(created_audits, 1);

    // Moderator claims then resolves (hides the review).
    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let (_, mod_page) = get_c(&app, "/moderation/reports?state=OPEN", Some(&mod_cookie)).await;
    let mcsrf = extract_csrf(&mod_page);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/reports/{report_id}/claim"),
        &[("csrf", &mcsrf)],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "claim: {s}");
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/reports/{report_id}/resolve"),
        &[("csrf", &mcsrf), ("note", "spam review")],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "resolve: {s}");

    let (state,): (String,) = sqlx::query_as("SELECT state FROM report WHERE id = $1")
        .bind(report_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(state, "RESOLVED");
    let (rstate,): (String,) = sqlx::query_as("SELECT moderation_state FROM review WHERE id = $1")
        .bind(review_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(rstate, "HIDDEN", "resolved report hides the review");

    // The hidden review disappears from public P3.
    let (_, pub_page) = get_c(&app, &format!("/parking/{loc}"), None).await;
    assert!(
        !pub_page.contains("Great secured rack"),
        "hidden review not rendered"
    );

    // Audit trail records who did what.
    let (claimed,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_events WHERE action = 'report.claimed' AND target_id = $1",
    )
    .bind(report_id.to_string())
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(claimed, 1);
    let (resolved,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_events WHERE action = 'report.resolved' AND target_id = $1",
    )
    .bind(report_id.to_string())
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(resolved, 1);

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'm5-report-loc'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(UPLOADER).await;
    cleanup_user_contributions(REPORTER).await;
    cleanup_user_contributions(MOD).await;
}

#[db_test]
async fn moderator_cannot_resolve_own_report(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const MOD: &str = "m5-selfmod@example.com";
    let loc = fixture_location(tx, "m5-self-loc", "M5 Self Loc").await;
    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let (_, mod_page) = get_c(&app, &format!("/parking/{loc}"), Some(&mod_cookie)).await;
    let csrf = extract_csrf(&mod_page);

    // The moderator submits a report, then tries to resolve it themselves.
    let (s, _, _) = post_form_hx(
        &app,
        "/reports",
        &[
            ("csrf", &csrf),
            ("target_type", "parking"),
            ("target_id", &loc.to_string()),
            ("reason", "spam"),
        ],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let report_id = last_report_id(MOD, "parking", loc).await;

    // Claim → allowed, but self-resolve → CONFLICT/self-resolve error.
    let (_, mod_page2) = get_c(&app, "/moderation/reports?state=OPEN", Some(&mod_cookie)).await;
    let mcsrf = extract_csrf(&mod_page2);
    post_form_hx(
        &app,
        &format!("/moderation/reports/{report_id}/claim"),
        &[("csrf", &mcsrf)],
        Some(&mod_cookie),
    )
    .await;
    let (s, body, _) = post_form_hx(
        &app,
        &format!("/moderation/reports/{report_id}/resolve"),
        &[("csrf", &mcsrf), ("note", "x")],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "self-resolve rejected: {s}");
    assert!(
        body.contains("cannot resolve"),
        "friendly self-resolve message: {body}"
    );

    let (state,): (String,) = sqlx::query_as("SELECT state FROM report WHERE id = $1")
        .bind(report_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(
        state, "UNDER_REVIEW",
        "report stays under review after rejected self-resolve"
    );

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'm5-self-loc'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(MOD).await;
}

#[db_test]
async fn invalidate_parking_public_404_moderator_banner_and_restore(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    const MOD: &str = "m5-inv-mod@example.com";
    let loc = fixture_location(tx, "m5-inv-loc", "M5 Invalidate Loc").await;

    // Public sees the active listing.
    let (s, _) = get_c(&app, &format!("/parking/{loc}"), None).await;
    assert_eq!(s, StatusCode::OK);

    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let (_, mod_page) = get_c(&app, "/moderation", Some(&mod_cookie)).await;
    let mcsrf = extract_csrf(&mod_page);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/parking/{loc}/invalidate"),
        &[("csrf", &mcsrf)],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "invalidate: {s}");

    // Public P3 now 404s; the moderator still sees it with a banner.
    let (s, _) = get_c(&app, &format!("/parking/{loc}"), None).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "public cannot see invalidated parking"
    );
    let (s, body) = get_c(&app, &format!("/parking/{loc}"), Some(&mod_cookie)).await;
    assert_eq!(s, StatusCode::OK, "moderator still sees the page");
    assert!(
        body.contains("under moderation"),
        "moderator banner rendered"
    );

    // Also absent from search (search filters ACTIVE).
    let (_, search) = get_c(&app, "/search?lat=-25.4284&lon=-49.2733&radius=2000", None).await;
    assert!(!search.contains("M5 Invalidate Loc"));

    // Restore brings it back (grab a fresh CSRF from the dashboard).
    let (_, mod_page2) = get_c(&app, "/moderation", Some(&mod_cookie)).await;
    let mcsrf2 = extract_csrf(&mod_page2);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/parking/{loc}/restore"),
        &[("csrf", &mcsrf2)],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "restore: {s}");
    let (s, _) = get_c(&app, &format!("/parking/{loc}"), None).await;
    assert_eq!(s, StatusCode::OK, "public sees restored listing");

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'm5-inv-loc'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(MOD).await;
}

#[db_test]
async fn admin_suspend_revokes_sessions_blocks_and_restore(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    const USER: &str = "m5-suspend@example.com";
    const ADMIN: &str = "m5-admin@example.com";
    // A verified user with a live session.
    let user_cookie = verified_cookie(&app, &email, USER).await;
    let admin_cookie = admin_cookie(&app, &email, ADMIN).await;
    let (_, admin_page) = get_c(&app, "/admin/users", Some(&admin_cookie)).await;
    let acsrf = extract_csrf(&admin_page);
    let (uid,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(USER)
        .fetch_one(&pool().await)
        .await
        .unwrap();

    // Suspend: session revoked + state SUSPENDED + audited.
    let (s, _, _) = post_form(
        &app,
        &format!("/admin/users/{uid}/suspend"),
        &[("csrf", &acsrf)],
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "suspend redirects: {s}");
    let (state,): (String,) = sqlx::query_as("SELECT account_state FROM users WHERE id = $1")
        .bind(uid)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(state, "SUSPENDED");
    let (revoked,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM sessions WHERE user_id = $1 AND revoked_at IS NOT NULL",
    )
    .bind(uid)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert!(revoked >= 1, "suspension revokes sessions");
    let (audit,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_events WHERE action = 'user.suspended' AND target_id = $1",
    )
    .bind(uid.to_string())
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(audit, 1);

    // The suspended user's existing session is now dead → redirected to login.
    let (s, _) = get_c(&app, "/account", Some(&user_cookie)).await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
        "mid-session gate: {s}"
    );

    // Login is blocked with the generic message.
    let (_, body, _) = post_form(
        &app,
        "/login",
        &[("email", USER), ("password", "password123")],
        None,
    )
    .await;
    assert!(
        body.contains("Email or password is incorrect"),
        "suspended login blocked: {body}"
    );

    // Restore → ACTIVE + login works.
    let (_, admin_page2) = get_c(&app, "/admin/users", Some(&admin_cookie)).await;
    let acsrf2 = extract_csrf(&admin_page2);
    let (s, _, _) = post_form(
        &app,
        &format!("/admin/users/{uid}/restore"),
        &[("csrf", &acsrf2)],
        Some(&admin_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "restore redirects: {s}");
    let (_, _, new_cookie) = post_form(
        &app,
        "/login",
        &[("email", USER), ("password", "password123")],
        None,
    )
    .await;
    assert!(
        new_cookie.as_deref().unwrap_or("").contains("session_id="),
        "restored user can log in"
    );

    let _ = tx;
    cleanup_user_contributions(USER).await;
    cleanup_user_contributions(ADMIN).await;
}

#[db_test]
async fn moderation_and_audit_routes_are_gated(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    // Anonymous → redirected to login.
    for uri in [
        "/moderation",
        "/moderation/reports",
        "/moderation/proposals",
    ] {
        let (s, _) = get_c(&app, uri, None).await;
        assert!(
            matches!(s, StatusCode::SEE_OTHER | StatusCode::FOUND),
            "{uri} anonymous: {s}"
        );
    }
    // Non-moderator verified user → 403 on every moderation route.
    let cookie = verified_cookie(&app, &email, "m5-nonmod@example.com").await;
    let (s, _) = get_c(&app, "/moderation", Some(&cookie)).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "non-moderator cannot open dashboard"
    );
    let (s, _) = get_c(&app, "/admin/audit", Some(&cookie)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "audit viewer is admin-only");
    // A moderator (not admin) cannot open the audit viewer.
    let mod_cookie = moderator_cookie(&app, &email, "m5-mod-gate@example.com").await;
    let (s, _) = get_c(&app, "/admin/audit", Some(&mod_cookie)).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "moderator (not admin) cannot open audit viewer"
    );
    let _ = tx;
    cleanup_user_contributions("m5-nonmod@example.com").await;
    cleanup_user_contributions("m5-mod-gate@example.com").await;
}

#[db_test]
async fn d3_review_photos_held_pending_until_approved(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const AUTHOR: &str = "m5-review-photo@example.com";
    let loc = fixture_location(tx, "m5-rp-loc", "M5 Review Photo Loc").await;
    let cookie = verified_cookie(&app, &email, AUTHOR).await;
    let (_, form) = get_c(&app, &format!("/parking/{loc}/review"), Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    // D3 multipart review with an attached photo (rating + body + photo in one body).
    let jpeg = tiny_jpeg();
    let mut body = Vec::new();
    body.extend_from_slice(
        b"------bikesnestphoto\r\nContent-Disposition: form-data; name=\"rating\"\r\n\r\n4\r\n",
    );
    body.extend_from_slice(b"------bikesnestphoto\r\nContent-Disposition: form-data; name=\"body\"\r\n\r\nNice locker\r\n");
    body.extend_from_slice(b"------bikesnestphoto\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"r.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n");
    body.extend_from_slice(&jpeg);
    body.extend_from_slice(b"\r\n------bikesnestphoto--\r\n");
    let (s, _) = post_multipart(
        &app,
        &format!("/parking/{loc}/review"),
        body,
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "review with photo: {s}");

    let (review_id,): (i64,) =
        sqlx::query_as("SELECT id FROM review WHERE location_id = $1 ORDER BY id DESC LIMIT 1")
            .bind(loc)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    let (rstate,): (String,) = sqlx::query_as("SELECT moderation_state FROM review WHERE id = $1")
        .bind(review_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(rstate, "ACTIVE", "review text publishes immediately");

    // The review photo is held PENDING_REVIEW; only approved render.
    let (pstate,): (String,) =
        sqlx::query_as("SELECT moderation_state FROM review_photo WHERE review_id = $1")
            .bind(review_id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(pstate, "PENDING_REVIEW", "review photo held pending");

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'm5-rp-loc'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(AUTHOR).await;
}

#[db_test]
async fn approved_review_photo_renders_on_p3(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const AUTHOR: &str = "m5-rp-render@example.com";
    const MOD: &str = "m5-rp-mod@example.com";
    let loc = fixture_location(tx, "m5-rp-render-loc", "M5 Review Render Loc").await;
    let cookie = verified_cookie(&app, &email, AUTHOR).await;
    let (_, form) = get_c(&app, &format!("/parking/{loc}/review"), Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    // D3 multipart review with an attached (pending) photo.
    let jpeg = tiny_jpeg();
    let mut body = Vec::new();
    body.extend_from_slice(
        b"------bikesnestphoto\r\nContent-Disposition: form-data; name=\"rating\"\r\n\r\n5\r\n",
    );
    body.extend_from_slice(b"------bikesnestphoto\r\nContent-Disposition: form-data; name=\"body\"\r\n\r\nGreat locker\r\n");
    body.extend_from_slice(b"------bikesnestphoto\r\nContent-Disposition: form-data; name=\"photo\"; filename=\"r.jpg\"\r\nContent-Type: image/jpeg\r\n\r\n");
    body.extend_from_slice(&jpeg);
    body.extend_from_slice(b"\r\n------bikesnestphoto--\r\n");
    let (s, _) = post_multipart(
        &app,
        &format!("/parking/{loc}/review"),
        body,
        &cookie,
        Some(&csrf),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER);

    let (review_id,): (i64,) =
        sqlx::query_as("SELECT id FROM review WHERE location_id = $1 ORDER BY id DESC LIMIT 1")
            .bind(loc)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    let (rp_id,): (i64,) = sqlx::query_as("SELECT id FROM review_photo WHERE review_id = $1")
        .bind(review_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();

    // Pending → not yet rendered on the public P3.
    let (_, pub_page) = get_c(&app, &format!("/parking/{loc}"), None).await;
    assert!(
        !pub_page.contains(&format!("{MEDIA_ORIGIN}/uploads/")),
        "pending review photo not rendered"
    );

    // Moderator approves it from the unified queue (kind=review).
    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let (_, mod_page) = get_c(&app, "/moderation/photos", Some(&mod_cookie)).await;
    let mcsrf = extract_csrf(&mod_page);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/photos/review/{rp_id}/approve"),
        &[("csrf", &mcsrf)],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Now the approved review photo renders on P3.
    let (_, pub_page) = get_c(&app, &format!("/parking/{loc}"), None).await;
    assert!(
        pub_page.contains(&format!("{MEDIA_ORIGIN}/uploads/")),
        "approved review photo renders on P3"
    );

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'm5-rp-render-loc'")
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(AUTHOR).await;
    cleanup_user_contributions(MOD).await;
}

// ---------------------------------------------------------------------------
// Template hygiene (pure filesystem scan, no DB)
// ---------------------------------------------------------------------------

/// `--color-error` was renamed to `--color-danger` in input.css; any
/// `text-error`/`bg-error`/`border-error`/`hover:border-error` utility left in
/// a template renders as dead CSS (no matching Tailwind class exists).
#[test]
fn no_error_colour_classes_remain_in_templates() {
    let templates_dir =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates"));
    let pattern = regex::Regex::new(r"\b(text|bg|border|hover:border)-error\b").unwrap();
    let mut offenders = Vec::new();
    let mut stack = vec![templates_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(contents) = std::fs::read_to_string(&path) {
                for m in pattern.find_iter(&contents) {
                    offenders.push(format!("{}: {}", path.display(), m.as_str()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "found dead `-error` colour classes:\n{}",
        offenders.join("\n")
    );
}

/// WP12: the header used to decide "signed in" from `layout.csrf != ""`, which
/// also lit up for the anonymous auth pages (login/register/reset/verify) that
/// mint a double-submit CSRF token without a session. It must branch on the
/// real session flag instead.
#[test]
fn base_layout_does_not_branch_on_csrf_presence() {
    let path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../templates/layouts/base.html"
    ));
    let contents = std::fs::read_to_string(path).expect("read base.html");
    assert!(
        !contents.contains("layout.csrf != \"\""),
        "base.html must not branch the header on csrf presence"
    );
    assert!(
        contents.contains("layout.is_authenticated"),
        "base.html header should branch on layout.is_authenticated"
    );
}

// ---------------------------------------------------------------------------
// Configuration hygiene (pure filesystem scan, no DB)
// ---------------------------------------------------------------------------

/// The web layer must never read the process environment: everything it needs
/// is parsed once into `Config` at startup and reaches handlers through
/// `AppState`. A stray `std::env::var` reintroduces per-request configuration
/// (and a second, divergent config path) — including `main.rs`, whose only job
/// is `dotenv` + `Config::from_env`.
#[test]
fn web_crate_never_reads_the_process_environment() {
    let src = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut offenders = Vec::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let contents = std::fs::read_to_string(&path).expect("read source file");
            for (n, line) in contents.lines().enumerate() {
                if line.contains("std::env::var") || line.contains("env::var(") {
                    offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "crates/web/src must read configuration from `Config`, not the environment:\n{}",
        offenders.join("\n")
    );
}

/// Walks every `.rs` file under `crates/web/src`, returning (path, contents).
fn web_sources() -> Vec<(std::path::PathBuf, String)> {
    let src = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let mut out = Vec::new();
    let mut stack = vec![src.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let contents = std::fs::read_to_string(&path).expect("read source file");
                out.push((path, contents));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Providers are wired in one place. A handler must reach the database only
/// through an application port held in `AppState`, so nothing under
/// `src/routes/` may name a repository, a pool or a concrete adapter — the
/// only infrastructure types it may mention are the parsed configuration
/// values it renders (`MapConfig`, the featured origin, …).
///
/// `wiring.rs` is where the `Sqlx…` constructors belong (`state.rs` names one
/// probe type, in the signature of the readiness use case it holds).
#[test]
fn route_handlers_never_reach_for_infrastructure() {
    const ALLOWED_INFRA_TYPES: &[&str] = &[
        "Config",
        "MapConfig",
        "SecurityConfig",
        "FEATURED_ORIGIN",
        "GeocodeLimits",
    ];
    const FORBIDDEN: &[&str] = &["Sqlx", "sqlx::", "S3ObjectStorage", "Db::", "db.pool()"];

    let infra = regex::Regex::new(r"bikesnest_infrastructure::\{?([A-Za-z_0-9]+)").unwrap();
    let mut offenders = Vec::new();
    for (path, contents) in web_sources() {
        if !path.components().any(|c| c.as_os_str() == "routes") {
            continue;
        }
        for (n, line) in contents.lines().enumerate() {
            for needle in FORBIDDEN {
                if line.contains(needle) {
                    offenders.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
                }
            }
            for caps in infra.captures_iter(line) {
                let name = caps.get(1).unwrap().as_str();
                if !ALLOWED_INFRA_TYPES.contains(&name) {
                    offenders.push(format!(
                        "{}:{}: bikesnest_infrastructure::{name}",
                        path.display(),
                        n + 1
                    ));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "handlers must go through an application port; wire providers in `wiring.rs`:\n{}",
        offenders.join("\n")
    );
}

/// The router used to be one 5k-line module. Nothing in the web crate should
/// grow back into that: a slice that outgrows this limit wants splitting.
/// `view.rs` (the view-model builders) is the one file still over it.
#[test]
fn no_web_source_file_is_longer_than_1200_lines() {
    const LIMIT: usize = 1200;
    const EXEMPT: &[&str] = &["view.rs"];

    let mut offenders = Vec::new();
    for (path, contents) in web_sources() {
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let lines = contents.lines().count();
        if lines > LIMIT && !EXEMPT.contains(&name.as_str()) {
            offenders.push(format!("{}: {lines} lines", path.display()));
        }
    }
    assert!(
        offenders.is_empty(),
        "these files are over {LIMIT} lines; split them by slice:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------------
// WP7: `X-Forwarded-For` is not a rate-limit identity unless a proxy is trusted
// ---------------------------------------------------------------------------

/// POST a form with an explicit `X-Forwarded-For`, carrying the anonymous
/// double-submit CSRF pair `post_form` would normally add.
async fn post_form_xff(
    app: &axum::Router,
    uri: &str,
    fields: &[(&str, &str)],
    xff: &str,
) -> (StatusCode, String) {
    let mut all: Vec<(String, String)> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut cookie = None;
    if let Some(src) = anon_source_for(uri)
        && let Some((line, token)) = anon_csrf(app, src).await
    {
        cookie = Some(line);
        all.push(("csrf".to_string(), token));
    }
    let body = all
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let mut b = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("Accept-Language", "en")
        .header("x-forwarded-for", xff);
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).to_string())
}

/// `test_config()` leaves `TRUSTED_PROXY_HOPS` at 0, so `X-Forwarded-For` is
/// ignored entirely and a caller cannot mint a fresh rate-limit bucket per
/// request by changing the header. Registration is limited to 3 per hour per
/// address; the emails below are invalid on purpose, so the limiter is the only
/// thing being exercised (no accounts are created) — the limit check runs
/// before the address is parsed.
#[db_test]
async fn a_spoofed_forwarded_for_cannot_dodge_the_rate_limit(_tx: &mut TestTx) {
    let (app, _email) = auth_app().await;
    const RATE_LIMITED: &str = "Too many attempts. Try again later.";
    const INVALID: &str = "That email is not valid.";

    for i in 0..3 {
        let (status, body) = post_form_xff(
            &app,
            "/register",
            &[("email", "not-an-email"), ("password", "password123")],
            &format!("203.0.113.{i}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "attempt {i} renders the form again");
        assert!(
            body.contains(INVALID) && !body.contains(RATE_LIMITED),
            "attempt {i} must be rejected on the address, not the limiter"
        );
    }

    // A fourth attempt from yet another forged address lands in the same bucket.
    let (_, body) = post_form_xff(
        &app,
        "/register",
        &[("email", "not-an-email"), ("password", "password123")],
        "198.51.100.77",
    )
    .await;
    assert!(
        body.contains(RATE_LIMITED),
        "a new X-Forwarded-For must not reset the per-address limit"
    );
}

/// The mirror of the test above: with `TRUSTED_PROXY_HOPS=1` the header *is*
/// the identity (a real proxy appends it), so four requests forwarded from four
/// different addresses are four buckets and none is limited. Together the two
/// tests show the extractor actually reads the configured hop count rather than
/// returning one constant.
#[db_test]
async fn a_trusted_proxys_forwarded_for_does_key_the_bucket(_tx: &mut TestTx) {
    let db = Db::from_pool(pool().await);
    let config = bikesnest_infrastructure::Config {
        trusted_proxy_hops: 1,
        ..test_config()
    };
    let app = app_router_with(
        std::sync::Arc::new(config),
        db,
        RouterDeps {
            email: std::sync::Arc::new(FakeEmailProvider::with_root(None)),
            oauth: None,
            hasher: TestPasswordHasher,
            rate_limiter: Box::new(bikesnest_infrastructure::InMemoryRateLimiter::new()),
            storage: std::sync::Arc::new(bikesnest_test_support::TestObjectStorage::new()),
        },
    );

    for i in 0..4 {
        let (_, body) = post_form_xff(
            &app,
            "/register",
            &[("email", "not-an-email"), ("password", "password123")],
            &format!("203.0.113.{i}"),
        )
        .await;
        assert!(
            !body.contains("Too many attempts. Try again later."),
            "attempt {i} came from a distinct forwarded address, so it has its own bucket"
        );
    }
}

// ---------------------------------------------------------------------------
// WP8: moderation state is enforced on the write path, not only on reads.
// ---------------------------------------------------------------------------

/// Every contribution route refuses a location moderation has taken down, and
/// leaves the row exactly as it found it. Favorites are the deliberate
/// exception: a private bookmark is not a contribution.
#[db_test]
async fn contribution_routes_refuse_a_location_that_is_not_active(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp8-not-active@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "WP8 Taken Down", &[]).await;

    // A verification while the spot is still ACTIVE, so `last_verified_at` has
    // a value a later `still_exists` could reset.
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/parking/{id}/verify"),
        &[
            ("csrf", &csrf),
            ("kind", "existence"),
            ("result", "still_exists"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "baseline verification accepted");

    const NOT_ACTIVE: &str = "no longer accepting contributions";
    for state in ["REMOVED", "INVALID"] {
        sqlx::query("UPDATE parking_location SET moderation_state = $2 WHERE id = $1")
            .bind(id)
            .bind(state)
            .execute(&pool().await)
            .await
            .unwrap();
        let before = location_write_state(id).await;

        // The edit form is gone, exactly as the public details page is.
        let (s, _) = get_c(&app, &format!("/parking/{id}/edit"), Some(&cookie)).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{state}: GET edit page");

        let (s, _, _) = post_form(
            &app,
            &format!("/parking/{id}/edit"),
            &[
                ("csrf", &csrf),
                ("version", "1"),
                ("name", "Renamed"),
                ("address", "Rua Y, 2"),
                ("parking_type", "rack"),
                ("cost_kind", "unknown"),
            ],
            Some(&cookie),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{state}: POST edit");

        let (s, _, _) = post_form(
            &app,
            &format!("/parking/{id}/proposal"),
            &[
                ("csrf", &csrf),
                ("kind", "change_existence"),
                ("existence", "no_longer_exists"),
                ("reason", "gone"),
            ],
            Some(&cookie),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{state}: POST proposal");

        let (s, body) = post_multipart(
            &app,
            &format!("/parking/{id}/review"),
            multipart_review("5", "still great", "----bikesnestphoto"),
            &cookie,
            Some(&csrf),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{state}: POST review");
        assert!(body.contains(NOT_ACTIVE), "{state}: review says why");

        let (s, body, _) = post_form(
            &app,
            &format!("/parking/{id}/verify"),
            &[
                ("csrf", &csrf),
                ("kind", "existence"),
                ("result", "still_exists"),
            ],
            Some(&cookie),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{state}: POST verify");
        assert!(body.contains(NOT_ACTIVE), "{state}: verify says why");

        let (s, _, _) = post_form(
            &app,
            &format!("/parking/{id}/parked-here"),
            &[("csrf", &csrf)],
            Some(&cookie),
        )
        .await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{state}: POST parked-here");

        // Nothing moved: version, freshness, revisions, reviews, verifications.
        assert_eq!(
            location_write_state(id).await,
            before,
            "{state}: no write landed"
        );

        // A favorite is still a favorite — toggling it stays available.
        let (s, _, _) = post_form_hx(
            &app,
            &format!("/parking/{id}/favorite"),
            &[("csrf", &csrf)],
            Some(&cookie),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{state}: favorites keep working");
    }

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

/// (version, last_verified_at, revision count, review count, verification count)
/// — everything a refused contribution must leave untouched.
async fn location_write_state(
    id: i64,
) -> (i64, Option<chrono::DateTime<chrono::Utc>>, i64, i64, i64) {
    sqlx::query_as(
        r#"
        SELECT l.version,
               l.last_verified_at,
               (SELECT count(*) FROM parking_revision WHERE location_id = l.id),
               (SELECT count(*) FROM review           WHERE location_id = l.id),
               (SELECT count(*) FROM verification     WHERE location_id = l.id)
        FROM parking_location l WHERE l.id = $1
        "#,
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap()
}

/// A second identical report is not a new signal — it is the same complaint.
#[db_test]
async fn duplicate_report_is_refused_with_a_conflict(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp8-dupe-reporter@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "WP8 Report Target", &[]).await;

    let fields = [
        ("csrf", csrf.as_str()),
        ("target_type", "parking"),
        ("target_id", &id.to_string()),
        ("reason", "duplicate"),
        ("description", "already listed"),
    ];
    let (s, _, _) = post_form_hx(&app, "/reports", &fields, Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK, "first report accepted");

    let (s, body, _) = post_form_hx(&app, "/reports", &fields, Some(&cookie)).await;
    assert_eq!(s, StatusCode::CONFLICT, "second identical report refused");
    assert!(
        body.contains("You already reported this"),
        "duplicate message shown: {body}"
    );

    let (open,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM report WHERE target_type = 'parking' AND target_id = $1",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(open, 1, "only one report row exists");

    let _ = tx;
    sqlx::query("DELETE FROM report WHERE target_type = 'parking' AND target_id = $1")
        .bind(id)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(EMAIL).await;
}

// ---------------------------------------------------------------------------
// WP10: htmx response discipline
//
// htmx 4 sends `HX-Request: true` on every request it issues, including boosted
// navigations and back/forward history replays — both of which swap `<body>`.
// Only a request htmx will swap into a real target may be answered with a
// partial; everything else must get a whole document.
// ---------------------------------------------------------------------------

/// A request with an arbitrary header set; returns status, headers and body.
async fn request_h(
    app: &axum::Router,
    method: &str,
    uri: &str,
    cookie: Option<&str>,
    headers: &[(&str, &str)],
) -> (StatusCode, HeaderMap, String) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("Accept-Language", "en");
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    if let Some(c) = cookie {
        b = b.header("cookie", c);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let head = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, head, String::from_utf8_lossy(&body).to_string())
}

/// Whether a response body is a whole page rather than a fragment.
fn is_document(body: &str) -> bool {
    body.contains("<!DOCTYPE") && body.contains("<header")
}

fn location_of(headers: &HeaderMap) -> String {
    headers
        .get("location")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Every `Vary` value on the response, joined — the header may appear more than
/// once (each layer appends its own names).
fn vary_of(headers: &HeaderMap) -> String {
    headers
        .get_all("vary")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect::<Vec<_>>()
        .join(", ")
        .to_ascii_lowercase()
}

// --- /search: fragment only for a real fragment request --------------------

#[db_test]
async fn search_answers_a_fragment_request_with_the_results_list(_tx: &mut TestTx) {
    let app = test_app().await;
    let (s, head, body) = request_h(&app, "GET", "/search?q=x", None, HX_FRAGMENT).await;
    assert_eq!(s, StatusCode::OK);
    assert!(!is_document(&body), "fragment must not be a whole page");
    let vary = vary_of(&head);
    assert!(vary.contains("hx-request"), "vary: {vary}");
    assert!(vary.contains("hx-request-type"), "vary: {vary}");
    assert!(vary.contains("hx-boosted"), "vary: {vary}");
}

#[db_test]
async fn search_answers_a_history_restore_with_a_whole_document(_tx: &mut TestTx) {
    // `#restoreHistory` replays the page targeting `document.body`, so a
    // fragment here would *become* the document.
    let app = test_app().await;
    let (s, _, body) = request_h(
        &app,
        "GET",
        "/search?q=x",
        None,
        &[
            ("HX-Request", "true"),
            ("HX-History-Restore-Request", "true"),
        ],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("<header"),
        "history restore must get the page"
    );
}

#[db_test]
async fn search_answers_a_boosted_request_with_a_whole_document(_tx: &mut TestTx) {
    let app = test_app().await;
    let (s, _, body) = request_h(
        &app,
        "GET",
        "/search?q=x",
        None,
        &[
            ("HX-Request", "true"),
            ("HX-Boosted", "true"),
            ("HX-Request-Type", "full"),
        ],
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("<header"), "boosted nav must get the page");
}

#[db_test]
async fn search_without_htmx_headers_is_the_full_page(_tx: &mut TestTx) {
    let (s, body) = get("/search?q=x").await;
    assert_eq!(s, StatusCode::OK);
    assert!(is_document(&body));
}

// --- Vary on ordinary HTML pages -------------------------------------------

#[db_test]
async fn html_pages_vary_by_locale_and_session(_tx: &mut TestTx) {
    let headers = get_headers("/").await;
    let vary = vary_of(&headers);
    assert!(vary.contains("accept-language"), "vary: {vary}");
    assert!(vary.contains("cookie"), "vary: {vary}");
}

// --- P3 fragment endpoints: partial for htmx, 303 for everyone else --------

#[db_test]
async fn p3_fragment_endpoints_redirect_a_whole_document_request(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp10-p3@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "WP10 P3 Target", &[]).await;

    // favorite — the success response *is* the button.
    let (s, body, _) = post_form_hx(
        &app,
        &format!("/parking/{id}/favorite"),
        &[("csrf", &csrf)],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "htmx favorite");
    assert!(
        body.contains(r#"id="favorite-button""#),
        "the button: {body}"
    );
    assert!(!is_document(&body), "fragment, not a page");

    let (s, _, _) = post_form(
        &app,
        &format!("/parking/{id}/favorite"),
        &[("csrf", &csrf)],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS favorite redirects");

    // parked-here.
    let (s, body, _) = post_form_hx(
        &app,
        &format!("/parking/{id}/parked-here"),
        &[("csrf", &csrf)],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "htmx parked-here");
    assert!(!is_document(&body), "fragment, not a page: {body}");

    let (s, _, cookie_hdr) = post_form(
        &app,
        &format!("/parking/{id}/parked-here"),
        &[("csrf", &csrf)],
        Some(&cookie),
    )
    .await;
    let _ = cookie_hdr;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS parked-here redirects");

    // verify.
    let (s, _, _) = post_form(
        &app,
        &format!("/parking/{id}/verify"),
        &[
            ("csrf", &csrf),
            ("kind", "existence"),
            ("result", "still_exists"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS verify redirects");

    // report.
    let (s, _, _) = post_form(
        &app,
        "/reports",
        &[
            ("csrf", &csrf),
            ("target_type", "parking"),
            ("target_id", &id.to_string()),
            ("reason", "duplicate"),
            ("description", "no-JS report"),
            ("page", &format!("/parking/{id}")),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS report redirects");

    // photo upload (multipart; the token rides `?csrf=` as the form's action does).
    let (_, body) = post_multipart(
        &app,
        &format!("/parking/{id}/photo?csrf={csrf}"),
        multipart_upload(&tiny_jpeg(), None, "----bikesnestphoto").1,
        &cookie,
        None,
    )
    .await;
    let _ = body;

    let _ = tx;
    sqlx::query("DELETE FROM report WHERE target_type = 'parking' AND target_id = $1")
        .bind(id)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn p3_fragment_endpoints_send_a_no_js_caller_to_the_page(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp10-p3-loc@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "WP10 Redirect Target", &[]).await;

    // Every redirect target is the page that now shows the new state.
    for (uri, fields, want) in [
        (
            format!("/parking/{id}/verify"),
            vec![
                ("csrf", csrf.clone()),
                ("kind", "existence".to_string()),
                ("result", "still_exists".to_string()),
            ],
            format!("/parking/{id}?verified=1"),
        ),
        (
            format!("/parking/{id}/parked-here"),
            vec![("csrf", csrf.clone())],
            format!("/parking/{id}?parked=1"),
        ),
        (
            format!("/parking/{id}/favorite"),
            vec![("csrf", csrf.clone())],
            format!("/parking/{id}"),
        ),
    ] {
        let refs: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let app2 = app.clone();
        let (s, _, _) = post_form(&app2, &uri, &refs, Some(&cookie)).await;
        assert_eq!(s, StatusCode::SEE_OTHER, "{uri}");
        // The Location is asserted through a second call that reads headers.
        let (_, head, _) = {
            let body = refs
                .iter()
                .map(|(k, v)| format!("{}={}", k, urlencode(v)))
                .collect::<Vec<_>>()
                .join("&");
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(&uri)
                        .header("content-type", "application/x-www-form-urlencoded")
                        .header("Accept-Language", "en")
                        .header("cookie", &cookie)
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            let st = res.status();
            let h = res.headers().clone();
            (st, h, ())
        };
        assert_eq!(location_of(&head), want, "{uri} lands on the page");
    }

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

// --- Moderation fragment endpoints -----------------------------------------

#[db_test]
async fn moderation_fragment_endpoints_redirect_to_their_queue(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    const UPLOADER: &str = "wp10-uploader@example.com";
    const MOD: &str = "wp10-mod@example.com";
    let loc = fixture_location(tx, "wp10-mod-queue", "WP10 Moderation Queue").await;

    let uploader = verified_cookie(&app, &email, UPLOADER).await;
    let (_, page) = get_c(&app, &format!("/parking/{loc}"), Some(&uploader)).await;
    let ucsrf = extract_csrf(&page);
    let (s, _) = post_multipart_hx(
        &app,
        &format!("/parking/{loc}/photo"),
        multipart_upload(&tiny_jpeg(), None, "----bikesnestphoto").1,
        &uploader,
        Some(&ucsrf),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "upload queued");
    let (photo_id,): (i64,) = sqlx::query_as(
        "SELECT id FROM parking_photo WHERE location_id = $1 ORDER BY id DESC LIMIT 1",
    )
    .bind(loc)
    .fetch_one(&pool().await)
    .await
    .unwrap();

    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let (_, queue) = get_c(&app, "/moderation/photos", Some(&mod_cookie)).await;
    let mcsrf = extract_csrf(&queue);

    // htmx gets the toast fragment…
    let (s, body, _) = post_form_hx(
        &app,
        &format!("/moderation/photos/parking/{photo_id}/approve"),
        &[("csrf", &mcsrf)],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "htmx approve");
    assert!(!is_document(&body), "toast, not a page: {body}");

    // …a whole-document caller gets sent back to the queue with a notice.
    let (s, head, _) = post_form_raw(
        &app,
        &format!("/moderation/photos/parking/{photo_id}/hide"),
        &[("csrf", mcsrf.as_str())],
        &mod_cookie,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS hide redirects");
    assert_eq!(location_of(&head), "/moderation/photos?done=photo_hidden");

    let (s, head, _) = post_form_raw(
        &app,
        &format!("/moderation/photos/parking/{photo_id}/restore"),
        &[("csrf", mcsrf.as_str())],
        &mod_cookie,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS restore redirects");
    assert_eq!(location_of(&head), "/moderation/photos?done=photo_restored");
    // The queue renders the notice the redirect asked for.
    let (_, banner) = get_c(
        &app,
        "/moderation/photos?done=photo_restored",
        Some(&mod_cookie),
    )
    .await;
    assert!(
        banner.contains("Photo restored") || banner.contains("restored"),
        "queue shows the notice"
    );

    // Reports: submit → claim → resolve, each redirecting a no-JS caller.
    let reporter = unverified_cookie(&app, "wp10-reporter@example.com").await;
    let (_, rpage) = get_c(&app, &format!("/parking/{loc}"), Some(&reporter)).await;
    let rcsrf = extract_csrf(&rpage);
    let (s, head, _) = post_form_raw(
        &app,
        "/reports",
        &[
            ("csrf", rcsrf.as_str()),
            ("target_type", "parking"),
            ("target_id", &loc.to_string()),
            ("reason", "spam"),
            ("page", &format!("/parking/{loc}")),
        ],
        &reporter,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS report redirects");
    assert_eq!(location_of(&head), format!("/parking/{loc}?reported=1"));
    let report_id = last_report_id("wp10-reporter@example.com", "parking", loc).await;

    let (s, head, _) = post_form_raw(
        &app,
        &format!("/moderation/reports/{report_id}/claim"),
        &[("csrf", mcsrf.as_str())],
        &mod_cookie,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS claim redirects");
    assert_eq!(location_of(&head), "/moderation/reports?done=claimed");

    let (s, head, _) = post_form_raw(
        &app,
        &format!("/moderation/reports/{report_id}/dismiss"),
        &[("csrf", mcsrf.as_str()), ("note", "not spam")],
        &mod_cookie,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "no-JS dismiss redirects");
    assert_eq!(location_of(&head), "/moderation/reports?done=dismissed");

    sqlx::query("DELETE FROM report WHERE id = $1")
        .bind(report_id)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(UPLOADER).await;
    cleanup_user_contributions(MOD).await;
    cleanup_user_contributions("wp10-reporter@example.com").await;
}

/// A urlencoded POST that returns the raw response headers (for `Location`).
async fn post_form_raw(
    app: &axum::Router,
    uri: &str,
    fields: &[(&str, &str)],
    cookie: &str,
) -> (StatusCode, HeaderMap, String) {
    let body = fields
        .iter()
        .map(|(k, v)| format!("{}={}", k, urlencode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Accept-Language", "en")
                .header("cookie", cookie)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let head = res.headers().clone();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, head, String::from_utf8_lossy(&body).to_string())
}

// --- Session expiry on a fragment POST -------------------------------------

#[db_test]
async fn anonymous_fragment_post_gets_401_and_hx_redirect(_tx: &mut TestTx) {
    // htmx follows a 302/303 transparently and would swap the whole login page
    // into `#favorite-button`, so the gate answers with `HX-Redirect` instead.
    let (app, _) = auth_app().await;
    let (cookie_line, token) = anon_csrf(&app, "/login").await.expect("anon csrf pair");
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/parking/1/favorite")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Accept-Language", "en")
                .header("HX-Request", "true")
                .header("HX-Request-Type", "partial")
                .header("cookie", &cookie_line)
                .body(Body::from(format!("csrf={token}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    let redirect = res
        .headers()
        .get("hx-redirect")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(
        redirect.starts_with("/login?next="),
        "hx-redirect: {redirect}"
    );
}

#[db_test]
async fn anonymous_plain_post_redirects_to_login_with_next(_tx: &mut TestTx) {
    // The POST path is an action, not a page: `next` is the page it came from.
    let (app, _) = auth_app().await;
    let (cookie_line, token) = anon_csrf(&app, "/login").await.expect("anon csrf pair");
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/parking/7/favorite")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Accept-Language", "en")
                .header("cookie", &cookie_line)
                .body(Body::from(format!("csrf={token}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(location_of(res.headers()), "/login?next=/parking/7");
}

#[db_test]
async fn anonymous_page_request_carries_its_own_path_as_next(_tx: &mut TestTx) {
    let (app, _) = auth_app().await;
    let (s, head, _) = request_h(&app, "GET", "/account/favorites", None, &[]).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    assert_eq!(location_of(&head), "/login?next=/account/favorites");
}

// --- `next` after login -----------------------------------------------------

#[db_test]
async fn login_honours_a_local_next(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp10-next@example.com";
    let _ = verified_cookie(&app, &email, EMAIL).await;

    let (_, head, _) = {
        let (cookie_line, token) = anon_csrf(&app, "/login").await.expect("anon csrf");
        let body = format!(
            "email={}&password=password123&csrf={}&next={}",
            urlencode(EMAIL),
            token,
            urlencode("/parking/7?x=1")
        );
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("Accept-Language", "en")
                    .header("cookie", &cookie_line)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        (res.status(), res.headers().clone(), ())
    };
    assert_eq!(location_of(&head), "/parking/7?x=1");

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn login_refuses_an_off_site_next(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp10-next-evil@example.com";
    let _ = verified_cookie(&app, &email, EMAIL).await;

    for evil in [
        "//evil.com",
        r"/\evil.com",
        "https://evil.com",
        "javascript:x",
    ] {
        let (cookie_line, token) = anon_csrf(&app, "/login").await.expect("anon csrf");
        let body = format!(
            "email={}&password=password123&csrf={}&next={}",
            urlencode(EMAIL),
            token,
            urlencode(evil)
        );
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/x-www-form-urlencoded")
                    .header("Accept-Language", "en")
                    .header("cookie", &cookie_line)
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            location_of(res.headers()),
            "/account",
            "`next={evil}` must not leave the origin"
        );
    }

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn login_page_renders_a_valid_next_as_a_hidden_field(_tx: &mut TestTx) {
    let (app, _) = auth_app().await;
    let (_, page) = get_c(&app, "/login?next=%2Fparking%2F7", None).await;
    assert!(
        page.contains(r#"name="next" value="/parking/7""#),
        "hidden next field: {page}"
    );
    let (_, page) = get_c(&app, "/login?next=%2F%5Cevil.com", None).await;
    assert!(!page.contains(r#"name="next""#), "rejected next is dropped");
}

// --- CSRF: safe methods, token sources, multipart ---------------------------

#[db_test]
async fn head_requests_are_safe_and_not_csrf_checked(_tx: &mut TestTx) {
    // axum answers HEAD with the GET route; the middleware used to treat it as
    // state-changing and 403 every HEAD.
    let (app, _) = auth_app().await;
    let (s, _, _) = request_h(&app, "HEAD", "/login", None, &[]).await;
    assert_eq!(s, StatusCode::OK, "HEAD /login");
    let (s, _, _) = request_h(&app, "HEAD", "/", None, &[]).await;
    assert_eq!(s, StatusCode::OK, "HEAD /");
}

#[db_test]
async fn multipart_review_accepts_the_token_from_the_query(
    tx: &mut bikesnest_test_support::TestTx,
) {
    // The middleware must not drain a multipart body (the handler's `Multipart`
    // extractor needs it), so the form carries the token on its action.
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp10-review-csrf@example.com";
    let loc = fixture_location(tx, "wp10-review-csrf", "WP10 Review CSRF").await;
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, &format!("/parking/{loc}/review"), Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    let (s, _) = post_multipart(
        &app,
        &format!("/parking/{loc}/review?csrf={csrf}"),
        multipart_review("4", "Query-string token, no header.", "----bikesnestphoto"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "review accepted via ?csrf=");

    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn multipart_review_without_any_token_is_the_styled_error_page(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp10-review-nocsrf@example.com";
    let loc = fixture_location(tx, "wp10-review-nocsrf", "WP10 Review NoCSRF").await;
    let cookie = verified_cookie(&app, &email, EMAIL).await;

    let (s, body) = post_multipart(
        &app,
        &format!("/parking/{loc}/review"),
        multipart_review("4", "No token anywhere.", "----bikesnestphoto"),
        &cookie,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert!(is_document(&body), "styled page, not a bare string: {body}");
    assert!(!body.trim().eq("Forbidden"), "not the raw literal");
    assert!(body.contains("403"), "the status is on the page");

    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn an_axum_rejection_is_rendered_as_the_styled_error_page(
    tx: &mut bikesnest_test_support::TestTx,
) {
    // A urlencoded body to a multipart endpoint: axum's `Multipart` rejects it
    // with plain English text, which used to reach the user verbatim.
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp10-rejection@example.com";
    let loc = fixture_location(tx, "wp10-rejection", "WP10 Rejection").await;
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, &format!("/parking/{loc}/review"), Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/parking/{loc}/review"))
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Accept-Language", "en")
                .header("cookie", &cookie)
                .body(Body::from(format!("csrf={csrf}&rating=4&body=hello")))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let ctype = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&body).to_string();
    assert!(status.is_client_error(), "status: {status}");
    assert!(ctype.starts_with("text/html"), "content-type: {ctype}");
    assert!(is_document(&body), "styled page: {body}");

    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn plain_text_endpoints_keep_their_plain_bodies(_tx: &mut TestTx) {
    // The styled-error fallback must not touch the probes.
    let (s, body) = get("/healthz").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body.trim(), "ok");
}

// --- Template hygiene -------------------------------------------------------

fn read_template(rel: &str) -> String {
    let path =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// One of the shipped front-end files, read from the source tree — the map's
/// behaviour lives in JS a request cannot assert on.
fn read_static(rel: &str) -> String {
    let path =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/static")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn the_map_toggle_works_at_every_breakpoint() {
    let page = read_template("pages/search.html");
    let toggle = page
        .lines()
        .find(|l| l.contains(r#"@click="toggleMap""#))
        .expect("the map toggle is in search.html");
    // `lg:hidden` used to make the desktop map impossible to put away.
    assert!(!toggle.contains("lg:hidden"), "{toggle}");
    // The results column's width is the Alpine binding's alone: a static
    // `lg:col-span-7` alongside `:class="resultsClass"` fights it.
    assert!(
        page.contains(r#"<div :class="resultsClass">"#),
        "the results column is driven only by the binding: {page}"
    );
    assert!(
        !page.contains(r#"class="lg:col-span-7" :class="resultsClass""#),
        "{page}"
    );
    // Both widths still have to reach the stylesheet.
    let css = read_static("css/app.css");
    assert!(css.contains(r"lg\:col-span-7"), "narrow column generated");
    assert!(css.contains(r"lg\:col-span-12"), "wide column generated");
}

#[test]
fn the_search_map_is_built_lazily_and_resized_on_reveal() {
    let js = read_static("js/search.js");
    // A map built inside a hidden panel measures 0×0 and renders blank.
    assert!(js.contains("resize("), "the map is resized: {js}");
    assert!(
        js.contains("bikesnest:map-toggle"),
        "the reveal is announced"
    );
    assert!(
        js.contains("ResizeObserver"),
        "belt and braces for the reveal"
    );
    assert!(js.contains("bikesnest:map-moved"), "moves offer a new area");
    let app = read_static("js/app.js");
    assert!(
        app.contains("bikesnest:map-toggle") && app.contains("bikesnest:map-moved"),
        "the Alpine side of both events: {app}"
    );
    assert!(
        app.contains("bn.search.mapOpen"),
        "the panel's state is remembered per viewer"
    );
}

#[test]
fn map_markers_are_controls_and_their_popups_are_built_as_nodes() {
    let js = read_static("js/search.js");
    assert!(js.contains(r#"setAttribute("role", "button")"#), "{js}");
    assert!(js.contains(r#"setAttribute("tabindex", "0")"#), "{js}");
    assert!(js.contains("keydown"), "Enter/Space open the popup");
    // A location's name is user content: it may only ever be written as text.
    assert!(
        js.contains("createElement") && js.contains("textContent"),
        "{js}"
    );
    assert!(
        !js.contains(".innerHTML") && !js.contains("innerHTML ="),
        "popup content must never be assembled as markup: {js}"
    );
    assert!(js.contains("setDOMContent"), "the popup takes nodes: {js}");
}

#[test]
fn the_favorite_button_is_defined_exactly_once() {
    let templates_dir =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates"));
    let mut hits = Vec::new();
    let mut stack = vec![templates_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(contents) = std::fs::read_to_string(&path) {
                for _ in contents.matches(r#"id="favorite-button""#) {
                    hits.push(path.display().to_string());
                }
            }
        }
    }
    assert_eq!(
        hits.len(),
        1,
        "the favorite control must live in one partial only: {hits:?}"
    );
    assert!(
        hits[0].ends_with("partials/favorite_button.html"),
        "{hits:?}"
    );
}

#[test]
fn the_favorite_button_guards_against_a_double_click() {
    let partial = read_template("partials/favorite_button.html");
    assert!(partial.contains(r#"hx-disable="this""#), "{partial}");
    assert!(partial.contains(r#"hx-sync="this:drop""#), "{partial}");
}

#[test]
fn the_layout_boosts_through_the_inherited_attribute() {
    // htmx 4 defaults `implicitInheritance` to false: a bare `hx-boost` on
    // <body> boosts nothing below it.
    let base = read_template("layouts/base.html");
    assert!(base.contains(r#"hx-boost:inherited="true""#), "{base}");
    assert!(
        !base.contains(r#"hx-boost="true""#),
        "the inert plain attribute must be gone"
    );
}

#[test]
fn the_account_menu_resets_around_boosted_navigation() {
    let base = read_template("layouts/base.html");
    assert!(
        base.contains(r#"@htmx:before:request.window="close""#),
        "an open menu should close as soon as navigation starts: {base}"
    );
    assert!(
        base.contains(r#"@htmx:after:swap.window="close""#),
        "the newly morphed account menu must reset its restored Alpine state: {base}"
    );
    assert!(
        base.contains(r#"@pageshow.window="close""#),
        "browser history-cache restores must clear transient menu state too: {base}"
    );
    assert!(
        base.contains(r#"x-show="open" x-cloak @click="close" style="display: none""#),
        "clicking a menu item must synchronously clear the ephemeral open state: {base}"
    );
    assert!(
        base.contains(r#"style="display: none""#),
        "the server-rendered menu must stay hidden even when a morph removes Alpine's generated style: {base}"
    );
}

#[test]
fn the_review_form_posts_plainly_with_the_token_in_the_query() {
    let form = read_template("pages/review_form.html");
    assert!(
        !form.contains(r#"hx-post=""#),
        "an hx-post with no target would swap the redirect's destination into the form"
    );
    assert!(form.contains("?csrf={{ layout.csrf }}"), "{form}");
    assert!(form.contains(r#"enctype="multipart/form-data""#), "{form}");
}

#[test]
fn the_report_modal_targets_a_container_inside_itself() {
    let page = read_template("pages/parking_details.html");
    assert!(
        page.contains(r##"hx-target="#report-modal-feedback""##),
        "feedback must land inside the modal"
    );
    assert!(
        page.contains(r#"id="report-modal-feedback""#),
        "the target must exist"
    );
    assert!(
        !page.contains("hx-swap-oob"),
        "the inert out-of-band marker on a live target is gone"
    );
    assert!(
        !page.contains("submitClose"),
        "the modal no longer closes on a timer regardless of outcome"
    );
    assert!(
        page.contains(r#"@htmx:after:request="afterRequest""#),
        "the modal closes from the response status"
    );
    assert!(
        page.contains("@keydown.escape.window"),
        "escape closes the modal (parity with the lightbox)"
    );
}

// --- Error pages honour the request shape too -------------------------------

#[db_test]
async fn a_404_for_a_fragment_request_is_a_fragment(_tx: &mut TestTx) {
    // A stale htmx control polling a route that no longer exists must not get a
    // whole document swapped into its target.
    let app = test_app().await;
    let (s, head, body) = request_h(&app, "GET", "/nonexistent", None, HX_FRAGMENT).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(body.contains(r#"role="alert""#), "fragment_error: {body}");
    assert!(!body.contains("<html"), "must not be a document: {body}");
    let vary = vary_of(&head);
    assert!(vary.contains("hx-request"), "vary: {vary}");
}

#[db_test]
async fn a_404_for_a_whole_document_request_is_the_styled_page(_tx: &mut TestTx) {
    let (s, body) = get("/nonexistent").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(is_document(&body), "styled error page: {body}");
    assert!(body.contains("404"), "the status is on the page");
}

#[db_test]
async fn a_404_for_a_boosted_request_is_the_styled_page(_tx: &mut TestTx) {
    // A boosted link swaps <body>, so it needs the whole document even though
    // it carries `HX-Request: true`.
    let app = test_app().await;
    let (s, _, body) = request_h(
        &app,
        "GET",
        "/nonexistent",
        None,
        &[
            ("HX-Request", "true"),
            ("HX-Boosted", "true"),
            ("HX-Request-Type", "full"),
        ],
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(is_document(&body), "styled error page: {body}");
}

#[db_test]
async fn a_missing_parking_page_answers_in_the_requests_shape(
    tx: &mut bikesnest_test_support::TestTx,
) {
    // `parking_details` used to emit a whole document for every caller.
    let app = test_app().await;
    let (s, _, body) = request_h(&app, "GET", "/parking/0", None, HX_FRAGMENT).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(body.contains(r#"role="alert""#), "fragment_error: {body}");
    assert!(!body.contains("<html"), "must not be a document: {body}");

    let (s, body) = get("/parking/0").await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert!(is_document(&body), "styled error page: {body}");
    let _ = tx;
}

#[db_test]
async fn the_anonymous_htmx_401_carries_exactly_one_vary(_tx: &mut TestTx) {
    let (app, _) = auth_app().await;
    let (cookie_line, token) = anon_csrf(&app, "/login").await.expect("anon csrf pair");
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/parking/1/favorite")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Accept-Language", "en")
                .header("HX-Request", "true")
                .header("HX-Request-Type", "partial")
                .header("cookie", &cookie_line)
                .body(Body::from(format!("csrf={token}")))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    // `CompressionLayer` (outermost, WP14) only appends its own
    // `Vary: accept-encoding` to a response it actually compresses, and this
    // one is `text/html` — excluded from compression (BREACH) — so it never
    // gets that extra header. The strict count still guards the app's own
    // invariant: the styled-error fallback must skip an already-rendered
    // response instead of appending a second `Vary`.
    assert_eq!(
        res.headers()
            .get_all(axum::http::header::VARY)
            .iter()
            .count(),
        1,
        "the styled-error fallback must skip an already-rendered response"
    );
    assert!(
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/html"),
        "the gate renders its own body"
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&body).to_string();
    assert!(body.contains(r#"role="alert""#), "fragment_error: {body}");
    assert!(!body.contains("<html"), "must not be a document: {body}");
}

// ---------------------------------------------------------------------------
// WP12: navigation and identity in the layout
// ---------------------------------------------------------------------------

#[db_test]
async fn anonymous_login_page_header_has_no_signed_in_links(_tx: &mut TestTx) {
    let (status, body) = get("/login").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("action=\"/logout\""),
        "anonymous header must not show a logout form: {body}"
    );
    assert!(
        !body.contains("href=\"/account\""),
        "anonymous header must not link /account: {body}"
    );
    assert!(
        body.contains("href=\"/login\""),
        "Entrar/Log in link present: {body}"
    );
    assert!(
        body.contains("href=\"/register\""),
        "Criar conta/Sign up link present: {body}"
    );
}

#[db_test]
async fn signed_in_user_sees_account_links_on_policy_and_error_pages(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    let cookie = verified_cookie(&app, &email, "wp12-header-privacy@example.com").await;
    for uri in ["/privacy", "/terms", "/this-route-does-not-exist-wp12"] {
        let (status, body) = get_c(&app, uri, Some(&cookie)).await;
        assert!(
            status == StatusCode::OK || status == StatusCode::NOT_FOUND,
            "{uri}: unexpected status {status}"
        );
        assert!(
            body.contains("action=\"/logout\""),
            "{uri}: logout form present: {body}"
        );
        assert!(
            body.contains("href=\"/account/favorites\""),
            "{uri}: favorites link present: {body}"
        );
        assert!(
            body.contains("href=\"/account/contributions\""),
            "{uri}: contributions link present: {body}"
        );
        assert!(
            !body.contains("href=\"/moderation\""),
            "{uri}: plain user has no moderation link: {body}"
        );
        assert!(
            !body.contains("href=\"/admin/users\""),
            "{uri}: plain user has no admin link: {body}"
        );
    }
    let _ = tx;
    cleanup_user_contributions("wp12-header-privacy@example.com").await;
}

#[db_test]
async fn header_shows_moderation_and_admin_links_by_role(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;

    let moderator = moderator_cookie(&app, &email, "wp12-header-mod@example.com").await;
    let (_, body) = get_c(&app, "/", Some(&moderator)).await;
    assert!(
        body.contains("href=\"/moderation\""),
        "moderator sees the moderation link: {body}"
    );
    assert!(
        !body.contains("href=\"/admin/users\""),
        "a moderator (not admin) has no admin link: {body}"
    );

    let admin = admin_cookie(&app, &email, "wp12-header-admin@example.com").await;
    let (_, body) = get_c(&app, "/", Some(&admin)).await;
    assert!(
        body.contains("href=\"/admin/users\""),
        "admin sees the admin link: {body}"
    );
    assert!(
        body.contains("href=\"/admin/audit\""),
        "admin sees the audit link: {body}"
    );

    let plain = verified_cookie(&app, &email, "wp12-header-plain@example.com").await;
    let (_, body) = get_c(&app, "/", Some(&plain)).await;
    assert!(
        !body.contains("href=\"/moderation\""),
        "plain user has no moderation link: {body}"
    );
    assert!(
        !body.contains("href=\"/admin/users\""),
        "plain user has no admin link: {body}"
    );

    let _ = tx;
    cleanup_user_contributions("wp12-header-mod@example.com").await;
    cleanup_user_contributions("wp12-header-admin@example.com").await;
    cleanup_user_contributions("wp12-header-plain@example.com").await;
}

#[db_test]
async fn add_spot_entry_points_are_gated_by_verification_status(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const MARK: &str = "wp12-add-spot-cta";
    const Q: &str = "/search?q=Rua%20XV%20de%20Novembro";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Add Spot CTA Fixture")
        .with_fixture_tag(MARK)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    let loc_id = loc.id();

    let (app, email) = auth_app().await;

    // Verified user: the real entry point on every page named in the plan.
    let verified = verified_cookie(&app, &email, "wp12-add-spot-verified@example.com").await;
    for uri in [
        Q.to_string(),
        "/about".to_string(),
        format!("/parking/{loc_id}"),
    ] {
        let (s, body) = get_c(&app, &uri, Some(&verified)).await;
        assert_eq!(s, StatusCode::OK, "{uri}");
        assert!(
            body.contains("href=\"/parking/new\""),
            "{uri}: add-a-spot entry point present: {body}"
        );
    }

    // Anonymous: signup-to-add CTA, not the real entry point.
    let (s, body) = get_c(&app, Q, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("href=\"/register\""),
        "anonymous CTA links to /register: {body}"
    );
    assert!(
        body.contains("Create an account to add a spot"),
        "signup-to-add copy present: {body}"
    );

    // Signed in but unverified: verify-to-contribute nudge, not the real entry point.
    let unverified = unverified_cookie(&app, "wp12-add-spot-unverified@example.com").await;
    let (s, body) = get_c(&app, Q, Some(&unverified)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("Verify your email to contribute"),
        "verify-to-contribute copy present: {body}"
    );

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("wp12-add-spot-verified@example.com").await;
    cleanup_user_contributions("wp12-add-spot-unverified@example.com").await;
}

#[db_test]
async fn about_page_links_entry_points_and_uses_present_tense_copy(_tx: &mut TestTx) {
    let (status, body) = get("/about").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("href=\"/parking/new\""),
        "add-a-spot card links to /parking/new: {body}"
    );
    assert!(
        body.matches("href=\"/search\"").count() >= 3,
        "verify/review/report cards link to /search: {body}"
    );
    assert!(
        body.contains("These tools are live today"),
        "present-tense copy present: {body}"
    );
    assert!(
        !body.contains("arrive as the project grows"),
        "old future-tense copy must be gone: {body}"
    );

    // pt-BR (default locale): the old "chegam conforme" copy must be gone too.
    let app = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri("/about")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&body).to_string();
    assert!(
        !body.contains("chegam conforme"),
        "old pt-BR copy must be gone: {body}"
    );
    assert!(
        body.contains("já estão disponíveis"),
        "new pt-BR copy present: {body}"
    );
}

#[db_test]
async fn moderation_dashboard_tiles_have_distinct_titles(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    let admin = admin_cookie(&app, &email, "wp12-mod-tiles@example.com").await;
    let (s, body) = get_c(&app, "/moderation", Some(&admin)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Open reports"), "{body}");
    assert!(body.contains("Reports in review"), "{body}");
    assert!(body.contains("Awaiting review"), "{body}");
    let _ = tx;
    cleanup_user_contributions("wp12-mod-tiles@example.com").await;
}

#[db_test]
async fn contributions_history_labels_parked_here_distinct_from_verified(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const MARK: &str = "wp12-parked-here";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Parked Here Fixture")
        .with_fixture_tag(MARK)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    let loc_id = loc.id();

    let (app, email) = auth_app().await;
    let cookie = verified_cookie(&app, &email, "wp12-parked-here@example.com").await;

    let (s, page) = get_c(&app, &format!("/parking/{loc_id}"), Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    let csrf = extract_csrf(&page);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/parking/{loc_id}/parked-here"),
        &[("csrf", &csrf)],
        Some(&cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "parked-here signal recorded");

    let (s, body) = get_c(&app, "/account/contributions", Some(&cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("Parked here · Parked Here Fixture"),
        "row has its own label, not \"Verified\": {body}"
    );
    assert!(
        !body.contains("Verified · Parked Here Fixture"),
        "must not read as a real verification: {body}"
    );

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions("wp12-parked-here@example.com").await;
}

// ---------------------------------------------------------------------------
// WP13 — moderation queues that show what they judge.
// ---------------------------------------------------------------------------

/// Insert a PENDING proposal directly, the way M3 wrote them: a JSONB payload
/// whose shape the typed [`bikesnest_domain::ProposedChange`] has to keep
/// reading unchanged.
async fn seed_proposal(
    location_id: i64,
    proposer_id: i64,
    base_version: i64,
    kind: &str,
    proposed: &str,
) -> i64 {
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
         VALUES ($1, $2, $3, $4, $5::jsonb, 'PENDING') RETURNING id",
    )
    .bind(location_id)
    .bind(proposer_id)
    .bind(base_version)
    .bind(kind)
    .bind(proposed)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    id
}

async fn user_id_for(email: &str) -> i64 {
    let (id,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    id
}

#[db_test]
async fn wp13_proposal_queue_prefills_the_move_and_links_the_location(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const MOD: &str = "wp13-prop-mod@example.com";
    const PROPOSER: &str = "wp13-prop-author@example.com";
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "wp13-prop", "WP13 Proposal Spot").await;
    let proposer = verified_cookie(&app, &email, PROPOSER).await;
    let _ = proposer;
    let proposer_id = user_id_for(PROPOSER).await;
    let (version,): (i64,) = sqlx::query_as("SELECT version FROM parking_location WHERE id = $1")
        .bind(loc)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    let pid = seed_proposal(
        loc,
        proposer_id,
        version,
        "move_location",
        r#"{"lat": -25.428400, "lon": -49.273300, "timezone": "America/Sao_Paulo", "reason": "the rack is across the street"}"#,
    )
    .await;

    // The queue is FIFO (`id ASC`) over a 50-row page and the database already
    // holds a backlog, so page straight to this fixture's own row with the
    // handler's real keyset cursor.
    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let (s, body) = get_c(
        &app,
        &format!("/moderation/proposals?after_id={}", pid - 1),
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // The row names the location and links to it, instead of showing an id.
    assert!(
        body.contains("WP13 Proposal Spot"),
        "the queue names the location"
    );
    assert!(
        body.contains(&format!(r#"href="/parking/{loc}""#)),
        "the row links to the location page"
    );
    // The proposer's note is the "why" a moderator needs.
    assert!(
        body.contains("the rack is across the street"),
        "the proposer's reason is shown"
    );
    // The approve form is pre-filled: approving as-is must be one click, not a
    // retyping exercise.
    assert!(
        body.contains(r#"name="lat" value="-25.428400""#),
        "latitude is pre-filled with the proposed value"
    );
    assert!(
        body.contains(r#"name="lon" value="-49.273300""#),
        "longitude is pre-filled with the proposed value"
    );
    assert!(
        body.contains(r#"name="timezone" value="America/Sao_Paulo""#),
        "timezone is pre-filled with the proposed value"
    );
    // Current vs proposed, plus the two-marker mini-map.
    assert!(
        body.contains("data-current-lat=") && body.contains("proposal-map"),
        "a move renders the before/after mini-map"
    );

    // Approving with the pre-filled values applies exactly them.
    let csrf = extract_csrf(&body);
    let (s, _, _) = post_form(
        &app,
        &format!("/moderation/proposals/{pid}/approve"),
        &[
            ("csrf", &csrf),
            ("lat", "-25.428400"),
            ("lon", "-49.273300"),
            ("timezone", "America/Sao_Paulo"),
        ],
        Some(&mod_cookie),
    )
    .await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::OK),
        "approval accepted: {s}"
    );
    let (lat, status): (Option<f64>, String) = sqlx::query_as(
        "SELECT l.lat, p.status FROM parking_location l \
         JOIN parking_proposal p ON p.id = $2 WHERE l.id = $1",
    )
    .bind(loc)
    .bind(pid)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(status, "APPROVED");
    assert!(
        lat.is_some_and(|v| (v - -25.4284).abs() < 1e-5),
        "the proposed latitude was applied: {lat:?}"
    );

    let _ = tx;
    cleanup_user_contributions(MOD).await;
    cleanup_user_contributions(PROPOSER).await;
}

#[db_test]
async fn wp13_proposal_queue_flags_stale_and_unreadable_proposals(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const MOD: &str = "wp13-stale-mod@example.com";
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "wp13-stale", "WP13 Stale Spot").await;
    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let mod_id = user_id_for(MOD).await;

    // Written against v1, but the location has moved on to v7: approving it
    // would clobber an edit the proposer never saw.
    sqlx::query("UPDATE parking_location SET version = 7 WHERE id = $1")
        .bind(loc)
        .execute(&pool().await)
        .await
        .unwrap();
    let stale = seed_proposal(
        loc,
        mod_id,
        1,
        "change_existence",
        r#"{"existence":"removed"}"#,
    )
    .await;
    // A payload this build cannot read must degrade to a card, not a 500.
    let unreadable = seed_proposal(
        loc,
        mod_id,
        7,
        "change_existence",
        r#"{"existence":"who_knows"}"#,
    )
    .await;

    let queue_url = format!("/moderation/proposals?after_id={}", stale - 1);
    let (s, body) = get_c(&app, &queue_url, Some(&mod_cookie)).await;
    assert_eq!(s, StatusCode::OK, "an unreadable payload does not 500");
    assert!(
        body.contains("Out of date"),
        "the stale proposal carries a distinct badge"
    );
    assert!(
        body.contains("Needs manual review"),
        "the unreadable payload is flagged instead of crashing the page"
    );

    // `hx-confirm` is client-side only: the attribute must be in the markup,
    // and the server must still accept the POST without it.
    assert!(
        body.contains("hx-confirm="),
        "approving a removal asks for confirmation"
    );
    let csrf = extract_csrf(&body);
    let (s, _, _) = post_form(
        &app,
        &format!("/moderation/proposals/{stale}/approve"),
        &[("csrf", &csrf), ("existence", "removed")],
        Some(&mod_cookie),
    )
    .await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::CONFLICT),
        "a stale approval is refused server-side: {s}"
    );
    let (status,): (String,) = sqlx::query_as("SELECT status FROM parking_proposal WHERE id = $1")
        .bind(stale)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(status, "PENDING", "a refused approval changes nothing");

    // An unreadable payload with the values supplied still goes through — the
    // moderator is the fallback, not a dead end.
    let (s, _, _) = post_form(
        &app,
        &format!("/moderation/proposals/{unreadable}/approve"),
        &[("csrf", &csrf), ("existence", "removed")],
        Some(&mod_cookie),
    )
    .await;
    assert!(
        matches!(s, StatusCode::SEE_OTHER | StatusCode::OK),
        "the moderator can approve an unreadable payload by hand: {s}"
    );
    let (state,): (String,) =
        sqlx::query_as("SELECT moderation_state FROM parking_location WHERE id = $1")
            .bind(loc)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "REMOVED");

    let _ = tx;
    cleanup_user_contributions(MOD).await;
}

#[db_test]
async fn wp13_report_queue_previews_and_links_its_targets(tx: &mut bikesnest_test_support::TestTx) {
    const MOD: &str = "wp13-rep-mod@example.com";
    const AUTHOR: &str = "wp13-rep-author@example.com";
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "wp13-rep", "WP13 Reported Spot").await;
    let author_cookie = verified_cookie(&app, &email, AUTHOR).await;
    let author_id = user_id_for(AUTHOR).await;

    // A review to report, with a body long enough to be excerpted.
    let long_body = format!("{} tail-that-must-be-cut", "spam ".repeat(60));
    let (review_id,): (i64,) = sqlx::query_as(
        "INSERT INTO review (location_id, author_id, rating, body, moderation_state) \
         VALUES ($1, $2, 1, $3, 'ACTIVE') RETURNING id",
    )
    .bind(loc)
    .bind(author_id)
    .bind(&long_body)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    let _ = author_cookie;

    let mut first_report = i64::MAX;
    for (target_type, target_id) in [("parking", loc), ("review", review_id)] {
        let (rid,): (i64,) = sqlx::query_as(
            "INSERT INTO report (reporter_id, target_type, target_id, reason, state) \
             VALUES ($1, $2, $3, 'spam', 'OPEN') RETURNING id",
        )
        .bind(author_id)
        .bind(target_type)
        .bind(target_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
        first_report = first_report.min(rid);
    }

    // FIFO queue, 50-row page, existing backlog: page to this fixture's rows.
    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let queue_url = format!(
        "/moderation/reports?state=OPEN&after_id={}",
        first_report - 1
    );
    let (s, body) = get_c(&app, &queue_url, Some(&mod_cookie)).await;
    assert_eq!(s, StatusCode::OK);

    // The row names and links the target instead of printing `#4057`.
    assert!(
        body.contains("WP13 Reported Spot"),
        "the row names the reported location"
    );
    assert!(
        body.contains(&format!(r#"href="/parking/{loc}""#)),
        "the parking report links to the location"
    );
    assert!(
        body.contains(&format!(r#"href="/parking/{loc}#review-{review_id}""#)),
        "the review report links to the review itself"
    );
    // The excerpt shows what is being judged, cut to a queue-sized length.
    assert!(
        body.contains("spam spam"),
        "the review report shows a body excerpt"
    );
    assert!(
        !body.contains("tail-that-must-be-cut"),
        "the excerpt is truncated, not the whole 300-character body"
    );
    // Acting on the content posts to the endpoints that already existed.
    assert!(
        body.contains(&format!(r#"action="/moderation/reviews/{review_id}/hide""#)),
        "the review row offers the existing hide action"
    );
    assert!(
        body.contains(&format!(r#"action="/moderation/parking/{loc}/invalidate""#)),
        "the parking row offers the existing invalidate action"
    );
    assert!(
        body.contains("hx-confirm="),
        "acting on reported content asks for confirmation"
    );

    // The action really works from the queue.
    let csrf = extract_csrf(&body);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/reviews/{review_id}/hide"),
        &[("csrf", &csrf)],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "hide from the queue: {s}");
    let (state,): (String,) = sqlx::query_as("SELECT moderation_state FROM review WHERE id = $1")
        .bind(review_id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(state, "HIDDEN");

    // Once hidden, the queue stops offering an action that would just fail.
    let (_, body) = get_c(&app, &queue_url, Some(&mod_cookie)).await;
    assert!(
        !body.contains(&format!(r#"action="/moderation/reviews/{review_id}/hide""#)),
        "an already-hidden review is not offered for hiding again"
    );

    let _ = tx;
    sqlx::query("DELETE FROM report WHERE reporter_id = $1")
        .bind(author_id)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(MOD).await;
    cleanup_user_contributions(AUTHOR).await;
}

#[db_test]
async fn wp13_photo_queue_refuses_to_approve_an_image_it_cannot_show(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const MOD: &str = "wp13-photo-mod@example.com";
    const UPLOADER: &str = "wp13-photo-up@example.com";
    let (app, email, storage) = auth_app_with_storage().await;
    let loc = fixture_location(tx, "wp13-photo", "WP13 Photo Spot").await;
    let uploader_id = {
        let cookie = verified_cookie(&app, &email, UPLOADER).await;
        let _ = cookie;
        user_id_for(UPLOADER).await
    };

    // A pending photo whose object was never written to storage: exactly the
    // state that used to render as a broken image with a live Approve button.
    let (photo_id,): (i64,) = sqlx::query_as(
        "INSERT INTO parking_photo (location_id, uploader_id, storage_key, content_type, alt, position, moderation_state) \
         VALUES ($1, $2, 'uploads/wp13-missing.jpg', 'image/jpeg', 'A missing photo', 0, 'PENDING_REVIEW') RETURNING id",
    )
    .bind(loc)
    .bind(uploader_id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert!(
        !storage.contains("uploads/wp13-missing.jpg"),
        "the fixture's whole point is that the object is absent"
    );

    let mod_cookie = moderator_cookie(&app, &email, MOD).await;
    let (s, body) = get_c(&app, "/moderation/photos", Some(&mod_cookie)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("Image unavailable"),
        "a missing object is named, not rendered as a broken <img>"
    );
    assert!(
        body.contains("disabled"),
        "the Approve button is disabled for an image nobody can see"
    );
    assert!(
        body.contains("File missing from storage"),
        "the rejection reason is pre-filled"
    );

    // The reject path still works, so the queue can be cleared.
    let csrf = extract_csrf(&body);
    let (s, _, _) = post_form_hx(
        &app,
        &format!("/moderation/photos/parking/{photo_id}/reject"),
        &[("csrf", &csrf), ("reason", "File missing from storage")],
        Some(&mod_cookie),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "rejecting a lost file: {s}");
    let (state,): (String,) =
        sqlx::query_as("SELECT moderation_state FROM parking_photo WHERE id = $1")
            .bind(photo_id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "REJECTED");

    let _ = tx;
    cleanup_user_contributions(MOD).await;
    cleanup_user_contributions(UPLOADER).await;
}

#[db_test]
async fn wp13_audit_log_shows_exact_times_and_named_actors(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const ADMIN: &str = "wp13-audit-admin@example.com";
    let (app, email) = auth_app().await;
    let admin = admin_cookie(&app, &email, ADMIN).await;
    let admin_id = user_id_for(ADMIN).await;
    sqlx::query("UPDATE users SET display_name = 'Ada Audit' WHERE id = $1")
        .bind(admin_id)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, result, metadata) \
         VALUES ($1, 'wp13.audit.probe', 'user', $2, 'success', '{}'::jsonb)",
    )
    .bind(admin_id)
    .bind(admin_id.to_string())
    .execute(&pool().await)
    .await
    .unwrap();

    let (s, body) = get_c(&app, "/admin/audit?action=wp13.audit.probe", Some(&admin)).await;
    assert_eq!(s, StatusCode::OK);
    // An audit trail read to answer "exactly when" cannot say "today".
    let date = regex_lite_date(&body);
    assert!(
        date.is_some(),
        "the audit row carries an absolute YYYY-MM-DD timestamp"
    );
    assert!(body.contains("UTC"), "the exact instant names its zone");
    // The actor is a person, not an opaque id.
    assert!(
        body.contains("Ada Audit"),
        "the actor id is resolved to a display label"
    );
    assert!(
        body.contains(&format!(r#"href="/admin/users?q={admin_id}""#)),
        "the actor links to their account row"
    );

    // The date filters are pickers, and a picked value round-trips.
    assert!(
        body.contains(r#"type="datetime-local""#),
        "date filters are datetime-local inputs, not hand-typed ISO strings"
    );
    let (s, body) = get_c(
        &app,
        "/admin/audit?action=wp13.audit.probe&from=2020-01-02T03%3A04",
        Some(&admin),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains(r#"value="2020-01-02T03:04""#),
        "a datetime-local filter is echoed back in the field"
    );
    assert!(
        body.contains("wp13.audit.probe"),
        "the event is still inside the filtered window"
    );
    // An ISO string from a bookmarked URL keeps working.
    let (s, body) = get_c(
        &app,
        "/admin/audit?action=wp13.audit.probe&from=2020-01-02T03%3A04%3A05Z",
        Some(&admin),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("wp13.audit.probe"),
        "a legacy RFC3339 filter still parses"
    );

    let _ = tx;
    let mut audit_tx = bikesnest_test_support::audit_mutation_tx(&pool().await).await;
    sqlx::query("DELETE FROM audit_events WHERE action = 'wp13.audit.probe'")
        .execute(&mut *audit_tx)
        .await
        .unwrap();
    audit_tx.commit().await.unwrap();
    cleanup_user_contributions(ADMIN).await;
}

/// Is there a `YYYY-MM-DD` anywhere in the page? (No regex crate in the web
/// test deps, and this is the whole pattern the assertion needs.)
fn regex_lite_date(html: &str) -> Option<&str> {
    let bytes = html.as_bytes();
    for i in 0..bytes.len().saturating_sub(10) {
        let w = &bytes[i..i + 10];
        let digits = |b: u8| b.is_ascii_digit();
        if digits(w[0])
            && digits(w[1])
            && digits(w[2])
            && digits(w[3])
            && w[4] == b'-'
            && digits(w[5])
            && digits(w[6])
            && w[7] == b'-'
            && digits(w[8])
            && digits(w[9])
        {
            return Some(&html[i..i + 10]);
        }
    }
    None
}

#[db_test]
async fn wp13_privacy_queue_shows_the_subject_and_what_they_asked(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const ADMIN: &str = "wp13-priv-admin@example.com";
    const SUBJECT: &str = "wp13-priv-subject@example.com";
    let (app, email) = auth_app().await;
    let subject_cookie = verified_cookie(&app, &email, SUBJECT).await;
    let _ = subject_cookie;
    let subject_id = user_id_for(SUBJECT).await;
    sqlx::query("UPDATE users SET display_name = 'Rita Rights' WHERE id = $1")
        .bind(subject_id)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO privacy_request (user_id, kind, state, details) \
         VALUES ($1, 'rectification', 'OPEN', '{\"note\":\"my display name is misspelled\"}'::jsonb)",
    )
    .bind(subject_id)
    .execute(&pool().await)
    .await
    .unwrap();

    let admin = admin_cookie(&app, &email, ADMIN).await;
    let (s, body) = get_c(&app, "/admin/privacy-requests", Some(&admin)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("Rita Rights"),
        "the queue says whose rights these are"
    );
    assert!(
        body.contains("my display name is misspelled"),
        "the queue shows what the subject actually asked for"
    );
    assert!(
        body.contains(&format!(r#"href="/admin/users?q={subject_id}""#)),
        "the subject links to their account row"
    );
    assert!(
        body.contains("days left") || body.contains("days overdue"),
        "the legal deadline is on the row"
    );
    assert!(
        regex_lite_date(&body).is_some(),
        "the requested-at timestamp is absolute"
    );

    let _ = tx;
    sqlx::query("DELETE FROM privacy_request WHERE user_id = $1")
        .bind(subject_id)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user_contributions(ADMIN).await;
    cleanup_user_contributions(SUBJECT).await;
}

#[db_test]
async fn wp13_admin_user_list_searches_masks_and_confirms(tx: &mut bikesnest_test_support::TestTx) {
    const ADMIN: &str = "wp13-users-admin@example.com";
    const NEEDLE: &str = "wp13-findme@example.com";
    const OTHER: &str = "wp13-other@example.com";
    let (app, email) = auth_app().await;
    for addr in [NEEDLE, OTHER] {
        let cookie = verified_cookie(&app, &email, addr).await;
        let _ = cookie;
    }
    let admin = admin_cookie(&app, &email, ADMIN).await;
    let needle_id = user_id_for(NEEDLE).await;

    // Unfiltered: both accounts are listed, and neither address is in plain
    // sight — the masked form is what the row shows.
    let (s, body) = get_c(&app, "/admin/users", Some(&admin)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        body.contains("w***@example.com"),
        "emails are masked by default"
    );
    assert!(
        body.contains("revealEmail"),
        "a reveal control is offered in the row"
    );

    // Search narrows the page to the matching account.
    let (s, body) = get_c(&app, "/admin/users?q=wp13-findme", Some(&admin)).await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains(NEEDLE), "the match is on the page");
    assert!(
        !body.contains(OTHER),
        "a non-matching account is filtered out"
    );

    // Search matches display names too.
    sqlx::query("UPDATE users SET display_name = 'Zebedee Unique' WHERE id = $1")
        .bind(needle_id)
        .execute(&pool().await)
        .await
        .unwrap();
    let (_, body) = get_c(&app, "/admin/users?q=Zebedee", Some(&admin)).await;
    assert!(
        body.contains("Zebedee Unique"),
        "the search matches display names, not only emails"
    );
    assert!(!body.contains(OTHER), "and still narrows the list");

    // A search with no matches says so instead of listing everyone.
    let (_, body) = get_c(&app, "/admin/users?q=no-such-account-xyz", Some(&admin)).await;
    assert!(
        body.contains("No accounts match"),
        "an empty search result is stated, not silently the whole table"
    );

    // Destructive actions confirm first, naming the user.
    let (_, body) = get_c(&app, "/admin/users?q=Zebedee", Some(&admin)).await;
    assert!(
        body.contains(r#"hx-confirm="Suspend Zebedee Unique?"#),
        "suspend confirms and names the user"
    );
    assert!(
        body.contains("hx-confirm=\"Grant MODERATOR to Zebedee Unique?"),
        "granting a role confirms and names the user"
    );
    // Activity columns are present (the list is no longer email-only).
    assert!(
        body.contains("Last active") && body.contains("Contributions"),
        "the row carries last-active and contribution counters"
    );

    // The confirm is client-side only: the POST still works without it.
    let csrf = extract_csrf(&body);
    let (s, _, _) = post_form(
        &app,
        &format!("/admin/users/{needle_id}/suspend"),
        &[("csrf", &csrf)],
        Some(&admin),
    )
    .await;
    assert_eq!(s, StatusCode::SEE_OTHER, "suspend still works server-side");

    let _ = tx;
    cleanup_user_contributions(ADMIN).await;
    cleanup_user_contributions(NEEDLE).await;
    cleanup_user_contributions(OTHER).await;
}

// ---------------------------------------------------------------------------
// WP14: assets and page weight
// ---------------------------------------------------------------------------

/// Like `get`, but keeps the raw response (headers + bytes) instead of
/// decoding the body as UTF-8 — a br/gzip-compressed body isn't valid UTF-8.
/// `extra_header` lets a caller negotiate compression (`Accept-Encoding`) or
/// anything else per-request.
async fn get_raw(uri: &str, extra_header: (&str, &str)) -> (StatusCode, HeaderMap, Vec<u8>) {
    let app = test_app().await;
    let res = app
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(extra_header.0, extra_header.1)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, headers, bytes)
}

#[db_test]
async fn static_css_is_served_brotli_compressed_on_request(_tx: &mut TestTx) {
    let (status, headers, _body) = get_raw("/static/css/app.css", ("accept-encoding", "br")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("br")
    );
}

#[db_test]
async fn static_css_is_served_gzip_compressed_on_request(_tx: &mut TestTx) {
    let (status, headers, _body) =
        get_raw("/static/css/app.css", ("accept-encoding", "gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-encoding")
            .and_then(|v| v.to_str().ok()),
        Some("gzip")
    );
}

/// BREACH (CVE-2013-3587): every HTML response embeds the per-session CSRF
/// token (`<meta name="csrf">` + hidden form fields) alongside
/// attacker-influenced input (search query, `next`, error messages) —
/// compressing that combination lets an attacker recover the secret byte by
/// byte from the compressed length. `text/html` must never be compressed,
/// even though the client offers both `br` and `gzip`.
#[db_test]
async fn html_pages_are_never_compressed(_tx: &mut TestTx) {
    let (status, headers, _body) = get_raw("/", ("accept-encoding", "br, gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.get("content-encoding").is_none(),
        "HTML must not be compressed (BREACH): {headers:?}"
    );
}

#[db_test]
async fn search_page_html_is_never_compressed(_tx: &mut TestTx) {
    let (status, headers, _body) = get_raw("/search?q=x", ("accept-encoding", "br, gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        headers.get("content-encoding").is_none(),
        "the search page reflects the query and carries the CSRF token — never compress it: {headers:?}"
    );
}

/// Pulls the hashed `href` the layout rendered for `css/app.css` out of a
/// page body (`layout.asset("css/app.css")` → `/static/h/<hash>/css/app.css`).
fn extract_hashed_app_css_url(body: &str) -> String {
    let re = regex::Regex::new(r#"href="(/static/h/[0-9a-f]+/css/app\.css)""#).unwrap();
    re.captures(body)
        .unwrap_or_else(|| panic!("no hashed css/app.css href in body:\n{body}"))[1]
        .to_string()
}

#[db_test]
async fn hashed_static_url_is_cached_as_immutable(_tx: &mut TestTx) {
    let (_, home_body) = get("/").await;
    let hashed_url = extract_hashed_app_css_url(&home_body);

    let (status, headers, _) = get_raw(&hashed_url, ("accept-encoding", "identity")).await;
    assert_eq!(status, StatusCode::OK);
    let cache_control = headers
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        cache_control.contains("immutable"),
        "hashed asset must be immutably cached: {cache_control}"
    );
    assert!(
        cache_control.contains("max-age=31536000"),
        "{cache_control}"
    );
}

#[db_test]
async fn unhashed_static_url_keeps_a_short_cache_lifetime(_tx: &mut TestTx) {
    let (status, headers, _) =
        get_raw("/static/css/app.css", ("accept-encoding", "identity")).await;
    assert_eq!(status, StatusCode::OK);
    let cache_control = headers
        .get("cache-control")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    assert!(
        !cache_control.contains("immutable"),
        "the plain /static/... path must not claim immutability: {cache_control}"
    );
    assert!(cache_control.contains("max-age=3600"), "{cache_control}");
}

#[db_test]
async fn hashed_static_url_with_a_wrong_hash_is_not_found(_tx: &mut TestTx) {
    let (status, _, _) = get_raw(
        "/static/h/deadbeef00/css/app.css",
        ("accept-encoding", "identity"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[db_test]
async fn login_page_never_loads_maplibre(_tx: &mut TestTx) {
    let (status, body) = get("/login").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !body.contains("maplibre-gl"),
        "a page with no map must not load MapLibre"
    );
}

#[db_test]
async fn search_page_loads_maplibre_once_with_a_preconnect(_tx: &mut TestTx) {
    let (status, body) = get("/search?q=x").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.matches("maplibre-gl.js").count(),
        1,
        "maplibre-gl.js must load exactly once: {body}"
    );
    assert!(
        body.contains(r#"rel="preconnect""#),
        "a configured map style must get a tile-host preconnect"
    );
}

#[db_test]
async fn parking_details_page_loads_maplibre_once_with_a_preconnect(tx: &mut TestTx) {
    const MARK: &str = "fix-http-wp14-details-map";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let conn = tx.executor();
    let created = ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("WP14 Map Assets Fixture")
        .at(-25.4300, -49.2700)
        .create(&mut *conn)
        .await
        .unwrap();
    tx.commit_fixture().await;

    let (status, body) = get(&format!("/parking/{}", created.id())).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body.matches("maplibre-gl.js").count(),
        1,
        "maplibre-gl.js must load exactly once: {body}"
    );
    assert!(
        body.contains(r#"rel="preconnect""#),
        "a configured map style must get a tile-host preconnect"
    );

    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn home_hero_has_srcset_and_priority_and_featured_images_are_lazy(_tx: &mut TestTx) {
    let (status, body) = get("/").await;
    assert_eq!(status, StatusCode::OK);
    let hero_start = body.find("hero-bike-parking").expect("hero image present");
    let hero_tag = &body[body[..hero_start].rfind("<img").unwrap()..];
    let hero_tag = &hero_tag[..hero_tag.find('>').unwrap()];
    assert!(hero_tag.contains("srcset="), "hero has srcset: {hero_tag}");
    assert!(
        hero_tag.contains(r#"fetchpriority="high""#),
        "hero is high priority: {hero_tag}"
    );

    // The suffixed variant filenames (not the bare basenames, which also
    // appear in seeded-location presigned photo URLs such as
    // `seed/curitiba/mtb-pair-rack.jpg` and would match the wrong `<img>`).
    for needle in ["mtb-pair-rack-800", "cyclist-foggy-avenue-1600"] {
        let start = body
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} present"));
        let tag_start = body[..start].rfind("<img").unwrap();
        let tag = &body[tag_start..];
        let tag = &tag[..tag.find('>').unwrap()];
        assert!(
            tag.contains(r#"loading="lazy""#),
            "{needle} image is lazy-loaded: {tag}"
        );
        assert!(tag.contains("srcset="), "{needle} image has srcset: {tag}");
    }
}

/// No template may reference `/static/...css` or `/static/...js` by a literal
/// path — every css/js asset goes through `layout.asset()` so it gets a
/// content-hashed, immutably-cached URL. Nothing is exempted today; a future
/// exception would be listed here.
#[test]
fn templates_reference_css_and_js_only_through_asset() {
    const ALLOWED_LITERAL_STATIC_REFERENCES: &[&str] = &[];

    let templates_dir =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates"));
    let pattern = regex::Regex::new(r#"["']/static/[^"']*\.(?:css|js)["']"#).unwrap();
    let mut offenders = Vec::new();
    let mut stack = vec![templates_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(contents) = std::fs::read_to_string(&path) {
                for m in pattern.find_iter(&contents) {
                    let hit = format!("{}: {}", path.display(), m.as_str());
                    if !ALLOWED_LITERAL_STATIC_REFERENCES
                        .iter()
                        .any(|allowed| hit.contains(allowed))
                    {
                        offenders.push(hit);
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "css/js must be referenced through layout.asset(), not a literal /static/ path:\n{}",
        offenders.join("\n")
    );
}

// ---------------------------------------------------------------------------
// Localised transactional email
// ---------------------------------------------------------------------------

/// A registration POST with an explicit `Accept-Language`. The shared
/// [`post_form`] pins that header to `en`, and the header is exactly what is
/// under test here.
async fn register_with_language(
    app: &axum::Router,
    email: &str,
    accept_language: &str,
) -> (StatusCode, Option<String>) {
    let (cookie_line, token) = anon_csrf(app, "/register").await.expect("anon csrf");
    let body = format!(
        "email={}&display_name=&password=password123&csrf={}",
        urlencode(email),
        urlencode(&token)
    );
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/register")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("Accept-Language", accept_language)
                .header("cookie", cookie_line)
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let set_cookie = res
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    (res.status(), set_cookie)
}

async fn stored_locale(email: &str) -> String {
    sqlx::query_scalar("SELECT locale FROM users WHERE email = $1")
        .bind(email)
        .fetch_one(&pool().await)
        .await
        .unwrap()
}

/// The bug this work package fixes: a Portuguese signup used to get an English
/// "Verify your email". The subject now follows the language the form was
/// rendered in, and that language is stored on the account so later messages
/// (sent with no request in scope) keep speaking it.
#[db_test]
async fn the_verification_email_is_written_in_the_signup_language(_tx: &mut TestTx) {
    const PT: &str = "locale-pt@example.com";
    const EN: &str = "locale-en@example.com";

    let (app, mail) = auth_app().await;
    cleanup_user(PT).await;
    let (status, _) = register_with_language(&app, PT, "pt-BR,pt;q=0.9").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        mail.subject_for_kind("verify").as_deref(),
        Some("Confirme seu e-mail no BikesNest"),
        "a pt-BR signup must be greeted in Portuguese"
    );
    assert_eq!(stored_locale(PT).await, "pt-BR");
    cleanup_user(PT).await;

    // A fresh app (and outbox) for the English signup.
    let (app_en, mail_en) = auth_app().await;
    cleanup_user(EN).await;
    let (status, _) = register_with_language(&app_en, EN, "en-GB,en;q=0.8").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(
        mail_en.subject_for_kind("verify").as_deref(),
        Some("Confirm your BikesNest email")
    );
    assert_eq!(stored_locale(EN).await, "en");
    cleanup_user(EN).await;
}

/// `GET /lang/{code}` sets the cookie for everyone; for a signed-in user it
/// also writes `users.locale`, which is the only thing a background job can
/// read. The mail that follows switches language with it.
#[db_test]
async fn the_language_toggle_persists_for_a_signed_in_user(_tx: &mut TestTx) {
    const EMAIL: &str = "locale-toggle@example.com";
    let (app, mail) = auth_app().await;
    cleanup_user(EMAIL).await;

    let (status, _) = register_with_language(&app, EMAIL, "pt-BR").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert_eq!(stored_locale(EMAIL).await, "pt-BR");

    let (_, _, cookie) = post_form(
        &app,
        "/login",
        &[("email", EMAIL), ("password", "password123")],
        None,
    )
    .await;
    let session = cookie.expect("session cookie");

    // Anonymous first: the cookie switches, no account is touched.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/lang/en")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert_eq!(
        stored_locale(EMAIL).await,
        "pt-BR",
        "an anonymous toggle must not touch anyone's account"
    );

    // Signed in: the choice is persisted.
    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/lang/en")
                .header("cookie", &session)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SEE_OTHER);
    assert!(
        res.headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .contains("lang=en"),
        "the toggle still sets the cookie"
    );
    assert_eq!(stored_locale(EMAIL).await, "en");

    // The next message — a resend, with no page rendered for it — follows.
    let (s, _, _) = post_form(&app, "/verify-email/resend", &[("email", EMAIL)], None).await;
    assert_eq!(s, StatusCode::SEE_OTHER);
    let subjects: Vec<String> = mail.emails().into_iter().map(|e| e.subject).collect();
    assert_eq!(
        subjects,
        vec![
            "Confirme seu e-mail no BikesNest".to_string(),
            "Confirm your BikesNest email".to_string()
        ],
        "the signup mail stays Portuguese; the one after the toggle is English"
    );

    cleanup_user(EMAIL).await;
}

/// With the worker enabled (production's default) nothing is sent on the
/// request path: registration writes an `email.send` job and returns. Running
/// the handler over that row — what the worker does — produces the localised
/// message.
#[db_test]
async fn with_the_worker_enabled_registration_queues_the_email(_tx: &mut TestTx) {
    use bikesnest_infrastructure::{JobConfig, SendEmailHandler};

    const EMAIL: &str = "locale-queued@example.com";
    let mail = FakeEmailProvider::with_root(None);
    let db = Db::from_pool(pool().await);
    let config = bikesnest_infrastructure::Config {
        jobs: JobConfig {
            enabled: true,
            ..JobConfig::default()
        },
        ..test_config()
    };
    let app = app_router_with(
        std::sync::Arc::new(config),
        db,
        RouterDeps {
            email: std::sync::Arc::new(mail.clone()),
            oauth: None,
            hasher: TestPasswordHasher,
            rate_limiter: Box::new(bikesnest_infrastructure::InMemoryRateLimiter::new()),
            storage: std::sync::Arc::new(bikesnest_test_support::TestObjectStorage::new()),
        },
    );
    cleanup_user(EMAIL).await;

    let (status, _) = register_with_language(&app, EMAIL, "pt-BR").await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    assert!(
        mail.emails().is_empty(),
        "the provider must not be touched on the request path"
    );

    let (job_id, payload): (i64, serde_json::Value) = sqlx::query_as(
        "SELECT id, payload FROM background_job
         WHERE kind = 'email.send' AND payload->>'to' = $1",
    )
    .bind(EMAIL)
    .fetch_one(&pool().await)
    .await
    .expect("registration queued an email.send job");
    assert_eq!(payload["locale"], "pt-BR");
    assert_eq!(payload["kind"], "verify_email");

    // Drain it the way the worker would.
    bikesnest_application::JobHandler::run(
        &SendEmailHandler::new(std::sync::Arc::new(mail.clone())),
        &payload,
    )
    .await
    .expect("the handler delivers the queued message");
    assert_eq!(
        mail.subject_for_kind("verify").as_deref(),
        Some("Confirme seu e-mail no BikesNest")
    );

    sqlx::query("DELETE FROM background_job WHERE id = $1")
        .bind(job_id)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user(EMAIL).await;
}

// ---------------------------------------------------------------------------
// WP17: the per-IP geocode budget on /search
// ---------------------------------------------------------------------------

/// A router whose geocode budget is `per_ip` cache-missing searches. The
/// limiter is in-memory and lives in this router's state, so every request
/// made through this instance shares one bucket (`ClientIp` resolves to a
/// single test peer when there is no `ConnectInfo`).
async fn app_with_geocode_budget(per_ip: u32) -> axum::Router {
    let config = bikesnest_infrastructure::Config {
        geocode: bikesnest_infrastructure::GeocodeLimits {
            per_ip,
            window: std::time::Duration::from_secs(900),
        },
        ..test_config()
    };
    bikesnest_web::app_router(std::sync::Arc::new(config), Db::from_pool(pool().await))
        .expect("test config builds every provider")
}

/// [`app_with_geocode_budget`] plus the captured mail, so a test can register
/// and verify a session against the same limiter bucket.
async fn auth_app_with_geocode_budget(per_ip: u32) -> (axum::Router, FakeEmailProvider) {
    let email = FakeEmailProvider::with_root(None);
    let config = bikesnest_infrastructure::Config {
        geocode: bikesnest_infrastructure::GeocodeLimits {
            per_ip,
            window: std::time::Duration::from_secs(900),
        },
        ..test_config()
    };
    let deps = RouterDeps {
        email: std::sync::Arc::new(email.clone()),
        oauth: None,
        hasher: TestPasswordHasher,
        rate_limiter: Box::new(bikesnest_infrastructure::InMemoryRateLimiter::new()),
        storage: std::sync::Arc::new(bikesnest_test_support::TestObjectStorage::new()),
    };
    (
        app_router_with(
            std::sync::Arc::new(config),
            Db::from_pool(pool().await),
            deps,
        ),
        email,
    )
}

/// Resolving a free-text destination is a billable third-party call, so one
/// network cannot spend an unbounded number of them — but a destination the
/// in-process cache can already answer costs the provider nothing and must
/// therefore cost the caller nothing either.
#[db_test]
async fn free_text_searches_are_metered_but_cached_ones_are_free(_tx: &mut TestTx) {
    let app = app_with_geocode_budget(2).await;

    // Two fresh destinations: two provider calls, both within budget.
    let (status, body) = get_c(&app, "/search?q=alpha+avenue", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains("Too many searches"));
    let (status, _) = get_c(&app, "/search?q=beta+boulevard", None).await;
    assert_eq!(status, StatusCode::OK);

    // A third one would be a third call: refused, with the notice, not a 500
    // and not an empty page pretending nothing matched.
    let (status, body) = get_c(&app, "/search?q=gamma+gardens", None).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        body.contains("Too many searches from your network right now"),
        "the 429 explains itself: {body}"
    );

    // The first destination is cached now, so asking again reaches no
    // provider — and is not charged against the exhausted budget.
    let (status, body) = get_c(&app, "/search?q=alpha+avenue", None).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a cache hit does not consume the budget: {body}"
    );
    assert!(!body.contains("Too many searches"));

    // Coordinates never reach the geocoder at all, so they are never
    // metered either — including after the budget is spent.
    let (status, _) = get_c(&app, "/search?lat=-25.4284&lon=-49.2733", None).await;
    assert_eq!(status, StatusCode::OK);
}

/// The budget is spent per geocode, not per request: a search that carries no
/// destination at all resolves nothing and must not be charged.
#[db_test]
async fn a_search_without_a_destination_is_not_metered(_tx: &mut TestTx) {
    let app = app_with_geocode_budget(1).await;

    for _ in 0..3 {
        let (status, _) = get_c(&app, "/search", None).await;
        assert_eq!(status, StatusCode::OK);
    }
    // The one geocode in the budget is still there to spend.
    let (status, _) = get_c(&app, "/search?q=delta+drive", None).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// WP19: contributing from a phone — the map picker's no-JS fields, the hours
// and tri-state security editors, the pre-create duplicate interstitial, and
// the address→coordinates endpoint behind the picker.
// ---------------------------------------------------------------------------

/// The add form must ask for a place, not for GIS data: no required decimal
/// coordinates in the main flow, no visible timezone field, a real hours
/// editor and three-way security controls.
#[db_test]
async fn add_form_asks_for_a_place_not_for_coordinates(tx: &mut TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp19-form@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (status, body) = get_c(&app, "/parking/new", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);

    // The picker's container, and the coordinates demoted into <details>.
    assert!(
        body.contains(r#"id="pin-map""#),
        "the map picker is rendered"
    );
    assert!(
        body.contains("<details") && body.contains("Advanced"),
        "the coordinates live behind an Advanced disclosure: {body}"
    );
    assert!(
        body.contains(r#"id="lat" name="lat""#) && body.contains(r#"id="lon" name="lon""#),
        "…but they are still real inputs, so the no-JS path can post them"
    );
    assert!(
        !body.contains(r#"name="lat" type="number" step="any" value="" required"#),
        "coordinates are not a required field of the main flow"
    );
    let tz_at = body
        .find(r#"name="timezone""#)
        .expect("timezone input exists");
    let details_at = body.find("<details").expect("an Advanced block exists");
    assert!(
        tz_at > details_at,
        "the timezone field is inside Advanced, not in the default flow"
    );

    // The hours editor: seven day rows, each with the four states.
    for day in ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] {
        assert!(
            body.contains(&format!(r#"name="h_{day}_state""#)),
            "hours row for {day}"
        );
        assert!(
            body.contains(&format!(r#"name="h_{day}_1_open""#)),
            "first range for {day}"
        );
    }
    assert!(body.contains(r#"value="all_day""#) && body.contains(r#"value="ranges""#));

    // Security: eight groups × three options, with real `name`s (the old
    // checkboxes had none, so scripting off dropped every selection).
    for code in [
        "dedicated_locking_point",
        "indoor",
        "cctv",
        "staffed",
        "security_guard",
        "controlled_access",
        "well_lit",
        "restricted_access",
    ] {
        for state in ["yes", "no", "unknown"] {
            assert!(
                body.contains(&format!(r#"name="sec_{code}" value="{state}""#)),
                "security radio sec_{code}={state}"
            );
        }
    }
    assert!(
        !body.contains(r#"name="security""#),
        "the hidden comma-separated mirror is gone"
    );

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

/// The whole no-JS submission: the fields a browser with no scripting can
/// produce, and nothing else. A definitive "no" reaches the details page as
/// the ✗ marker, and a weekly overnight range reaches its hours table.
#[db_test]
async fn a_script_free_submission_records_hours_and_a_definitive_no(tx: &mut TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp19-nojs@example.com";
    const MARK: &str = "wp19-nojs";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    let (status, _, _) = post_form(
        &app,
        "/parking/new",
        &[
            ("csrf", &csrf),
            ("name", "Overnight Rack"),
            ("address", "Rua Y, 9"),
            ("parking_type", "rack"),
            ("cost_kind", "unknown"),
            // Typed into the Advanced disclosure: no map, no geolocation.
            ("lat", "-23.5"),
            ("lon", "-46.7"),
            ("h_mon_state", "ranges"),
            ("h_mon_1_open", "22:00"),
            ("h_mon_1_close", "02:00"),
            ("h_tue_state", "closed"),
            ("sec_cctv", "no"),
            ("sec_well_lit", "yes"),
            ("confirm", "1"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "the no-JS field set is a complete submission"
    );

    let (id,): (i64,) = sqlx::query_as(
        "SELECT id FROM parking_location WHERE name = 'Overnight Rack' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&pool().await)
    .await
    .unwrap();

    // No timezone was posted; the service derived one from the point.
    let (timezone,): (String,) =
        sqlx::query_as("SELECT timezone FROM parking_location WHERE id = $1")
            .bind(id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert!(
        !timezone.is_empty(),
        "the timezone is derived from the pin, not typed"
    );

    // The overnight range is stored on Monday only; Tuesday has no row.
    let rows: Vec<(i16, chrono::NaiveTime, chrono::NaiveTime, bool)> = sqlx::query_as(
        "SELECT day_of_week, opens_at, closes_at, all_day FROM opening_hours
         WHERE location_id = $1 ORDER BY day_of_week",
    )
    .bind(id)
    .fetch_all(&pool().await)
    .await
    .unwrap();
    assert_eq!(rows.len(), 1, "one range, on one day: {rows:?}");
    assert_eq!(rows[0].0, 1, "Monday");
    assert_eq!(
        rows[0].1,
        chrono::NaiveTime::from_hms_opt(22, 0, 0).unwrap()
    );
    assert_eq!(rows[0].2, chrono::NaiveTime::from_hms_opt(2, 0, 0).unwrap());
    assert!(!rows[0].3);

    // CCTV recorded as a definitive NO (state 2), not as unknown.
    let (cctv,): (i16,) = sqlx::query_as(
        "SELECT state FROM parking_security WHERE location_id = $1 AND feature_code = 'cctv'",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(cctv, 2, "\"no\" is recordable");

    // …and the details page's ✗ marker, previously unreachable, renders.
    let (status, body) = get_c(&app, &format!("/parking/{id}"), Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains(r#"aria-label="No">✗</span><span class="text-muted">CCTV"#),
        "the details page marks CCTV as absent: {body}"
    );
    assert!(
        body.contains("22:00 – 02:00"),
        "the hours table shows the overnight range"
    );

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

/// A day whose two ranges overlap is refused with the message next to that
/// day, and nothing is created.
#[db_test]
async fn overlapping_hours_are_refused_with_a_field_level_message(tx: &mut TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp19-overlap@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    let (status, body, _) = post_form(
        &app,
        "/parking/new",
        &[
            ("csrf", &csrf),
            ("name", "Overlap Spot"),
            ("address", "Rua Y, 9"),
            ("parking_type", "rack"),
            ("cost_kind", "unknown"),
            ("lat", "-23.51"),
            ("lon", "-46.71"),
            ("h_wed_state", "ranges"),
            ("h_wed_1_open", "09:00"),
            ("h_wed_1_close", "18:00"),
            ("h_wed_2_open", "17:00"),
            ("h_wed_2_close", "20:00"),
            ("confirm", "1"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.contains("The two ranges for this day overlap."),
        "the overlap is named: {body}"
    );
    assert!(
        body.contains(r#"value="ranges" selected"#),
        "the rejected form comes back with what was typed"
    );
    let (count,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM parking_location WHERE name = 'Overlap Spot'")
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(count, 0, "a rejected form creates nothing");

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

/// Duplicate detection runs BEFORE the insert: a near-identical spot 15 m away
/// gets the interstitial and no row; confirming creates it and lands on the
/// details page with the "what happens next" notice.
#[db_test]
async fn a_near_duplicate_is_confirmed_before_anything_is_created(tx: &mut TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp19-dupe@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    // The spot that already exists.
    let existing = add_location(&app, &cookie, &csrf, "Bicicletario Praca Central", &[]).await;

    // ~15 m north of it, same name. `add_location` posts -23.4/-46.6.
    let near_lat = (-23.4_f64 + 0.000_135).to_string();
    let submission: Vec<(&str, &str)> = vec![
        ("csrf", &csrf),
        ("name", "Bicicletario Praca Central"),
        ("address", "Rua X, 1"),
        ("parking_type", "rack"),
        ("cost_kind", "unknown"),
        ("lat", &near_lat),
        ("lon", "-46.6"),
    ];
    let (status, body, _) = post_form(&app, "/parking/new", &submission, Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "the interstitial, not a redirect");
    assert!(
        body.contains("Is this the same spot?"),
        "the interstitial asks first: {body}"
    );
    assert!(
        body.contains(&format!(r#"href="/parking/{existing}""#)),
        "the candidate is linked so it can be checked"
    );
    assert!(
        body.contains(r#"name="confirm" value="1""#),
        "…and offers a way through"
    );
    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM parking_location WHERE name = 'Bicicletario Praca Central'",
    )
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(count, 1, "nothing was created while the question stood");

    // Confirming creates it and lands on the notice.
    let mut confirmed = submission.clone();
    confirmed.push(("confirm", "1"));
    let (status, _, _) = post_form(&app, "/parking/new", &confirmed, Some(&cookie)).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let (created,): (i64,) = sqlx::query_as(
        "SELECT id FROM parking_location WHERE name = 'Bicicletario Praca Central'
         ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_ne!(created, existing, "a second spot now exists");
    let (status, body) = get_c(
        &app,
        &format!("/parking/{created}?created=1"),
        Some(&cookie),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("Your spot is live. The community will verify it over time"),
        "the details page says what happens next: {body}"
    );

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

/// A submission nowhere near an existing spot skips the interstitial entirely.
#[db_test]
async fn a_lone_spot_is_created_without_an_interstitial(tx: &mut TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp19-lone@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    // Far from the seed data and from anything the other tests create.
    let (status, _, _) = post_form(
        &app,
        "/parking/new",
        &[
            ("csrf", &csrf),
            ("name", "Deserted Wharf Racks"),
            ("address", "Cais Norte, 400"),
            ("parking_type", "rack"),
            ("cost_kind", "unknown"),
            ("lat", "-30.0331"),
            ("lon", "-51.23"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::SEE_OTHER,
        "no candidates, so no question to ask"
    );

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

/// `GET /api/geocode` is the picker's address lookup: verified users only, and
/// metered on the same per-IP budget as `/search` because it reaches the same
/// billable provider.
#[db_test]
async fn the_geocode_endpoint_is_gated_and_metered(tx: &mut TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp19-geocode@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;

    let (status, body) = get_c(&app, "/api/geocode?q=Rua+XV+de+Novembro", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let hit: serde_json::Value = serde_json::from_str(&body).expect("JSON: {body}");
    assert!(hit["lat"].as_f64().is_some(), "a latitude: {body}");
    assert!(hit["lon"].as_f64().is_some(), "a longitude: {body}");
    assert!(hit["label"].as_str().is_some(), "and a label: {body}");

    // Anonymous callers are sent to sign in, never answered.
    let (status, _) = get_c(&app, "/api/geocode?q=Rua+XV+de+Novembro", None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);

    // An unverified session cannot spend the budget either.
    let unverified = unverified_cookie(&app, "wp19-unverified@example.com").await;
    let (status, _) = get_c(&app, "/api/geocode?q=Avenida+Nova", Some(&unverified)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let _ = tx;
    cleanup_user(EMAIL).await;
    cleanup_user("wp19-unverified@example.com").await;
}

/// The budget is the same bucket `/search` spends from, and a cached address
/// costs nothing.
#[db_test]
async fn the_geocode_endpoint_refuses_over_budget(tx: &mut TestTx) {
    let (app, email) = auth_app_with_geocode_budget(1).await;
    const EMAIL: &str = "wp19-budget@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;

    let (status, _) = get_c(&app, "/api/geocode?q=Rua+Primeira", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "the one geocode in the budget");

    let (status, body) = get_c(&app, "/api/geocode?q=Rua+Segunda", Some(&cookie)).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body, "{}", "an empty object, not a page");

    // The first address is cached now: free, so still answered.
    let (status, _) = get_c(&app, "/api/geocode?q=Rua+Primeira", Some(&cookie)).await;
    assert_eq!(status, StatusCode::OK, "a cache hit is not charged");

    let _ = tx;
    cleanup_user(EMAIL).await;
}

// ---------------------------------------------------------------------------
// WP21: accessibility pass
// ---------------------------------------------------------------------------

/// The full `<tag ...>` opening tag containing `id="{id}"` — a substring
/// match on the whole tag rather than a literal attribute-order string, so a
/// harmless reordering of attributes does not break the assertion.
fn opening_tag_with_id<'a>(body: &'a str, id: &str) -> &'a str {
    let needle = format!(r#"id="{id}""#);
    let at = body
        .find(&needle)
        .unwrap_or_else(|| panic!("id=\"{id}\" not found in body"));
    let start = body[..at].rfind('<').expect("a tag opens before the id");
    let end = at + body[at..].find('>').expect("the tag closes") + 1;
    &body[start..end]
}

/// The full opening tag containing `marker` anywhere in its attributes — same
/// substring-of-the-tag technique as [`opening_tag_with_id`], keyed on an
/// arbitrary marker instead of an `id=` value (the login error banner has no
/// id of its own).
fn opening_tag_containing<'a>(body: &'a str, marker: &str) -> &'a str {
    let at = body
        .find(marker)
        .unwrap_or_else(|| panic!("{marker:?} not found in body"));
    let start = body[..at]
        .rfind('<')
        .expect("a tag opens before the marker");
    let end = at + body[at..].find('>').expect("the tag closes") + 1;
    &body[start..end]
}

#[db_test]
async fn login_wrong_password_banner_is_an_alert(tx: &mut bikesnest_test_support::TestTx) {
    let (app, _) = auth_app().await;
    const EMAIL: &str = "wp21-login-alert@example.com";
    cleanup_user(EMAIL).await;
    post_form(
        &app,
        "/register",
        &[("email", EMAIL), ("password", "password123")],
        None,
    )
    .await;

    let (s, login_page) = get_c(&app, "/login", None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        !login_page.contains("bg-danger/10"),
        "no error banner before any attempt: {login_page}"
    );

    let (s, body, _) = post_form(
        &app,
        "/login",
        &[("email", EMAIL), ("password", "wrong")],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    // Pinned to the banner element itself (the `bg-danger/10` div), not just
    // `role="alert"` anywhere on the page — a field-level `<p role="alert">`
    // would otherwise satisfy a looser assertion even if the banner itself
    // lost its role.
    let banner = opening_tag_containing(&body, "bg-danger/10");
    assert!(
        banner.contains(r#"role="alert""#),
        "the login error banner must itself carry role=\"alert\": {banner}"
    );
    // The generic message never says which of the two was wrong, but a
    // failure still flags both inputs (WP21 a11y pass) rather than neither.
    assert!(
        body.contains(r#"aria-describedby="email-error""#)
            && body.contains(r#"aria-describedby="password-error""#),
        "{body}"
    );

    let _ = tx;
    cleanup_user(EMAIL).await;
}

/// `AuthError::EmailTaken` is defined but `AuthService::register` never
/// returns it: a taken email is deliberately answered exactly like a fresh
/// one (`Ok(())`, no mail sent) so the response cannot be used to enumerate
/// registered addresses ( — see the comment in
/// `crates/application/src/auth.rs`'s `register`). `register_field_error`'s
/// `EmailTaken => Some("email")` arm therefore has no live producer through
/// this form; the same field association is exercised here through
/// `AuthError::InvalidEmail`, which *does* reach the error branch (a
/// malformed address fails `UserEmail::parse` before any lookup).
#[db_test]
async fn register_with_an_invalid_email_flags_the_email_field(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, _email) = auth_app().await;
    let (s, body, _) = post_form(
        &app,
        "/register",
        &[("email", "not-an-email"), ("password", "password123")],
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "the invalid-email re-render: {body}");
    assert!(body.contains(r#"role="alert""#), "the banner too: {body}");
    let email_input = opening_tag_with_id(&body, "email");
    assert!(
        email_input.contains(r#"aria-invalid="true""#),
        "{email_input}"
    );
    assert!(
        email_input.contains(r#"aria-describedby="email-error""#),
        "{email_input}"
    );
    assert!(body.contains(r#"<p id="email-error""#), "{body}");

    let _ = tx;
}

#[db_test]
async fn parking_new_bad_currency_code_flags_the_price_field(
    tx: &mut bikesnest_test_support::TestTx,
) {
    // `cost_from_form` never rejects an unparsable numeric amount — a price
    // that fails to parse just falls back to "paid, amount unknown" — so the
    // one way to make the price group of fields fail server-side validation
    // today is a currency/unit code `CurrencyCode::parse`/`PricingUnit::from_code`
    // refuses, which is exactly what a stray non-ISO currency value is.
    let (app, email) = auth_app().await;
    const EMAIL_ADDR: &str = "wp21-price@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL_ADDR).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);

    let (s, body, _) = post_form(
        &app,
        "/parking/new",
        &[
            ("csrf", &csrf),
            ("name", "Bad Price Spot"),
            ("address", "Rua X, 1"),
            ("parking_type", "rack"),
            ("cost_kind", "paid"),
            ("price", "10"),
            ("price_currency", "X"),
            ("price_unit", "hour"),
            ("lat", "-23.4"),
            ("lon", "-46.6"),
            ("timezone", "America/Sao_Paulo"),
            ("confirm", "1"),
        ],
        Some(&cookie),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::BAD_REQUEST,
        "the invalid currency code is rejected: {body}"
    );
    let price_input = opening_tag_with_id(&body, "price");
    assert!(
        price_input.contains(r#"aria-invalid="true""#),
        "{price_input}"
    );
    assert!(
        price_input.contains(r#"aria-describedby="price-error""#),
        "{price_input}"
    );
    assert!(body.contains(r#"<p id="price-error""#), "{body}");

    let _ = tx;
    cleanup_user_contributions(EMAIL_ADDR).await;
}

#[db_test]
async fn review_with_an_empty_body_flags_the_body_field(tx: &mut bikesnest_test_support::TestTx) {
    let (app, email) = auth_app().await;
    const EMAIL: &str = "wp21-review-body@example.com";
    let cookie = verified_cookie(&app, &email, EMAIL).await;
    let (_, form) = get_c(&app, "/parking/new", Some(&cookie)).await;
    let csrf = extract_csrf(&form);
    let id = add_location(&app, &cookie, &csrf, "WP21 Review Spot", &[]).await;

    let (_, review_form) = get_c(&app, &format!("/parking/{id}/review"), Some(&cookie)).await;
    let rcsrf = extract_csrf(&review_form);
    let rbody = multipart_review("4", "", "----bikesnestphoto");
    let (s, body) = post_multipart(
        &app,
        &format!("/parking/{id}/review"),
        rbody,
        &cookie,
        Some(&rcsrf),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "the empty body is rejected: {body}");
    let body_field = opening_tag_with_id(&body, "body");
    assert!(
        body_field.contains(r#"aria-invalid="true""#),
        "{body_field}"
    );
    assert!(
        body_field.contains(r#"aria-describedby="body-error""#),
        "{body_field}"
    );
    assert!(body.contains(r#"<p id="body-error""#), "{body}");

    let _ = tx;
    cleanup_user_contributions(EMAIL).await;
}

#[db_test]
async fn parking_details_dialogs_and_swap_targets_are_accessible(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let (app, email) = auth_app().await;
    let loc = fixture_location(tx, "wp21-dialogs", "WP21 Dialogs Spot").await;
    const UPLOADER: &str = "wp21-dialogs-up@example.com";
    const MODERATOR: &str = "wp21-dialogs-mod@example.com";
    let uploader = verified_cookie(&app, &email, UPLOADER).await;

    // A photo, approved, so the gallery — and the lightbox it feeds — renders.
    let (_, page) = get_c(&app, &format!("/parking/{loc}"), Some(&uploader)).await;
    let csrf = extract_csrf(&page);
    let (_, mbody) = multipart_upload(&tiny_jpeg(), None, "----bikesnestphoto");
    post_multipart(
        &app,
        &format!("/parking/{loc}/photo"),
        mbody,
        &uploader,
        Some(&csrf),
    )
    .await;
    let (photo_id,): (i64,) = sqlx::query_as("SELECT id FROM parking_photo WHERE location_id = $1")
        .bind(loc)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    let mod_cookie = moderator_cookie(&app, &email, MODERATOR).await;
    let (_, queue) = get_c(&app, "/moderation/photos", Some(&mod_cookie)).await;
    let mcs = extract_csrf(&queue);
    post_form_hx(
        &app,
        &format!("/moderation/photos/parking/{photo_id}/approve"),
        &[("csrf", &mcs)],
        Some(&mod_cookie),
    )
    .await;

    let (s, body) = get_c(&app, &format!("/parking/{loc}"), Some(&uploader)).await;
    assert_eq!(s, StatusCode::OK);

    // Report modal: role/aria-modal/aria-labelledby, and the target exists.
    let report_modal = opening_tag_with_id(&body, "report-modal");
    assert!(report_modal.contains(r#"role="dialog""#), "{report_modal}");
    assert!(
        report_modal.contains(r#"aria-modal="true""#),
        "{report_modal}"
    );
    assert!(
        report_modal.contains(r#"aria-labelledby="report-modal-title""#),
        "{report_modal}"
    );
    assert!(
        body.contains(r#"id="report-modal-title""#),
        "the labelledby target exists: {body}"
    );

    // Lightbox — only rendered because the gallery is non-empty.
    let lightbox = opening_tag_with_id(&body, "photo-lightbox");
    assert!(lightbox.contains(r#"role="dialog""#), "{lightbox}");
    assert!(lightbox.contains(r#"aria-modal="true""#), "{lightbox}");
    assert!(
        lightbox.contains(r#"aria-labelledby="photo-lightbox-title""#),
        "{lightbox}"
    );
    assert!(
        body.contains(r#"id="photo-lightbox-title""#),
        "the labelledby target exists: {body}"
    );

    // Swap targets: aria-live + a focus anchor.
    for id in [
        "verification-panel",
        "photo-upload-result",
        "report-modal-feedback",
    ] {
        let tag = opening_tag_with_id(&body, id);
        assert!(tag.contains(r#"aria-live="polite""#), "{id}: {tag}");
        assert!(tag.contains(r#"tabindex="-1""#), "{id}: {tag}");
    }

    let _ = tx;
    cleanup_user_contributions(UPLOADER).await;
    cleanup_user_contributions(MODERATOR).await;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = 'wp21-dialogs'")
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn search_results_list_has_no_script_child_and_listitems_are_direct_children(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const MARK: &str = "wp21-search-list";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    // The FakeGeocoder resolves "Rua XV de Novembro" to exactly this point
    // (crates/infrastructure/src/geocoding.rs), so a fixture placed there is
    // guaranteed to be in range of the query the task names.
    ParkingBuilder::new()
        .with_fixture_tag(MARK)
        .with_name("WP21 List Structure Rack")
        .at(-25.4284, -49.2733)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;

    let (status, body) = get("/search?q=Rua+XV+de+Novembro").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.contains("WP21 List Structure Rack"),
        "the seeded fixture is in the results: {body}"
    );

    // `#search-data` lives outside `#results`: exactly one copy on a plain
    // page load, and it renders before `#results`, not inside it.
    assert_eq!(
        body.matches(r#"id="search-data""#).count(),
        1,
        "one copy on a non-htmx page load: {body}"
    );
    let results_at = body
        .find(r#"<div id="results""#)
        .expect("the results container");
    let script_at = body
        .find(r#"<script type="application/json" id="search-data""#)
        .expect("the search-data script");
    assert!(
        script_at < results_at,
        "search-data must render before #results, not inside it"
    );

    // The first element inside `#results` must be a listitem, not a wrapper
    // div — nothing but whitespace stands between the list's own opening tag
    // and its first child.
    let after_results = &body[results_at..];
    let open_end = after_results.find('>').unwrap() + 1;
    let after_open = &after_results[open_end..];
    let next_tag_at = after_open.find('<').expect("a child tag follows");
    let head = &after_open[next_tag_at..(next_tag_at + 80).min(after_open.len())];
    // The tag's own attributes are one per line (see
    // components/parking_card.html), so this checks the tag name and the
    // presence of `role="listitem"` rather than an exact single-line prefix.
    assert!(
        head.starts_with("<article") && head.contains(r#"role="listitem""#),
        "the first child of #results must be a listitem: {head}"
    );

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
}

// --- Template/static-asset hygiene (pure filesystem scan, no DB) ------------

/// No `focus:outline-none` may remain in a template: it strips the *keyboard*
/// focus ring along with the mouse one, defeating the global `:focus-visible`
/// rule input.css now carries (WP21 a11y pass).
#[test]
fn no_focus_outline_none_remains_in_templates() {
    let templates_dir =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates"));
    let mut offenders = Vec::new();
    let mut stack = vec![templates_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(contents) = std::fs::read_to_string(&path)
                && contents.contains("focus:outline-none")
            {
                offenders.push(path.display().to_string());
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "focus:outline-none remains in:\n{}",
        offenders.join("\n")
    );
}

/// No cramped `<button>` (`text-xs` + `py-0.5`/exactly `py-1`) may remain: at
/// that padding a button's tap target falls under the 24×24 CSS px minimum
/// (WCAG 2.5.8). `.btn-compact` (input.css) is the fix; every real offender
/// found during the WP21 audit now carries it.
#[test]
fn no_tiny_text_xs_buttons_remain_in_templates() {
    let templates_dir =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates"));
    let button = regex::Regex::new(r"(?s)<button\b[^>]*>").unwrap();
    let tiny_py =
        regex::Regex::new(r#"(?:^|[\s"])py-0\.5(?:[\s"]|$)|(?:^|[\s"])py-1(?:[\s"]|$)"#).unwrap();
    let mut offenders = Vec::new();
    let mut stack = vec![templates_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            for m in button.find_iter(&contents) {
                let tag = m.as_str();
                if tag.contains("text-xs") && tiny_py.is_match(tag) {
                    offenders.push(format!("{}: {tag}", path.display()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "cramped text-xs buttons remain:\n{}",
        offenders.join("\n")
    );
}

/// Every `<button>` must have an accessible name: either visible text content
/// or an `aria-label` (a bound `:aria-label` counts too — it always ships
/// alongside a matching static `aria-label` fallback in this codebase, see
/// templates/pages/admin_users.html's reveal-email toggle). A button whose
/// only content is an `<img alt="…">` also passes: the image's alt text
/// becomes the button's accessible name.
///
/// Heuristic, not a real DOM/accessible-name computation — false positives go
/// in `ALLOWED` with a comment, not a code change. Empty today: the WP21
/// audit found and fixed every real gap.
#[test]
fn every_button_has_visible_text_or_an_aria_label() {
    // (path substring, snippet substring) — none needed yet: the WP21 audit
    // found and fixed every real gap.
    const ALLOWED: &[(&str, &str)] = &[];
    let templates_dir =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates"));
    let button = regex::Regex::new(r"(?s)<button\b([^>]*)>(.*?)</button>").unwrap();
    let tag_strip = regex::Regex::new(r"(?s)<[^>]+>").unwrap();
    let mut offenders = Vec::new();
    let mut stack = vec![templates_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            for cap in button.captures_iter(&contents) {
                let attrs = &cap[1];
                let inner = &cap[2];
                if attrs.contains("aria-label") {
                    continue;
                }
                let text = tag_strip.replace_all(inner, "");
                if !text.trim().is_empty() {
                    continue;
                }
                if inner.contains("x-text") || attrs.contains("x-text") {
                    continue;
                }
                if inner.contains("alt=\"") && !inner.contains("alt=\"\"") {
                    continue;
                }
                let snippet = format!("{attrs} ... {inner}");
                if ALLOWED
                    .iter()
                    .any(|(p, s)| path.display().to_string().contains(p) && snippet.contains(s))
                {
                    continue;
                }
                offenders.push(format!("{}: {snippet}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "buttons without an accessible name:\n{}",
        offenders.join("\n")
    );
}

/// JS static check: the shared focus-trap helper and the after-swap focus
/// listener both live in app.js (WP21 a11y pass), and search.js still finds
/// `#search-data` by id regardless of where in the DOM it now renders.
#[test]
fn app_js_has_the_focus_trap_and_after_swap_focus_listener() {
    let app_js = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../web/static/js/app.js"
    ));
    let js = std::fs::read_to_string(app_js).expect("read app.js");
    assert!(js.contains("FocusTrap"), "the shared helper: {js}");
    assert!(js.contains("trapTab"), "Tab/Shift+Tab cycling: {js}");
    assert!(
        js.contains("inertBackground") || js.contains(".inert"),
        "background content is inerted while a dialog is open"
    );
    assert!(
        js.contains("htmx:after:swap"),
        "the after-swap focus listener"
    );
    assert!(
        js.contains("verification-panel"),
        "it targets the verification panel specifically: {js}"
    );

    let search_js = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../web/static/js/search.js"
    ));
    let js = std::fs::read_to_string(search_js).expect("read search.js");
    assert!(
        js.contains(r#"document.getElementById("search-data")"#),
        "readData() must query by id from `document`, not a page-specific root: {js}"
    );
}

/// Generalises [`no_error_colour_classes_remain_in_templates`] (which only
/// ever covered the one renamed `-error` token) to every Tailwind
/// colour-utility prefix: any `text|bg|border|...-<name>` utility in a
/// template must name a token this app actually defines (`--color-<name>` in
/// `web/static/css/input.css`'s `@theme` block), one of Tailwind's own
/// colour keywords (`white`/`black`/`transparent`/`current`/`inherit`), or a
/// value on the small, commented allowlist of same-prefix utilities that are
/// not colours at all (`text-sm`, `border-t`, `outline-none`, …). A colour
/// renamed or dropped from `@theme` without updating every template now fails
/// here instead of silently rendering as dead CSS.
#[test]
fn no_undefined_tailwind_color_tokens_remain_in_templates() {
    let css_path = std::path::Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../web/static/css/input.css"
    ));
    let css = std::fs::read_to_string(css_path).expect("read input.css");
    let theme_start = css.find("@theme").expect("input.css must declare @theme");
    let theme_open = theme_start
        + css[theme_start..]
            .find('{')
            .expect("@theme block opens with {");
    let theme_close = css[theme_open..]
        .find("\n}")
        .map(|i| theme_open + i)
        .unwrap_or(css.len());
    let theme_block = &css[theme_open..theme_close];

    let token_re = regex::Regex::new(r"--color-([a-z][a-z0-9-]*)\s*:").unwrap();
    let tokens: std::collections::HashSet<&str> = token_re
        .captures_iter(theme_block)
        .map(|c| c.get(1).unwrap().as_str())
        .collect();
    assert!(
        !tokens.is_empty(),
        "must find at least one --color-* token in input.css's @theme block"
    );

    // Tailwind keywords usable on any colour utility without being one of
    // this app's own tokens. The numeric default palette (`red-500`,
    // `slate-100`, …) is not in this allowlist because grepping the
    // templates tree shows it is never used here — a real `text-red-500`
    // sneaking in should fail this test, not be silently allowed.
    const BUILTIN_COLOR_WORDS: &[&str] = &["white", "black", "transparent", "current", "inherit"];

    // Same-prefix Tailwind utilities that are not a colour name, so the scan
    // (which matches on prefix alone, not on knowing Tailwind's whole
    // grammar) would otherwise misreport them. Each is a real utility used in
    // the current templates; add to this list only with a one-line reason.
    const NON_COLOR_SUFFIXES: &[&str] = &[
        // text- size/alignment
        "base",
        "sm",
        "lg",
        "xl",
        "xs",
        "center",
        "left",
        "right",
        // `text-balance` controls line wrapping, not text colour.
        "balance",
        // border side / divide axis (`border-t`, `divide-y`, …)
        "t",
        "b",
        "l",
        "r",
        "x",
        "y",
        "l-2",
        // border/outline style keywords
        "dashed",
        "none",
        // `bg-gradient-to-*` is a fixed Tailwind direction keyword, not
        // `bg-<color>`
        "gradient-to-b",
        "gradient-to-t",
        "gradient-to-l",
        "gradient-to-r",
        "gradient-to-tr",
        "gradient-to-tl",
        "gradient-to-br",
        "gradient-to-bl",
        // Raw inline-SVG attributes (`stroke-width="2"`, `stroke-linecap=
        // "round"`, `stroke-linejoin="round"`) that this whole-file text scan
        // matches the same way it would a class, since it does not parse HTML
        // attributes.
        "width",
        "linecap",
        "linejoin",
    ];

    let templates_dir =
        std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../templates"));
    let utility_re = regex::Regex::new(
        r"\b(text|bg|border|ring|outline|from|to|fill|stroke|placeholder|divide|hover:text|hover:bg|hover:border|peer-checked:bg|peer-checked:text|focus-visible:ring)-([a-z][a-z0-9-]*)(?:/\d+)?",
    )
    .unwrap();

    let mut offenders = Vec::new();
    let mut stack = vec![templates_dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&path) else {
                continue;
            };
            for caps in utility_re.captures_iter(&contents) {
                let prefix = caps.get(1).unwrap().as_str();
                let name = caps.get(2).unwrap().as_str();
                if tokens.contains(name)
                    || BUILTIN_COLOR_WORDS.contains(&name)
                    || NON_COLOR_SUFFIXES.contains(&name)
                {
                    continue;
                }
                offenders.push(format!("{}: {prefix}-{name}", path.display()));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "these utilities name a colour absent from input.css's @theme (and not on the \
         built-in/non-colour allowlists) — first offender wins, add the token or fix the \
         template:\n{}",
        offenders.join("\n")
    );
}

/// Static UI copy belongs in the i18n catalog and reaches JS through a
/// server-rendered `data-*` attribute (see `search.js`'s `ds.label*` reads),
/// never as a literal in the script itself — a hardcoded string here ships in
/// whatever language the developer typed it in, for every locale.
///
/// Heuristic only (a quoted, capitalised two-word literal), not a real
/// natural-language detector: it does not even match `app.js`'s "Copy to all
/// days" (a `/* … */` comment naming a feature, four words with no quote
/// immediately after the second — never a candidate). It does match
/// `search.js`'s `ds.labelDetails || "View details"`, which is an intentional
/// last-resort fallback for when the translated dataset attribute is absent,
/// not copy shown in the normal path — allowlisted below with that reason.
#[test]
fn no_hardcoded_english_sentences_in_static_js() {
    const ALLOWED: &[(&str, &str)] = &[(
        "search.js",
        "View details", // fallback default for a missing `data-label-details`, not shown copy
    )];
    let js_dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/static/js"));
    let sentence_re = regex::Regex::new(r#""[A-Z][a-z]+ [a-z]+""#).unwrap();
    let mut offenders = Vec::new();
    for entry in std::fs::read_dir(js_dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "js") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let contents = std::fs::read_to_string(&path).expect("read JS source file");
        for m in sentence_re.find_iter(&contents) {
            let text = m.as_str().trim_matches('"');
            if ALLOWED.iter().any(|(f, t)| *f == name && *t == text) {
                continue;
            }
            offenders.push(format!("{name}: {text:?}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "quoted English sentence-like literals in static JS (translatable copy belongs in the \
         i18n catalog, read from a server-rendered data attribute):\n{}",
        offenders.join("\n")
    );
}
