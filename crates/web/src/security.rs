//! Security response headers + Content-Security-Policy.
//!
//! A single middleware adds the hard-header set to *every* response (public,
//! account, admin, media, error): `Strict-Transport-Security` (only when TLS is
//! on), `Content-Security-Policy`, `X-Content-Type-Options`, `Referrer-Policy`,
//! `Permissions-Policy` and `X-Frame-Options`.
//!
//! The CSP is strict for the MapLibre and Mapbox profiles. The Google Maps
//! JavaScript API needs its documented remote origins plus `'unsafe-inline'`
//! and `'unsafe-eval'`; those allowances are sent **only on map pages** (pages
//! that declare `template[data-map-assets]`, marked by [`MapPage`]). Because a
//! boosted htmx navigation keeps the policy of the document it started from,
//! a boosted navigation *into* a map page is answered with `HX-Redirect` when
//! the Google profile is on, so the map page is always a real document load
//! that carries its own policy.
//!
//! Both the enforced and the report-only policy report violations to
//! [`CSP_REPORT_PATH`] (`report-uri` plus `report-to`).

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use bikesnest_infrastructure::config::validate_csp_origin;
use bikesnest_infrastructure::{MapConfig, SecurityConfig};

use crate::htmx;

#[derive(Clone, Debug, Default)]
pub struct CspNonce(pub String);

/// Response extension: this full page declares `template[data-map-assets]`, so
/// it may need the map provider's script allowances.
#[derive(Clone, Copy, Debug)]
pub struct MapPage;

/// Where browsers POST violation reports (`report-uri` / `report-to`).
pub const CSP_REPORT_PATH: &str = "/csp-report";
/// The `Reporting-Endpoints` group name the policies' `report-to` names.
const CSP_REPORT_GROUP: &str = "csp-endpoint";
/// `Reporting-Endpoints` value mapping [`CSP_REPORT_GROUP`] to [`CSP_REPORT_PATH`].
const REPORTING_ENDPOINTS: &str = "csp-endpoint=\"/csp-report\"";

/// Served instead of a policy that could not be represented as a header. The
/// configured origins are validated at startup, so this is defence in depth,
/// never the expected path — and it fails closed (no remote origin at all).
const FALLBACK_CSP: &str = "default-src 'self'; object-src 'none'; base-uri 'self'; \
     frame-ancestors 'none'; form-action 'self'";

/// One page kind's pair of policies, built once at startup.
#[derive(Debug, Clone)]
struct Policies {
    enforced: HeaderValue,
    /// The report-only candidate around its per-request nonce.
    candidate_head: String,
    candidate_tail: String,
}

/// Config-driven security-header policy for the running instance.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    /// Whether TLS terminates at/behind this instance. When true, `Strict-Transport-Security`
    /// is emitted (never in plaintext dev).
    tls_on: bool,
    /// Google profile: map pages need its allowances and a real document load.
    google_maps: bool,
    page: Policies,
    map: Policies,
}

/// The configured origins and provider a policy is built from.
#[derive(Debug, Clone, Default)]
struct Sources {
    /// Origins MapLibre loads the style (fetch), vector/raster tiles, glyphs and sprites from.
    tile_hosts: Vec<String>,
    /// Origins the browser may reach for client-side geocoding (empty in dev — geocoding is
    /// server-side via the `Geocoder` port).
    geocode_hosts: Vec<String>,
    /// Object-storage origin(s) that parking photos are served from as direct
    /// (pre-signed) URLs, e.g. `http://localhost:9000` in dev or
    /// `https://<bucket>.s3.<region>.amazonaws.com` in production.
    media_hosts: Vec<String>,
    mapbox_maps: bool,
}

const GOOGLE_CONTENT: &str = " https://*.googleapis.com https://*.gstatic.com https://*.google.com https://*.ggpht.com https://*.googleusercontent.com";

impl Sources {
    /// The enforced policy. `google` adds the Maps JavaScript API allowances.
    fn enforced(&self, google: bool) -> String {
        // Cloudflare injects its Web Analytics beacon at the edge in production.
        // https://developers.cloudflare.com/fundamentals/reference/policies-compliances/content-security-policies/
        const CLOUDFLARE_SCRIPT: &str = " https://static.cloudflareinsights.com";
        const CLOUDFLARE_CONNECT: &str = " https://cloudflareinsights.com";
        let google_script = if google {
            " 'unsafe-inline' 'unsafe-eval' https://*.googleapis.com https://*.gstatic.com https://*.google.com https://*.ggpht.com https://*.googleusercontent.com blob:"
        } else {
            ""
        };
        format!(
            "default-src 'self'; \
             script-src 'self'{google_script}{CLOUDFLARE_SCRIPT}; \
             {shared}; \
             connect-src 'self'{tile}{geocode}{google_content}{google_connect}{mapbox}{CLOUDFLARE_CONNECT}; \
             worker-src 'self' blob:; \
             object-src 'none'; \
             base-uri 'self'; \
             frame-ancestors 'none'; \
             form-action 'self'{frame}; \
             {reporting}",
            shared = self.style_img_font(google),
            tile = join_hosts(&self.tile_hosts),
            geocode = join_hosts(&self.geocode_hosts),
            google_content = if google { GOOGLE_CONTENT } else { "" },
            google_connect = google_connect(google),
            mapbox = self.mapbox_content(),
            frame = frame(google),
            reporting = reporting(),
        )
    }

    /// Nonce/strict-dynamic candidate emitted report-only until edge analytics
    /// and the live Google SDK have been validated with the same policy.
    /// Returned as the text before and after the nonce.
    fn candidate(&self, google: bool) -> (String, String) {
        let google_eval = if google { " 'unsafe-eval'" } else { "" };
        let tail = format!(
            "' 'strict-dynamic'{google_eval}; \
             {shared}; \
             connect-src 'self'{tile}{geocode}{google_content}{google_connect}{mapbox}; \
             worker-src 'self' blob:; object-src 'none'; base-uri 'self'; \
             frame-ancestors 'none'; form-action 'self'{frame}; \
             {reporting}",
            shared = self.style_img_font(google),
            tile = join_hosts(&self.tile_hosts),
            geocode = join_hosts(&self.geocode_hosts),
            google_content = if google { GOOGLE_CONTENT } else { "" },
            google_connect = google_connect(google),
            mapbox = self.mapbox_content(),
            frame = frame(google),
            reporting = reporting(),
        );
        ("default-src 'self'; script-src 'nonce-".to_string(), tail)
    }

    /// `style-src`, `img-src` and `font-src`, identical in both policies.
    fn style_img_font(&self, google: bool) -> String {
        let tile = join_hosts(&self.tile_hosts);
        format!(
            "style-src 'self' 'unsafe-inline'{google_style}; \
             img-src 'self' data: blob:{tile}{media}{google_content}{mapbox}; \
             font-src 'self'{tile}{google_font}",
            google_style = if google {
                " https://fonts.googleapis.com"
            } else {
                ""
            },
            media = join_hosts(&self.media_hosts),
            google_content = if google { GOOGLE_CONTENT } else { "" },
            mapbox = self.mapbox_content(),
            google_font = if google {
                " https://fonts.gstatic.com"
            } else {
                ""
            },
        )
    }

    fn mapbox_content(&self) -> &'static str {
        if self.mapbox_maps {
            " https://api.mapbox.com https://events.mapbox.com"
        } else {
            ""
        }
    }

    fn policies(&self, google: bool) -> Policies {
        let enforced = HeaderValue::from_str(&self.enforced(google)).unwrap_or_else(|_| {
            tracing::error!("content security policy is not a valid header; serving the fallback");
            HeaderValue::from_static(FALLBACK_CSP)
        });
        let (candidate_head, candidate_tail) = self.candidate(google);
        Policies {
            enforced,
            candidate_head,
            candidate_tail,
        }
    }
}

/// Google Maps workers fetch inline images, so img-src alone is insufficient.
/// https://developers.google.com/maps/documentation/javascript/content-security-policy
fn google_connect(google: bool) -> &'static str {
    if google { " data: blob:" } else { "" }
}

fn frame(google: bool) -> &'static str {
    if google {
        "; frame-src https://*.google.com"
    } else {
        ""
    }
}

fn reporting() -> String {
    format!("report-uri {CSP_REPORT_PATH}; report-to {CSP_REPORT_GROUP}")
}

/// Join configured origins into a directive fragment (leading space when non-empty).
fn join_hosts(hosts: &[String]) -> String {
    if hosts.is_empty() {
        String::new()
    } else {
        format!(" {}", hosts.join(" "))
    }
}

/// Keep only origins the configuration validator accepts. `Config` already
/// refused anything else at startup; a `SecurityConfig` assembled by hand
/// still cannot smuggle a directive or a control character into the header.
fn valid_origins(key: &str, hosts: &[String]) -> Vec<String> {
    hosts
        .iter()
        .filter(|host| match validate_csp_origin(host) {
            Ok(()) => true,
            Err(reason) => {
                tracing::error!(key, %reason, "dropping an invalid CSP origin");
                false
            }
        })
        .cloned()
        .collect()
}

impl SecurityHeaders {
    /// Built once at startup from the parsed CSP origins plus whether TLS
    /// terminates here (which gates HSTS).
    pub fn new(config: &SecurityConfig, map: &MapConfig, tls_on: bool) -> Self {
        let sources = Sources {
            tile_hosts: valid_origins("CSP_TILE_HOSTS", &config.tile_hosts),
            geocode_hosts: valid_origins("CSP_GEOCODE_HOSTS", &config.geocode_hosts),
            media_hosts: valid_origins("CSP_MEDIA_HOSTS", &config.media_hosts),
            mapbox_maps: matches!(map, MapConfig::Mapbox { .. }),
        };
        let google_maps = matches!(map, MapConfig::Google { .. });
        Self {
            tls_on,
            google_maps,
            page: sources.policies(false),
            map: sources.policies(google_maps),
        }
    }

    fn policies(&self, map_page: bool) -> &Policies {
        if map_page { &self.map } else { &self.page }
    }

    /// The enforced `Content-Security-Policy` for a map page or any other page.
    pub fn csp(&self, map_page: bool) -> &str {
        self.policies(map_page)
            .enforced
            .to_str()
            .unwrap_or(FALLBACK_CSP)
    }

    /// The report-only candidate for one response's nonce.
    pub fn candidate_csp(&self, nonce: &str, map_page: bool) -> String {
        let policies = self.policies(map_page);
        format!(
            "{}{nonce}{}",
            policies.candidate_head, policies.candidate_tail
        )
    }
}

/// A navigation that replaces the whole document through htmx (a boosted
/// link/form or a history restore), as opposed to a fragment swap.
fn is_htmx_document_navigation(headers: &HeaderMap) -> bool {
    headers.contains_key(htmx::HX_REQUEST) && !htmx::is_fragment_request(headers)
}

/// Turn a boosted navigation into a real one: htmx assigns `location.href`
/// on `HX-Redirect`, so the page loads with its own policy.
fn full_navigation(target: &str) -> Response {
    let mut res = StatusCode::NO_CONTENT.into_response();
    if let Ok(value) = HeaderValue::from_str(target) {
        res.headers_mut().insert(htmx::HX_REDIRECT, value);
    }
    htmx::vary_fragment(res)
}

/// Axum middleware: append the security-header set to every response.
pub async fn security_headers(
    State(s): State<SecurityHeaders>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let path_is_private = is_private_path(req.uri().path());
    let path_is_static = req.uri().path().starts_with("/static/");
    // Only the Google profile has a map-page policy that differs; a boosted
    // read of a map page then has to become a real document load.
    let reload_target = (s.google_maps
        && matches!(*req.method(), Method::GET | Method::HEAD)
        && is_htmx_document_navigation(req.headers()))
    .then(|| {
        req.uri().path_and_query().map_or_else(
            || req.uri().path().to_string(),
            |pq| pq.as_str().to_string(),
        )
    });
    let nonce = format!("{:032x}", rand::random::<u128>());
    req.extensions_mut().insert(CspNonce(nonce.clone()));
    let mut res = next.run(req).await;
    let mut map_page = res.extensions().get::<MapPage>().is_some();
    if map_page && let Some(target) = reload_target {
        res = full_navigation(&target);
        map_page = false;
    }
    let response_is_success = res.status().is_success();
    let policies = s.policies(map_page);
    let candidate = HeaderValue::from_str(&s.candidate_csp(&nonce, map_page)).ok();
    let head = res.headers_mut();
    head.insert(
        "X-Content-Type-Options",
        HeaderValue::from_static("nosniff"),
    );
    head.insert(
        "Referrer-Policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    head.insert(
        "Permissions-Policy",
        HeaderValue::from_static(
            "camera=(), microphone=(), geolocation=(self), interest-cohort=()",
        ),
    );
    // Legacy guard — the modern guard is CSP `frame-ancestors 'none'`.
    head.insert("X-Frame-Options", HeaderValue::from_static("DENY"));
    head.insert("Content-Security-Policy", policies.enforced.clone());
    // The nonce is hex and the rest was validated at startup, so this always
    // converts; if it ever did not, the report-only header is simply omitted.
    if let Some(candidate) = candidate {
        head.insert("Content-Security-Policy-Report-Only", candidate);
    }
    head.insert(
        "Reporting-Endpoints",
        HeaderValue::from_static(REPORTING_ENDPOINTS),
    );
    if s.tls_on {
        head.insert(
            "Strict-Transport-Security",
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
        );
    }
    // Dynamic pages embed a per-session CSRF token, including otherwise-public
    // pages and styled errors. Responses and redirects may also vary by auth.
    // Preserve explicit caching only for successful static assets. Everything
    // dynamic, plus static misses/errors, is private and never stored.
    if !path_is_static || !response_is_success {
        head.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("private, no-store"),
        );
    }
    // Private data must never be indexed. Also enforced in robots.txt.
    if path_is_private {
        head.insert(
            "X-Robots-Tag",
            HeaderValue::from_static("noindex, nofollow"),
        );
    }
    add_vary(&mut res);
    res
}

/// Every HTML response is negotiated: the locale comes from `Accept-Language`
/// (and the `lang` cookie), the rendering from the session cookie. Say so, or a
/// shared cache serves one visitor's page to another.
///
/// Fragment endpoints have already appended [`htmx::VARY_FRAGMENT`], which is a
/// superset — appending the short list again would only duplicate names.
fn add_vary(res: &mut Response) {
    let is_html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"));
    if !is_html {
        return;
    }
    let already_varies_by_htmx = res
        .headers()
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.to_ascii_lowercase().contains("hx-request"));
    if already_varies_by_htmx {
        return;
    }
    res.headers_mut()
        .append(header::VARY, HeaderValue::from_static(htmx::VARY_HTML));
}

/// Private (account/admin/moderation) paths — never indexable.
fn is_private_path(path: &str) -> bool {
    path.starts_with("/account") || path.starts_with("/admin") || path.starts_with("/moderation")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(tls_on: bool, tile_hosts: &[&str], geocode_hosts: &[&str]) -> SecurityHeaders {
        headers_with_media(tls_on, tile_hosts, geocode_hosts, &[])
    }

    fn security_config(tile: &[&str], geocode: &[&str], media: &[&str]) -> SecurityConfig {
        let owned = |hosts: &[&str]| hosts.iter().map(|s| s.to_string()).collect();
        SecurityConfig {
            tile_hosts: owned(tile),
            geocode_hosts: owned(geocode),
            media_hosts: owned(media),
        }
    }

    fn maplibre() -> MapConfig {
        MapConfig::MapLibre {
            style_url: "https://tiles.openfreemap.org/styles/liberty".to_string(),
            access_token: String::new(),
        }
    }

    fn google() -> MapConfig {
        MapConfig::Google {
            browser_api_key: "browser-key".to_string(),
            map_id: "map-id".to_string(),
        }
    }

    fn mapbox() -> MapConfig {
        MapConfig::Mapbox {
            style_url: "mapbox://styles/mapbox/streets-v12".to_string(),
            access_token: "public-token".to_string(),
        }
    }

    fn headers_with_media(
        tls_on: bool,
        tile_hosts: &[&str],
        geocode_hosts: &[&str],
        media_hosts: &[&str],
    ) -> SecurityHeaders {
        SecurityHeaders::new(
            &security_config(tile_hosts, geocode_hosts, media_hosts),
            &maplibre(),
            tls_on,
        )
    }

    fn directive<'a>(csp: &'a str, name: &str) -> &'a str {
        csp.split(';')
            .map(str::trim)
            .find(|d| d.starts_with(&format!("{name} ")))
            .unwrap_or_else(|| panic!("no {name} in {csp}"))
    }

    #[test]
    fn csp_is_strict_no_unsafe_eval() {
        let csp = headers(
            false,
            &["https://tiles.example.com"],
            &["https://geo.example.com"],
        )
        .csp(false)
        .to_string();
        assert!(csp.contains("script-src 'self'"));
        assert!(!csp.contains("unsafe-eval"));
        assert!(csp.contains("object-src 'none'"));
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(csp.contains("base-uri 'self'"));
        assert!(csp.contains("form-action 'self'"));
        assert!(csp.contains("https://tiles.example.com"));
        assert!(csp.contains("https://geo.example.com"));
    }

    #[test]
    fn configured_csp_hosts_are_omitted_when_absent() {
        let h = headers(false, &[], &[]);
        let csp = h.csp(false);
        assert!(!csp.contains("tiles.example.com"));
        assert!(!csp.contains("geo.example.com"));
        assert!(csp.contains("img-src 'self' data: blob:;"));
        assert!(csp.contains("font-src 'self';"));
        assert!(csp.contains("connect-src 'self'"));
    }

    #[test]
    fn cloudflare_web_analytics_is_allowed() {
        let h = headers(false, &[], &[]);
        let csp = h.csp(false);
        assert!(csp.contains("script-src 'self' https://static.cloudflareinsights.com"));
        assert!(csp.contains("connect-src 'self' https://cloudflareinsights.com"));
    }

    #[test]
    fn both_policies_report_to_the_csp_endpoint() {
        let h = SecurityHeaders::new(&SecurityConfig::default(), &google(), false);
        for map_page in [false, true] {
            for csp in [h.csp(map_page).to_string(), h.candidate_csp("n", map_page)] {
                assert_eq!(directive(&csp, "report-uri"), "report-uri /csp-report");
                assert_eq!(directive(&csp, "report-to"), "report-to csp-endpoint");
            }
        }
        assert_eq!(
            REPORTING_ENDPOINTS,
            format!("{CSP_REPORT_GROUP}=\"{CSP_REPORT_PATH}\"")
        );
    }

    #[test]
    fn report_only_candidate_is_nonce_strict_dynamic_and_eval_free_by_default() {
        let csp = headers(false, &[], &[]).candidate_csp("known-nonce", true);
        let script = directive(&csp, "script-src");
        assert_eq!(script, "script-src 'nonce-known-nonce' 'strict-dynamic'");
        assert!(!csp.contains("cloudflareinsights"));
    }

    #[test]
    fn google_candidate_retains_only_its_documented_eval_exception_on_map_pages() {
        let h = SecurityHeaders::new(&SecurityConfig::default(), &google(), false);
        let map = h.candidate_csp("known-nonce", true);
        assert_eq!(
            directive(&map, "script-src"),
            "script-src 'nonce-known-nonce' 'strict-dynamic' 'unsafe-eval'"
        );
        let page = h.candidate_csp("known-nonce", false);
        assert_eq!(
            directive(&page, "script-src"),
            "script-src 'nonce-known-nonce' 'strict-dynamic'"
        );
        assert!(!page.contains("google"));
    }

    #[test]
    fn csp_img_src_includes_media_hosts() {
        let h = headers_with_media(false, &[], &[], &["http://localhost:9000"]);
        assert!(
            h.csp(false)
                .contains("img-src 'self' data: blob: http://localhost:9000")
        );
    }

    #[test]
    fn google_allowances_are_scoped_to_map_pages() {
        let h = SecurityHeaders::new(&SecurityConfig::default(), &google(), false);
        let map = h.csp(true);
        assert!(map.contains("script-src 'self' 'unsafe-inline' 'unsafe-eval'"));
        assert!(map.contains("https://*.googleapis.com"));
        assert!(map.contains("https://fonts.googleapis.com"));
        assert!(map.contains("https://fonts.gstatic.com"));
        assert!(map.contains("frame-src https://*.google.com"));

        let page = h.csp(false);
        assert!(!page.contains("unsafe-inline' 'unsafe-eval"), "{page}");
        assert!(!page.contains("unsafe-eval"), "{page}");
        assert!(!page.contains("google"), "{page}");
        assert!(!page.contains("frame-src"), "{page}");
        assert!(directive(page, "script-src").starts_with("script-src 'self' https://static"));
    }

    #[test]
    fn non_google_profiles_use_one_policy_for_every_page() {
        for map in [maplibre(), mapbox()] {
            let h = SecurityHeaders::new(&SecurityConfig::default(), &map, false);
            assert_eq!(h.csp(true), h.csp(false));
            assert_eq!(h.candidate_csp("n", true), h.candidate_csp("n", false));
            assert!(!h.google_maps);
        }
    }

    #[test]
    fn mapbox_map_origins_are_enabled_for_mapbox_profile() {
        let h = SecurityHeaders::new(&SecurityConfig::default(), &mapbox(), false);
        let csp = h.csp(true);
        assert!(
            csp.contains("connect-src 'self' https://api.mapbox.com https://events.mapbox.com")
        );
        assert!(!csp.contains("unsafe-eval"));
    }

    #[test]
    fn worker_inline_fetches_are_allowed_only_for_google_map_pages() {
        for (map, map_page, expected) in [
            (google(), true, true),
            (google(), false, false),
            (maplibre(), true, false),
            (mapbox(), true, false),
        ] {
            let config = security_config(&["https://tiles.openfreemap.org"], &[], &[]);
            let h = SecurityHeaders::new(&config, &map, false);
            let csp = h.csp(map_page);
            let connect = directive(csp, "connect-src");
            for scheme in ["data:", "blob:"] {
                assert_eq!(
                    connect.split_whitespace().any(|source| source == scheme),
                    expected,
                    "unexpected {scheme} allowance in {connect}"
                );
            }
        }
    }

    #[test]
    fn origins_that_could_reshape_the_policy_never_reach_the_header() {
        let h = headers_with_media(
            false,
            &[
                "https://tiles.example.com; script-src *",
                "https://ok.example.com",
            ],
            &["https://geo.example.com\nX-Evil: 1"],
            &["https://media.example.com\u{7f}"],
        );
        for map_page in [false, true] {
            let csp = h.csp(map_page);
            assert!(!csp.contains("script-src *"), "{csp}");
            assert!(!csp.contains("geo.example.com"), "{csp}");
            assert!(!csp.contains("media.example.com"), "{csp}");
            assert!(csp.contains("https://ok.example.com"), "{csp}");
            assert!(HeaderValue::from_str(&h.candidate_csp("abc", map_page)).is_ok());
        }
    }

    #[test]
    fn dev_allows_default_street_tiles() {
        let cfg = bikesnest_infrastructure::Config::for_tests("postgres://localhost/x");
        let h = SecurityHeaders::new(&cfg.security, &cfg.map, cfg.tls_on);
        assert!(
            h.csp(false)
                .contains(bikesnest_infrastructure::config::DEFAULT_TILE_HOST)
        );
        assert!(!h.tls_on, "plaintext dev never emits HSTS");
    }

    #[test]
    fn hsts_only_when_tls_on() {
        assert!(!headers(false, &[], &[]).tls_on);
        assert!(headers(true, &[], &[]).tls_on);
    }
}
