//! Shared formatting: labels, money, distances, dates and form options.

use crate::i18n::Translator;
use bikesnest_application::ObjectStorage;
use bikesnest_domain::{Cost, FreshnessCategory, OpenStatus, ParkingType, PricingUnit};
use std::time::Duration;

/// TTL for presigned photo GET URLs rendered into a page.
pub const PHOTO_URL_TTL: Duration = Duration::from_secs(3600);

/// JSON escaped so it can be embedded verbatim in a `<script type="application/json">`
/// (or an HTML attribute) without breaking out (a stored-XSS guard).
///
/// `serde_json` does not escape `<`, `>`, `&`, U+2028 or U+2029, all of which can
/// terminate a `<script>` block or an attribute. Escaping them to `\uXXXX` keeps
/// `JSON.parse` (or the browser's JSON handling) decoding back the original value,
/// but a literal `</script><img …>` can no longer appear in the output.
pub fn escape_script_json(s: String) -> String {
    s.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Resolve a location's stored photo key to a presigned URL, if present.
pub async fn resolve_photo(storage: &dyn ObjectStorage, key: Option<&str>) -> Option<String> {
    let k = key?;
    storage.presigned_get(k, PHOTO_URL_TTL).await.ok()
}

/// Filterable parking-type codes (labels come from the translator).
pub const TYPE_CODES: &[&str] = &["rack", "parking_facility", "indoor", "secured", "locker"];

/// The security catalog codes (labels come from the translator). Canonical list
/// lives in the domain (`SECURITY_FEATURE_CODES`).
use bikesnest_domain::SECURITY_FEATURE_CODES as SECURITY_CODES;

/// One checkbox/radio option with its label and checked-state, precomputed in
/// Rust (Askama templates stay logic-light).
#[derive(Debug, Clone)]
pub struct OptionVm {
    pub value: &'static str,
    pub label: &'static str,
    pub checked: bool,
}

/// Field-scoped form errors: a small set of
/// `(input name, message)` pairs a handler records when it already knows
/// which input a rejected submission belongs to. Askama calls methods on a
/// struct field directly (as it already does for `tr.t(...)`), so a template
/// asks `{% if let Some(msg) = field_errors.err("email") %}` and renders
/// `aria-invalid` + `aria-describedby` on the matching input without the
/// template itself doing any string matching.
///
/// The page-level `error: Option<String>` banner is unchanged and still
/// covers everything that is not field-specific (rate limits, conflicts,
/// "try again").
#[derive(Debug, Clone, Default)]
pub struct FieldErrors(Vec<(&'static str, String)>);

impl FieldErrors {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// One error for one field — the common case (most handlers reject at
    /// most one input per submission).
    pub fn single(field: &'static str, message: String) -> Self {
        Self(vec![(field, message)])
    }

    /// Record another field error (e.g. the same message under more than one
    /// input, when a validation failure cannot be attributed to just one —
    /// `GeoPoint::new`'s "coordinates out of range" flags both `lat` and `lon`).
    pub fn push(&mut self, field: &'static str, message: String) {
        self.0.push((field, message));
    }

    /// The message recorded for `field`, if the handler that rendered this
    /// page found one.
    pub fn err(&self, field: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, m)| m.as_str())
    }

    /// Whether a server-rendered field should be surfaced (for example, by
    /// opening the native disclosure that contains it after a rejected form).
    pub fn has(&self, field: &str) -> bool {
        self.err(field).is_some()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

pub fn type_options(t: Translator, selected: Option<&str>) -> Vec<OptionVm> {
    let sel = selected.unwrap_or("");
    TYPE_CODES
        .iter()
        .map(|code| OptionVm {
            value: code,
            label: type_label_for_code(t, code),
            checked: sel.split(',').any(|c| c.trim() == *code),
        })
        .collect()
}

pub fn security_options(t: Translator, selected: Option<&str>) -> Vec<OptionVm> {
    let sel = selected.unwrap_or("");
    SECURITY_CODES
        .iter()
        .map(|code| OptionVm {
            value: code,
            label: t.security(code),
            checked: sel.split(',').any(|c| c.trim() == *code),
        })
        .collect()
}

fn type_label_for_code(t: Translator, code: &str) -> &'static str {
    match code {
        "rack" => t.t("type.rack"),
        "parking_facility" => t.t("type.parking_facility"),
        "indoor" => t.t("type.indoor"),
        "secured" => t.t("type.secured"),
        "locker" => t.t("type.locker"),
        _ => t.t("type.other"),
    }
}

pub fn type_label(t: Translator, ty: ParkingType) -> &'static str {
    match ty {
        ParkingType::Rack => t.t("type.rack"),
        ParkingType::ParkingFacility => t.t("type.parking_facility"),
        ParkingType::Indoor => t.t("type.indoor"),
        ParkingType::Secured => t.t("type.secured"),
        ParkingType::Locker => t.t("type.locker"),
        ParkingType::Other => t.t("type.other"),
    }
}

/// A distance in the reader's locale: whole metres below 1 km, otherwise
/// kilometres to one decimal (`1.5 km` in en, `1,5 km` in pt-BR).
pub fn distance_label(t: Translator, m: f64) -> String {
    if m < 1000.0 {
        format!("{m:.0} m")
    } else {
        format!("{} km", decimal(t, m / 1000.0, 1))
    }
}

/// `value` rounded to `places` decimals with the locale's decimal separator
/// (a comma in pt-BR, a dot in en). Thousands separators are not added.
fn decimal(t: Translator, value: f64, places: usize) -> String {
    let s = format!("{value:.places$}");
    if t.is_pt() { s.replace('.', ",") } else { s }
}

fn currency_symbol(code: &str) -> &str {
    match code {
        "BRL" => "R$",
        "EUR" => "€",
        "USD" => "$",
        other => other,
    }
}

pub fn cost_label(t: Translator, cost: &Cost) -> String {
    match cost {
        Cost::Free => t.t("cost.free").to_string(),
        Cost::Unknown => t.t("cost.unknown").to_string(),
        Cost::Paid { price: None } => t.t("cost.paid_unknown").to_string(),
        Cost::Paid { price: Some(money) } => {
            let major = money.cents() as f64 / 100.0;
            let unit = match money.unit() {
                PricingUnit::Hour => t.t("unit.hour"),
                PricingUnit::Day => t.t("unit.day"),
                PricingUnit::Month => t.t("unit.month"),
                PricingUnit::Entry => t.t("unit.entry"),
            };
            format!(
                "{} {} / {unit}",
                currency_symbol(money.currency().as_str()),
                format_money(t, major)
            )
        }
    }
}

/// Locale-aware decimal formatting: pt-BR uses a comma as the decimal
/// separator (`1234,56`), en uses a dot. Thousands separators are not added.
pub fn format_money(t: Translator, value: f64) -> String {
    decimal(t, value, 2)
}

/// The average rating to one decimal in the reader's locale, with the review
/// count: `4.5 (12)` in en, `4,5 (12)` in pt-BR.
pub fn rating_label(t: Translator, avg: Option<f64>, count: i64) -> String {
    match avg {
        Some(a) => format!("{} ({count})", decimal(t, a, 1)),
        None => t.t("rating.none").to_string(),
    }
}

pub fn freshness_label(t: Translator, f: FreshnessCategory) -> &'static str {
    match f {
        FreshnessCategory::Fresh => t.t("freshness.fresh"),
        FreshnessCategory::RecentlyVerified => t.t("freshness.recently_verified"),
        FreshnessCategory::Aging => t.t("freshness.aging"),
        FreshnessCategory::Stale => t.t("freshness.stale"),
        FreshnessCategory::VeryStale => t.t("freshness.very_stale"),
        FreshnessCategory::Never => t.t("freshness.never"),
    }
}

pub fn open_label(t: Translator, s: OpenStatus) -> &'static str {
    match s {
        OpenStatus::Open => t.t("open.now"),
        OpenStatus::Closed => t.t("open.closed"),
        OpenStatus::Unknown => t.t("open.unknown"),
    }
}

pub(super) fn time_ago_label(t: Translator, at: chrono::DateTime<chrono::Utc>) -> String {
    let now = chrono::Utc::now();
    let days = (now - at).num_days();
    if days == 0 {
        t.t("time.today").to_string()
    } else if days == 1 {
        t.t("time.yesterday").to_string()
    } else if days < 30 {
        t.t("time.days_ago").replace("{n}", &days.to_string())
    } else {
        let months = days / 30;
        t.t("time.months_ago").replace("{n}", &months.to_string())
    }
}

/// An exact UTC instant, seconds included and the zone spelled out — the form
/// an operator can paste into a ticket or correlate with a log line.
pub fn utc_datetime_label(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M:%S UTC").to_string()
}

/// The value an `<input type="datetime-local">` expects (no zone, minutes).
pub fn datetime_local_value(dt: chrono::DateTime<chrono::Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M").to_string()
}

/// A locale-neutral ISO date-time label (YYYY-MM-DD HH:MM).
pub fn iso_datetime_label(t: Translator, dt: chrono::DateTime<chrono::Utc>) -> String {
    if t.is_pt() {
        dt.format("%d/%m/%Y %H:%M").to_string()
    } else {
        dt.format("%Y-%m-%d %H:%M").to_string()
    }
}
