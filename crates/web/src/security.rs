//! Security response headers + Content-Security-Policy.
//!
//! A single middleware adds the hard-header set to *every* response (public,
//! account, admin, media, error): `Strict-Transport-Security` (only when TLS is
//! on), `Content-Security-Policy`, `X-Content-Type-Options`, `Referrer-Policy`,
//! `Permissions-Policy` and `X-Frame-Options`.
//!
//! The CSP is strict for the MapLibre profile. The Google Maps JavaScript API
//! needs its documented remote origins and runtime evaluation allowance; those
//! are added only when that provider is selected.

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request, header};
use axum::middleware::Next;
use axum::response::Response;
use bikesnest_infrastructure::{MapConfig, SecurityConfig};

use crate::htmx;

/// Config-driven security-header policy for the running instance.
#[derive(Debug, Clone)]
pub struct SecurityHeaders {
    /// Whether TLS terminates at/behind this instance. When true, `Strict-Transport-Security`
    /// is emitted (never in plaintext dev).
    tls_on: bool,
    /// Origins MapLibre loads the style (fetch), vector/raster tiles, glyphs and sprites from.
    tile_hosts: Vec<String>,
    /// Origins the browser may reach for client-side geocoding (empty in dev — geocoding is
    /// server-side via the `Geocoder` port).
    geocode_hosts: Vec<String>,
    /// Object-storage origin(s) that parking photos are served from as direct
    /// (pre-signed) URLs, e.g. `http://localhost:9000` in dev or
    /// `https://<bucket>.s3.<region>.amazonaws.com` in production.
    media_hosts: Vec<String>,
    google_maps: bool,
    mapbox_maps: bool,
}

impl SecurityHeaders {
    /// Built once at startup from the parsed CSP origins plus whether TLS
    /// terminates here (which gates HSTS).
    pub fn new(config: &SecurityConfig, map: &MapConfig, tls_on: bool) -> Self {
        Self {
            tls_on,
            tile_hosts: config.tile_hosts.clone(),
            geocode_hosts: config.geocode_hosts.clone(),
            media_hosts: config.media_hosts.clone(),
            google_maps: matches!(map, MapConfig::Google { .. }),
            mapbox_maps: matches!(map, MapConfig::Mapbox { .. }),
        }
    }

    /// The `Content-Security-Policy` value, with hosts selected from config.
    pub fn csp(&self) -> String {
        // Cloudflare injects its Web Analytics beacon at the edge in production.
        // https://developers.cloudflare.com/fundamentals/reference/policies-compliances/content-security-policies/
        const CLOUDFLARE_SCRIPT: &str = " https://static.cloudflareinsights.com";
        const CLOUDFLARE_CONNECT: &str = " https://cloudflareinsights.com";
        let tile = self.join_hosts(&self.tile_hosts);
        let geocode = self.join_hosts(&self.geocode_hosts);
        let media = self.join_hosts(&self.media_hosts);
        let google_script = if self.google_maps {
            " 'unsafe-inline' 'unsafe-eval' https://*.googleapis.com https://*.gstatic.com https://*.google.com https://*.ggpht.com https://*.googleusercontent.com blob:"
        } else {
            ""
        };
        let google_style = if self.google_maps {
            " https://fonts.googleapis.com"
        } else {
            ""
        };
        let google_content = if self.google_maps {
            " https://*.googleapis.com https://*.gstatic.com https://*.google.com https://*.ggpht.com https://*.googleusercontent.com"
        } else {
            ""
        };
        let google_font = if self.google_maps {
            " https://fonts.gstatic.com"
        } else {
            ""
        };
        // Google Maps workers fetch inline images, so img-src alone is insufficient.
        // https://developers.google.com/maps/documentation/javascript/content-security-policy
        let google_connect = if self.google_maps { " data: blob:" } else { "" };
        let frame = if self.google_maps {
            "; frame-src https://*.google.com"
        } else {
            ""
        };
        let mapbox_content = if self.mapbox_maps {
            " https://api.mapbox.com https://events.mapbox.com"
        } else {
            ""
        };
        format!(
            "default-src 'self'; \
             script-src 'self'{google_script}{CLOUDFLARE_SCRIPT}; \
             style-src 'self' 'unsafe-inline'{google_style}; \
             img-src 'self' data: blob:{tile}{media}{google_content}{mapbox_content}; \
             font-src 'self'{tile}{google_font}; \
             connect-src 'self'{tile}{geocode}{google_content}{google_connect}{mapbox_content}{CLOUDFLARE_CONNECT}; \
             worker-src 'self' blob:; \
             object-src 'none'; \
             base-uri 'self'; \
             frame-ancestors 'none'; \
             form-action 'self'{frame}"
        )
    }

    /// Join configured origins into a directive fragment (leading space when non-empty).
    fn join_hosts(&self, hosts: &[String]) -> String {
        if hosts.is_empty() {
            String::new()
        } else {
            format!(" {}", hosts.join(" "))
        }
    }
}

/// Axum middleware: append the security-header set to every response.
pub async fn security_headers(
    State(s): State<SecurityHeaders>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let path_is_private = is_private_path(req.uri().path());
    let mut res = next.run(req).await;
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
    // The CSP string is a fixed shape (no forbidden header chars), so
    // `from_str` always succeeds; a failure means a real bug and should panic
    // rather than silently strip the policy.
    head.insert(
        "Content-Security-Policy",
        HeaderValue::from_str(&s.csp()).expect("valid CSP header value"),
    );
    if s.tls_on {
        head.insert(
            "Strict-Transport-Security",
            HeaderValue::from_static("max-age=31536000; includeSubDomains"),
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

    fn headers_with_media(
        tls_on: bool,
        tile_hosts: &[&str],
        geocode_hosts: &[&str],
        media_hosts: &[&str],
    ) -> SecurityHeaders {
        SecurityHeaders {
            tls_on,
            tile_hosts: tile_hosts.iter().map(|s| s.to_string()).collect(),
            geocode_hosts: geocode_hosts.iter().map(|s| s.to_string()).collect(),
            media_hosts: media_hosts.iter().map(|s| s.to_string()).collect(),
            google_maps: false,
            mapbox_maps: false,
        }
    }

    #[test]
    fn csp_is_strict_no_unsafe_eval() {
        let csp = headers(
            false,
            &["https://tiles.example.com"],
            &["https://geo.example.com"],
        )
        .csp();
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
        let csp = headers(false, &[], &[]).csp();
        assert!(!csp.contains("tiles.example.com"));
        assert!(!csp.contains("geo.example.com"));
        assert!(csp.contains("img-src 'self' data: blob:"));
        assert!(csp.contains("font-src 'self'"));
        assert!(csp.contains("connect-src 'self'"));
    }

    #[test]
    fn cloudflare_web_analytics_is_allowed() {
        let csp = headers(false, &[], &[]).csp();
        assert!(csp.contains("script-src 'self' https://static.cloudflareinsights.com"));
        assert!(csp.contains("connect-src 'self' https://cloudflareinsights.com"));
    }

    #[test]
    fn csp_img_src_includes_media_hosts() {
        let csp = headers_with_media(false, &[], &[], &["http://localhost:9000"]).csp();
        assert!(csp.contains("img-src 'self' data: blob: http://localhost:9000"));
    }

    #[test]
    fn google_map_origins_are_enabled_only_for_google_profile() {
        let map = MapConfig::Google {
            browser_api_key: "browser-key".to_string(),
            map_id: "map-id".to_string(),
        };
        let csp = SecurityHeaders::new(&SecurityConfig::default(), &map, false).csp();
        assert!(csp.contains("script-src 'self' 'unsafe-inline' 'unsafe-eval'"));
        assert!(csp.contains("https://*.googleapis.com"));
        assert!(csp.contains("https://fonts.googleapis.com"));
        assert!(csp.contains("https://fonts.gstatic.com"));
        assert!(csp.contains("frame-src https://*.google.com"));
    }

    #[test]
    fn mapbox_map_origins_are_enabled_for_mapbox_profile() {
        let map = MapConfig::Mapbox {
            style_url: "mapbox://styles/mapbox/streets-v12".to_string(),
            access_token: "public-token".to_string(),
        };
        let csp = SecurityHeaders::new(&SecurityConfig::default(), &map, false).csp();
        assert!(
            csp.contains("connect-src 'self' https://api.mapbox.com https://events.mapbox.com")
        );
        assert!(!csp.contains("unsafe-eval"));
    }

    #[test]
    fn worker_inline_fetches_are_allowed_only_for_google_maps() {
        for (google_maps, mapbox_maps) in [(true, false), (false, false), (false, true)] {
            let mut policy = headers(false, &["https://tiles.openfreemap.org"], &[]);
            policy.google_maps = google_maps;
            policy.mapbox_maps = mapbox_maps;
            let csp = policy.csp();
            let connect = csp
                .split(';')
                .map(str::trim)
                .find(|directive| directive.starts_with("connect-src "))
                .unwrap();
            for scheme in ["data:", "blob:"] {
                assert_eq!(
                    connect.split_whitespace().any(|source| source == scheme),
                    google_maps,
                    "unexpected {scheme} allowance in {connect}"
                );
            }
        }
    }

    #[test]
    fn dev_allows_default_street_tiles() {
        let cfg = bikesnest_infrastructure::Config::for_tests("postgres://localhost/x");
        let h = SecurityHeaders::new(&cfg.security, &cfg.map, cfg.tls_on);
        assert!(
            h.tile_hosts
                .iter()
                .any(|h| h == bikesnest_infrastructure::config::DEFAULT_TILE_HOST)
        );
        assert!(h.geocode_hosts.is_empty());
        assert!(!h.tls_on, "plaintext dev never emits HSTS");
    }

    #[test]
    fn hsts_only_when_tls_on() {
        assert!(!headers(false, &[], &[]).tls_on);
        assert!(headers(true, &[], &[]).tls_on);
    }
}
