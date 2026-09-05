//! Security response headers + Content-Security-Policy.
//!
//! A single middleware adds the hard-header set to *every* response (public,
//! account, admin, media, error): `Strict-Transport-Security` (only when TLS is
//! on), `Content-Security-Policy`, `X-Content-Type-Options`, `Referrer-Policy`,
//! `Permissions-Policy` and `X-Frame-Options`.
//!
//! The CSP is deliberately strict: `script-src 'self'` with **no `'unsafe-eval'`**
//! — this is the whole point of the Alpine CSP build.
//! `style-src 'unsafe-inline'` is retained only because MapLibre injects inline
//! styles (attribution/controls/markers); we add no inline styles of our own, so
//! that surface is MapLibre's alone. The tile/geocode/media origins are templated
//! from configuration so the same binary works against any hosted provider (dev
//! defaults to the `tiles.openfreemap.org` street style for tiles).

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderValue, Request, header};
use axum::middleware::Next;
use axum::response::Response;
use bikesnest_infrastructure::SecurityConfig;

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
}

impl SecurityHeaders {
    /// Built once at startup from the parsed CSP origins plus whether TLS
    /// terminates here (which gates HSTS).
    pub fn new(config: &SecurityConfig, tls_on: bool) -> Self {
        Self {
            tls_on,
            tile_hosts: config.tile_hosts.clone(),
            geocode_hosts: config.geocode_hosts.clone(),
            media_hosts: config.media_hosts.clone(),
        }
    }

    /// The `Content-Security-Policy` value. Hosts templated from config; no `'unsafe-eval'`.
    pub fn csp(&self) -> String {
        let tile = self.join_hosts(&self.tile_hosts);
        let geocode = self.join_hosts(&self.geocode_hosts);
        let media = self.join_hosts(&self.media_hosts);
        format!(
            "default-src 'self'; \
             script-src 'self'; \
             style-src 'self' 'unsafe-inline'; \
             img-src 'self' data: blob:{tile}{media}; \
             font-src 'self'{tile}; \
             connect-src 'self'{tile}{geocode}; \
             worker-src 'self' blob:; \
             object-src 'none'; \
             base-uri 'self'; \
             frame-ancestors 'none'; \
             form-action 'self'"
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
    fn csp_hosts_omitted_when_absent() {
        let csp = headers(false, &[], &[]).csp();
        assert!(!csp.contains("https://"));
        assert!(csp.contains("img-src 'self' data: blob:"));
        assert!(csp.contains("font-src 'self'"));
        assert!(csp.contains("connect-src 'self'"));
    }

    #[test]
    fn csp_img_src_includes_media_hosts() {
        let csp = headers_with_media(false, &[], &[], &["http://localhost:9000"]).csp();
        assert!(csp.contains("img-src 'self' data: blob: http://localhost:9000"));
    }

    #[test]
    fn dev_allows_default_street_tiles() {
        let cfg = bikesnest_infrastructure::Config::for_tests("postgres://localhost/x");
        let h = SecurityHeaders::new(&cfg.security, cfg.tls_on);
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
