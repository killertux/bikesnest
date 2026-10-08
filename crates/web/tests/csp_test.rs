//! CSP/asset consistency: the CSP media-host bug shipped because
//! nothing checked that a rendered page's own asset origins are actually
//! allowed by the `Content-Security-Policy` header riding the same response.
//! This file renders a representative page set, extracts every
//! `<img src>`/`<script src>`/`<link rel="stylesheet|preconnect" href>` origin
//! from the body, and asserts each one is covered by the matching CSP
//! directive (relative URLs by `'self'`, absolute ones by an explicit origin
//! in that directive's host list).
//!
//! Self-contained on purpose: each file under `tests/` is its own binary, so
//! this duplicates the small slice of `http_test.rs`'s helpers it needs
//! (`get_c`, `post_form`/`post_form_hx`, the photo-upload fixture helpers,
//! `admin_cookie`/`verified_cookie`) rather than depending on that file.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bikesnest_infrastructure::{Db, FakeEmailProvider, FakeOAuthProvider, TEST_MEDIA_ORIGIN};
use bikesnest_test_support::{ParkingBuilder, TestPasswordHasher, db_test, test_config};
use bikesnest_web::{RouterDeps, app_router_with};
use http_body_util::BodyExt;
use regex::Regex;
use std::collections::HashMap;
use tower::ServiceExt;

// ---------------------------------------------------------------------------
// Minimal app + auth helpers (trimmed copies of the equivalents in
// http_test.rs — see that file for the full, exercised versions).
// ---------------------------------------------------------------------------

fn csp_app(db: Db) -> (axum::Router, FakeEmailProvider) {
    let email = FakeEmailProvider::with_root(None);
    let config = test_config();
    let deps = RouterDeps {
        email: std::sync::Arc::new(email.clone()),
        oauth: Some(FakeOAuthProvider::new(
            "oauth.user@example.com",
            "sub-oauth-1",
        )),
        hasher: TestPasswordHasher,
        rate_limiter: Box::new(bikesnest_infrastructure::InMemoryRateLimiter::new()),
        storage: std::sync::Arc::new(bikesnest_test_support::TestObjectStorage::new()),
        detail_reads: None,
    };
    let app = app_router_with(std::sync::Arc::new(config), db, deps);
    (app, email)
}

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

async fn get_full(
    app: &axum::Router,
    uri: &str,
    cookie: Option<&str>,
) -> (StatusCode, String, String) {
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
    let csp = res
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body).to_string(), csp)
}

fn anon_source_for(uri: &str) -> Option<&str> {
    if uri.starts_with("/login") {
        Some("/login")
    } else if uri.starts_with("/register") {
        Some("/register")
    } else if uri == "/verify-email" {
        Some("/verify-email")
    } else {
        None
    }
}

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
    if !sc.starts_with("__Host-csrf=") {
        return None;
    }
    let cookie_line = sc.split(';').next().unwrap().to_string();
    let token = cookie_line.split('=').nth(1).unwrap().to_string();
    Some((cookie_line, token))
}

const HX_FRAGMENT: &[(&str, &str)] = &[("HX-Request", "true"), ("HX-Request-Type", "partial")];

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

async fn post_form(
    app: &axum::Router,
    uri: &str,
    fields: &[(&str, &str)],
    cookie: Option<&str>,
) -> (StatusCode, String, Option<String>) {
    post_form_h(app, uri, fields, cookie, &[]).await
}

async fn post_form_hx(
    app: &axum::Router,
    uri: &str,
    fields: &[(&str, &str)],
    cookie: Option<&str>,
) -> (StatusCode, String, Option<String>) {
    post_form_h(app, uri, fields, cookie, HX_FRAGMENT).await
}

fn extract_csrf(html: &str) -> String {
    let marker = r#"name="csrf" content=""#;
    let start = html.find(marker).map(|i| i + marker.len()).unwrap_or(0);
    html[start..].split('"').next().unwrap_or("").to_string()
}

async fn verified_cookie(app: &axum::Router, email: &FakeEmailProvider, addr: &str) -> String {
    post_form(
        app,
        "/register",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    let token = verification_token_to(email, addr);
    get_full(app, &format!("/verify-email?token={token}"), None).await;
    post_form(
        app,
        "/verify-email",
        &[("token", &token), ("password", "password123")],
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

async fn admin_cookie(
    db: &Db,
    app: &axum::Router,
    email: &FakeEmailProvider,
    addr: &str,
) -> String {
    post_form(
        app,
        "/register",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    let token = verification_token_to(email, addr);
    get_full(app, &format!("/verify-email?token={token}"), None).await;
    post_form(
        app,
        "/verify-email",
        &[("token", &token), ("password", "password123")],
        None,
    )
    .await;
    let mut conn = db.acquire().await.unwrap();
    let (uid,): (i64,) = sqlx::query_as("SELECT id FROM users WHERE email = $1")
        .bind(addr)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'ADMIN', NULL) ON CONFLICT DO NOTHING",
    )
    .bind(uid)
    .execute(&mut *conn)
    .await
    .unwrap();
    drop(conn);
    let (_, _, cookie) = post_form(
        app,
        "/login",
        &[("email", addr), ("password", "password123")],
        None,
    )
    .await;
    cookie.unwrap().split(';').next().unwrap().to_string()
}

async fn fixture_location(db: &Db, name: &str) -> i64 {
    let mut conn = db.acquire().await.unwrap();
    let loc = ParkingBuilder::new()
        .with_name(name)
        .create(&mut conn)
        .await
        .unwrap();
    loc.id()
}

fn tiny_jpeg() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(32, 32, image::Rgb([20, 40, 60]));
    let mut b = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut b)
        .encode_image(&img)
        .unwrap();
    b
}

fn multipart_upload(jpeg: &[u8], boundary: &str) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"photo\"; filename=\"photo.jpg\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: image/jpeg\r\n\r\n");
    body.extend_from_slice(jpeg);
    body.extend_from_slice(b"\r\n");
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn post_multipart(
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
            "multipart/form-data; boundary=----bikesnestcsp",
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

// ---------------------------------------------------------------------------
// CSP parsing + origin extraction
// ---------------------------------------------------------------------------

/// `directive-name -> the rest of that directive's source list` (as written
/// in the header, e.g. `"'self' https://demotiles.maplibre.org"`).
fn csp_directives(header: &str) -> HashMap<String, String> {
    header
        .split(';')
        .filter_map(|part| {
            let part = part.trim();
            if part.is_empty() {
                return None;
            }
            let mut it = part.splitn(2, ' ');
            let name = it.next()?.to_string();
            let rest = it.next().unwrap_or("").trim().to_string();
            Some((name, rest))
        })
        .collect()
}

/// The `scheme://host[:port]` of an absolute URL; the URL unchanged if it has
/// no scheme (relative).
fn origin_of(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let after = &url[scheme_end + 3..];
    let host_end = after.find(['/', '?', '#']).unwrap_or(after.len());
    format!("{}{}", &url[..scheme_end + 3], &after[..host_end])
}

/// A relative URL is covered by `'self'`; an absolute one must have its exact
/// origin present in `directive_value` (a simple substring check — good
/// enough for the small, literal host lists this app emits).
fn assert_origin_allowed(
    page: &str,
    label: &str,
    url: &str,
    directive: &str,
    directive_value: &str,
) {
    if url.starts_with("data:") || url.starts_with("blob:") || url.starts_with('#') {
        return;
    }
    if !url.contains("://") {
        assert!(
            directive_value.contains("'self'"),
            "{page}: {label} {url} is same-origin but {directive} does not allow 'self': {directive_value}"
        );
        return;
    }
    let origin = origin_of(url);
    assert!(
        directive_value.contains(origin.as_str()),
        "{page}: {label} {url} (origin {origin}) is not allowed by {directive}: {directive_value:?}"
    );
}

/// Extracts the `src`/`href` of every `<img>`, `<script>`, and `<link
/// rel="stylesheet"|"preconnect">` tag in `body`, tagged with the directive it
/// must be covered by (`connect-src` stands in for "the preconnect origin
/// must appear somewhere in the CSP", per the task note that a preconnect hint
/// is not itself a fetch destination CSP names one directive for).
fn extract_asset_refs(body: &str) -> Vec<(&'static str, String)> {
    let tag_re = Regex::new(r"<(img|script|link)\b[^>]*>").unwrap();
    let href_re = Regex::new(r#"(?:src|href)="([^"]+)""#).unwrap();
    let mut out = Vec::new();
    for m in tag_re.find_iter(body) {
        let tag = m.as_str();
        let Some(url) = href_re.captures(tag).map(|c| c[1].to_string()) else {
            continue;
        };
        if tag.starts_with("<img") {
            out.push(("img-src", url));
        } else if tag.starts_with("<script") {
            out.push(("script-src", url));
        } else if tag.contains(r#"rel="stylesheet""#) {
            out.push(("style-src", url));
        } else if tag.contains(r#"rel="preconnect""#) {
            out.push(("connect-src", url));
        }
    }
    out
}

/// Renders `uri` and asserts every asset origin it references is covered by
/// that response's own CSP header.
async fn assert_page_is_csp_consistent(app: &axum::Router, uri: &str, cookie: Option<&str>) {
    let (status, body, csp) = get_full(app, uri, cookie).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    assert!(!csp.is_empty(), "{uri}: no content-security-policy header");
    let directives = csp_directives(&csp);
    for (directive, url) in extract_asset_refs(&body) {
        let value = directives.get(directive).cloned().unwrap_or_default();
        if directive == "connect-src" {
            // A preconnect hint is not itself fetched under one specific
            // directive — the task's rule is looser: its origin just has to
            // appear somewhere in the policy.
            if !url.contains("://") {
                continue;
            }
            let origin = origin_of(&url);
            assert!(
                csp.contains(origin.as_str()),
                "{uri}: preconnect origin {origin} does not appear anywhere in the CSP: {csp:?}"
            );
            continue;
        }
        assert_origin_allowed(uri, "asset", &url, directive, &value);
    }
}

/// Asserts `uri` renders at least one `<img src>` whose origin is exactly
/// `TEST_MEDIA_ORIGIN` — proof that a real (absolute-origin) presigned photo
/// URL reached the page, not merely a same-origin placeholder. Without this,
/// [`assert_page_is_csp_consistent`] alone could pass on a page that renders
/// no photo at all, and would never actually exercise the `img-src`/
/// `media_hosts` wiring this file exists to guard.
async fn assert_page_has_media_origin_img(app: &axum::Router, uri: &str, cookie: Option<&str>) {
    let (status, body, _) = get_full(app, uri, cookie).await;
    assert_eq!(status, StatusCode::OK, "GET {uri}");
    let has_media_photo = extract_asset_refs(&body)
        .iter()
        .any(|(directive, url)| *directive == "img-src" && url.starts_with(TEST_MEDIA_ORIGIN));
    assert!(
        has_media_photo,
        "{uri}: expected at least one <img src> at {TEST_MEDIA_ORIGIN} (a real photo), found none in: {body}"
    );
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[db_test]
async fn csp_allows_every_asset_origin_it_renders(tx: &mut bikesnest_test_support::TestTx) {
    const PHOTO_USER: &str = "csp-photo-uploader@example.com";
    const ADMIN: &str = "csp-admin@example.com";

    // Every fixture and real router adapter shares this rollback scope.
    // No committed fixture or process-wide ADMIN lock is needed.
    let db = tx.db().await;
    let (app, email) = csp_app(db.clone());

    // A parking page with a published (APPROVED, publicly visible) photo, so
    // the gallery's `<img src>` is exercised too.
    let loc = fixture_location(&db, "CSP Photo Spot").await;
    let uploader = verified_cookie(&app, &email, PHOTO_USER).await;
    let (_, page, _) = get_full(&app, &format!("/parking/{loc}"), Some(&uploader)).await;
    let csrf = extract_csrf(&page);
    let (_, upload_body) = multipart_upload(&tiny_jpeg(), "----bikesnestcsp");
    let (up_status, _) = post_multipart(
        &app,
        &format!("/parking/{loc}/photo"),
        upload_body,
        &uploader,
        Some(&csrf),
        HX_FRAGMENT,
    )
    .await;
    assert_eq!(up_status, StatusCode::OK, "photo upload succeeds");
    let (photo_id,): (i64,) = sqlx::query_as("SELECT id FROM parking_photo WHERE location_id = $1")
        .bind(loc)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();

    let admin = admin_cookie(&db, &app, &email, ADMIN).await;
    let (_, queue, _) = get_full(&app, "/moderation/photos", Some(&admin)).await;
    // The queue holds only PENDING_REVIEW photos, so this is the one window
    // in which our own upload (not yet approved below) is guaranteed to be
    // the one rendering here — proof the queue thumbnail is a real,
    // absolute-origin presigned URL rather than a same-origin placeholder.
    assert!(
        extract_asset_refs(&queue)
            .iter()
            .any(|(directive, url)| *directive == "img-src" && url.starts_with(TEST_MEDIA_ORIGIN)),
        "/moderation/photos: expected at least one pending-photo <img src> at {TEST_MEDIA_ORIGIN}, found none in: {queue}"
    );
    let mod_csrf = extract_csrf(&queue);
    let (approve_status, _, _) = post_form_hx(
        &app,
        &format!("/moderation/photos/parking/{photo_id}/approve"),
        &[("csrf", &mod_csrf)],
        Some(&admin),
    )
    .await;
    assert_eq!(approve_status, StatusCode::OK, "admin approves the photo");

    // --- Render the representative page set and check each one's own asset
    // origins against its own CSP header. -----------------------------------
    assert_page_is_csp_consistent(&app, "/", None).await;
    assert_page_is_csp_consistent(&app, "/search?q=Rua%20XV%20de%20Novembro", None).await;
    assert_page_is_csp_consistent(&app, &format!("/parking/{loc}"), None).await;
    assert_page_is_csp_consistent(&app, "/login", None).await;
    assert_page_is_csp_consistent(&app, "/about", None).await;
    assert_page_is_csp_consistent(&app, "/moderation/photos", Some(&admin)).await;

    // --- The published photo page actually renders an absolute, media-origin
    // <img> (the `/moderation/photos` counterpart was already checked above,
    // in the one window it holds our still-pending upload). ------------------
    // `TestObjectStorage::presigned_get` signs its URLs under
    // `TEST_MEDIA_ORIGIN` (the same origin `Config::for_tests` puts in
    // `security.media_hosts`) rather than a same-origin `/media/...` path, so
    // this is a real end-to-end proof, not just an assertion on config in the
    // abstract: if the double (or the view code building `photo.thumb_url`)
    // ever regressed to a same-origin placeholder, this fails here — and the
    // `assert_page_is_csp_consistent` call above already re-checked this same
    // rendered URL against `img-src`.
    assert_page_has_media_origin_img(&app, &format!("/parking/{loc}"), None).await;

    // --- Mutation-sensitive assertion ---------------------------------------
    // Emptying `security.media_hosts` now fails two different ways: the
    // `assert_page_is_csp_consistent(&app, &format!("/parking/{loc}"), ...)`
    // call above fails first (the rendered photo's `TEST_MEDIA_ORIGIN` origin
    // is no longer in `img-src`), and this assertion — independent of
    // whatever any one page happens to render — pins the exact host list
    // `Config::for_tests` configures. See TESTING.md for the mutation
    // evidence (both failure messages, and the exact revert).
    let (_, _, home_csp) = get_full(&app, "/", None).await;
    let directives = csp_directives(&home_csp);
    let img_src = directives.get("img-src").cloned().unwrap_or_default();
    let media_hosts = &test_config().security.media_hosts;
    assert!(
        !media_hosts.is_empty(),
        "test config must configure at least one media host"
    );
    for host in media_hosts {
        assert!(
            img_src.contains(host.as_str()),
            "img-src must list the configured media host {host}: {img_src}"
        );
    }
}

/// The newest verification link mailed to `addr`. `token_for` returns the
/// first captured link, which belongs to whichever account registered first.
fn verification_token_to(email: &FakeEmailProvider, addr: &str) -> String {
    let mail = email
        .emails()
        .into_iter()
        .rev()
        .find(|m| m.to == addr && m.text.contains("/verify-email?token="))
        .expect("verification email captured");
    regex::Regex::new(r"verify-email\?token=([A-Za-z0-9_-]+)")
        .unwrap()
        .captures(&mail.text)
        .unwrap()[1]
        .to_string()
}
