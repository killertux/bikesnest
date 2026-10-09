//! `POST /csp-report`: where browsers send Content-Security-Policy violations.
//!
//! Both policies name this endpoint (`report-uri` for older browsers,
//! `report-to` + `Reporting-Endpoints` for the Reporting API), so it accepts
//! both wire formats: `application/csp-report` (one `{"csp-report": {…}}`
//! object) and `application/reports+json` (an array of reports).
//!
//! Browsers send these without a CSRF token, so this exact route is the one
//! CSRF exemption (see `crate::auth`). It changes no state: it only logs. It
//! is therefore capped by body size and by a per-IP rate limit, and it logs a
//! redacted summary — the directive, the blocked origin (never a full URL)
//! and the document path without its query — so a report can carry neither
//! a token from a URL nor arbitrary text into the logs.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use serde_json::Value;

use crate::client_ip::ClientIp;
use crate::state::AppState;

/// Largest report body accepted; anything bigger is a 413 before parsing.
pub(crate) const MAX_REPORT_BYTES: usize = 16 * 1024;
/// Reports one client may send per window before they are dropped.
const REPORTS_PER_IP: u32 = 30;
const REPORT_WINDOW: Duration = Duration::from_secs(60);
/// Reports logged from one batch (`application/reports+json` can carry many).
const MAX_LOGGED_PER_REQUEST: usize = 5;
const MAX_FIELD_CHARS: usize = 120;

pub(crate) async fn csp_report(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    let Some(format) = report_format(&headers) else {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE;
    };
    let admitted = state
        .rate_limiter
        .check(
            &format!("csp-report:ip:{ip}"),
            REPORTS_PER_IP,
            REPORT_WINDOW,
        )
        .await;
    if !matches!(admitted, Ok(true)) {
        return StatusCode::TOO_MANY_REQUESTS;
    }
    let Ok(json) = serde_json::from_slice::<Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let reports = violations(format, &json);
    if reports.is_empty() {
        return StatusCode::BAD_REQUEST;
    }
    for report in reports.iter().take(MAX_LOGGED_PER_REQUEST) {
        tracing::warn!(
            target: "bikesnest::csp",
            directive = %report.directive,
            blocked = %report.blocked,
            document = %report.document,
            disposition = %report.disposition,
            "csp violation reported"
        );
    }
    StatusCode::NO_CONTENT
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// `report-uri`: `{"csp-report": {"document-uri": …}}`.
    Legacy,
    /// `report-to`: `[{"type": "csp-violation", "body": {"documentURL": …}}]`.
    Reporting,
}

fn report_format(headers: &HeaderMap) -> Option<Format> {
    let content_type = headers.get(header::CONTENT_TYPE)?.to_str().ok()?;
    let essence = content_type.split(';').next()?.trim().to_ascii_lowercase();
    match essence.as_str() {
        "application/csp-report" | "application/json" => Some(Format::Legacy),
        "application/reports+json" => Some(Format::Reporting),
        _ => None,
    }
}

/// One violation, already redacted for the log.
#[derive(Debug, PartialEq, Eq)]
struct Violation {
    directive: String,
    blocked: String,
    document: String,
    disposition: &'static str,
}

fn violations(format: Format, json: &Value) -> Vec<Violation> {
    match format {
        Format::Legacy => json
            .get("csp-report")
            .and_then(Value::as_object)
            .map(|report| {
                let field = |key: &str| report.get(key).and_then(Value::as_str);
                vec![Violation::redact(
                    field("effective-directive").or_else(|| field("violated-directive")),
                    field("blocked-uri"),
                    field("document-uri"),
                    field("disposition"),
                )]
            })
            .unwrap_or_default(),
        Format::Reporting => json
            .as_array()
            .into_iter()
            .flatten()
            .filter(|report| report.get("type").and_then(Value::as_str) == Some("csp-violation"))
            .filter_map(|report| report.get("body").and_then(Value::as_object))
            .map(|body| {
                let field = |key: &str| body.get(key).and_then(Value::as_str);
                Violation::redact(
                    field("effectiveDirective"),
                    field("blockedURL"),
                    field("documentURL"),
                    field("disposition"),
                )
            })
            .collect(),
    }
}

impl Violation {
    fn redact(
        directive: Option<&str>,
        blocked: Option<&str>,
        document: Option<&str>,
        disposition: Option<&str>,
    ) -> Self {
        Self {
            directive: directive_name(directive.unwrap_or_default()),
            blocked: blocked_source(blocked.unwrap_or_default()),
            document: document_path(document.unwrap_or_default()),
            disposition: match disposition {
                Some("enforce") => "enforce",
                Some("report") => "report",
                _ => "unknown",
            },
        }
    }
}

/// The directive name only (`violated-directive` may carry its sources).
fn directive_name(raw: &str) -> String {
    let name: String = raw
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || *c == '-')
        .take(40)
        .collect();
    if name.is_empty() {
        "unknown".to_string()
    } else {
        name
    }
}

/// The blocked origin (`scheme://host[:port]`), or the keyword/scheme the
/// browser reports for non-URL sources (`inline`, `eval`, `data`, `blob`, …).
fn blocked_source(raw: &str) -> String {
    let raw = raw.trim();
    if let Some((scheme, rest)) = raw.split_once("://") {
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        let host = authority.rsplit('@').next().unwrap_or_default();
        let scheme = sanitize(scheme, |c| c.is_ascii_alphanumeric() || "+.-".contains(c));
        let host = sanitize(host, |c| c.is_ascii_alphanumeric() || ".-:[]*".contains(c));
        return format!("{scheme}://{host}");
    }
    let keyword = raw.split(':').next().unwrap_or_default();
    let keyword = sanitize(keyword, |c| c.is_ascii_alphanumeric() || c == '-');
    if keyword.is_empty() {
        "unknown".to_string()
    } else {
        keyword
    }
}

/// The document's path, never its query or fragment.
fn document_path(raw: &str) -> String {
    let after_authority = match raw.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |at| &rest[at..]),
        None => raw,
    };
    let path = after_authority.split(['?', '#']).next().unwrap_or_default();
    let path = sanitize(path, |c| c.is_ascii_graphic());
    if path.is_empty() {
        "unknown".to_string()
    } else {
        path
    }
}

fn sanitize(raw: &str, keep: impl Fn(char) -> bool) -> String {
    raw.chars()
        .filter(|c| keep(*c))
        .take(MAX_FIELD_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_reports_are_redacted_to_directive_origin_and_path() {
        let json = serde_json::json!({
            "csp-report": {
                "document-uri": "https://bikesnest.example/verify-email?token=SECRET#frag",
                "violated-directive": "script-src-elem 'self'",
                "blocked-uri": "https://user:pw@evil.example:8443/x.js?k=SECRET",
                "disposition": "report",
            }
        });
        assert_eq!(
            violations(Format::Legacy, &json),
            [Violation {
                directive: "script-src-elem".into(),
                blocked: "https://evil.example:8443".into(),
                document: "/verify-email".into(),
                disposition: "report",
            }]
        );
    }

    #[test]
    fn reporting_api_batches_keep_only_csp_violations() {
        let json = serde_json::json!([
            {"type": "deprecation", "body": {}},
            {"type": "csp-violation", "url": "https://x/", "body": {
                "documentURL": "https://bikesnest.example/search?q=home+address",
                "effectiveDirective": "script-src-elem",
                "blockedURL": "inline",
                "disposition": "enforce",
            }},
        ]);
        assert_eq!(
            violations(Format::Reporting, &json),
            [Violation {
                directive: "script-src-elem".into(),
                blocked: "inline".into(),
                document: "/search".into(),
                disposition: "enforce",
            }]
        );
    }

    #[test]
    fn hostile_fields_cannot_forge_log_lines() {
        let v = Violation::redact(
            Some("img-src\nforged=1"),
            Some("data:image/png;base64,AAAA\r\n"),
            Some("https://bikesnest.example/a b\nforged"),
            Some("whatever"),
        );
        assert_eq!(v.directive, "img-src");
        assert_eq!(v.blocked, "data");
        assert_eq!(v.document, "/abforged");
        assert_eq!(v.disposition, "unknown");
        assert!(document_path(&"/x".repeat(500)).len() <= MAX_FIELD_CHARS);
    }

    #[test]
    fn only_the_two_report_media_types_are_accepted() {
        let with = |ct: &str| {
            let mut h = HeaderMap::new();
            h.insert(header::CONTENT_TYPE, ct.parse().unwrap());
            report_format(&h)
        };
        assert_eq!(with("application/csp-report"), Some(Format::Legacy));
        assert_eq!(
            with("application/reports+json; charset=utf-8"),
            Some(Format::Reporting)
        );
        assert_eq!(with("text/plain"), None);
        assert_eq!(with("application/x-www-form-urlencoded"), None);
        assert_eq!(report_format(&HeaderMap::new()), None);
    }
}
