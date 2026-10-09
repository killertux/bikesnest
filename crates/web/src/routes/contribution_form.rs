//! The wire shape of the add/edit contribution form, and the two editors that
//! were previously not expressible at all.
//!
//! One struct serves both `POST /parking/new` and `POST /parking/{id}/edit`:
//! the two forms differ only in which fields they render (the edit page has no
//! position, the add page has no `version`), and a single struct keeps the
//! hours/security grammar defined once.
//!
//! **Hours.** The domain models a weekly schedule (`OpeningHours::Weekly`) with
//! per-day ranges, overnight ranges and an explicit `Unknown`; the form used to
//! offer a single "open 24h" checkbox, so everything else was unreachable. The
//! wire shape is deliberately flat — seven `h_{day}_state` selects plus two
//! optional `h_{day}_{1,2}_{open,close}` times each — because a nested shape
//! would need JavaScript to submit. Every state is reachable with scripting off.
//!
//! **Security.** Eight `sec_{code}` radio groups, each `yes`/`no`/`unknown`.
//! The old checkboxes could only say "yes" (and, having no `name`, dropped
//! every selection when the hidden field's Alpine binding did not run), so a
//! definitive "no" — which the domain, the store and the details page all
//! support — could never be recorded.

use bikesnest_domain::{
    OpeningHours, SECURITY_FEATURE_CODES, SecurityFeature, SecurityState, TimeRange,
};
use chrono::Timelike;

use crate::i18n::Translator;
use crate::view::OptionVm;

/// ISO day 1..=7 paired with the wire key its fields carry.
pub const DAY_KEYS: [&str; 7] = ["mon", "tue", "wed", "thu", "fri", "sat", "sun"];

const DAY_LABEL_KEYS: [&str; 7] = [
    "day.mon", "day.tue", "day.wed", "day.thu", "day.fri", "day.sat", "day.sun",
];

// ---------------------------------------------------------------------------
// The form
// ---------------------------------------------------------------------------

/// Every field either contribution form can post. Absent fields deserialize to
/// the empty string (`serde(default)`), which is what a checkbox-free,
/// script-free browser sends — so the server never depends on the client
/// having filled anything in.
#[derive(Debug, Default, serde::Deserialize)]
pub struct ContributionForm {
    /// Optimistic-concurrency version; only the edit form posts one.
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub parking_type: String,
    #[serde(default)]
    pub cost_kind: String,
    /// Price in major units (e.g. "5"/"5.50"), NOT cents — cents is a backend
    /// detail. The user types a human-readable amount (see the form UX).
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub price_currency: String,
    #[serde(default)]
    pub price_unit: String,
    /// Position. Written by the map picker when it runs; typed by hand in the
    /// "Advanced" details element when it does not.
    #[serde(default)]
    pub lat: String,
    #[serde(default)]
    pub lon: String,
    /// IANA timezone override. Empty is the normal case: the contribution
    /// service derives it from the point through its `TimezoneResolver`.
    #[serde(default)]
    pub timezone: String,
    /// Set by the duplicate interstitial's "create it anyway" button.
    #[serde(default)]
    pub confirm: String,

    #[serde(default)]
    pub h_mon_state: String,
    #[serde(default)]
    pub h_mon_1_open: String,
    #[serde(default)]
    pub h_mon_1_close: String,
    #[serde(default)]
    pub h_mon_2_open: String,
    #[serde(default)]
    pub h_mon_2_close: String,
    #[serde(default)]
    pub h_tue_state: String,
    #[serde(default)]
    pub h_tue_1_open: String,
    #[serde(default)]
    pub h_tue_1_close: String,
    #[serde(default)]
    pub h_tue_2_open: String,
    #[serde(default)]
    pub h_tue_2_close: String,
    #[serde(default)]
    pub h_wed_state: String,
    #[serde(default)]
    pub h_wed_1_open: String,
    #[serde(default)]
    pub h_wed_1_close: String,
    #[serde(default)]
    pub h_wed_2_open: String,
    #[serde(default)]
    pub h_wed_2_close: String,
    #[serde(default)]
    pub h_thu_state: String,
    #[serde(default)]
    pub h_thu_1_open: String,
    #[serde(default)]
    pub h_thu_1_close: String,
    #[serde(default)]
    pub h_thu_2_open: String,
    #[serde(default)]
    pub h_thu_2_close: String,
    #[serde(default)]
    pub h_fri_state: String,
    #[serde(default)]
    pub h_fri_1_open: String,
    #[serde(default)]
    pub h_fri_1_close: String,
    #[serde(default)]
    pub h_fri_2_open: String,
    #[serde(default)]
    pub h_fri_2_close: String,
    #[serde(default)]
    pub h_sat_state: String,
    #[serde(default)]
    pub h_sat_1_open: String,
    #[serde(default)]
    pub h_sat_1_close: String,
    #[serde(default)]
    pub h_sat_2_open: String,
    #[serde(default)]
    pub h_sat_2_close: String,
    #[serde(default)]
    pub h_sun_state: String,
    #[serde(default)]
    pub h_sun_1_open: String,
    #[serde(default)]
    pub h_sun_1_close: String,
    #[serde(default)]
    pub h_sun_2_open: String,
    #[serde(default)]
    pub h_sun_2_close: String,

    #[serde(default)]
    pub sec_dedicated_locking_point: String,
    #[serde(default)]
    pub sec_indoor: String,
    #[serde(default)]
    pub sec_cctv: String,
    #[serde(default)]
    pub sec_staffed: String,
    #[serde(default)]
    pub sec_security_guard: String,
    #[serde(default)]
    pub sec_controlled_access: String,
    #[serde(default)]
    pub sec_well_lit: String,
    #[serde(default)]
    pub sec_restricted_access: String,
}

impl ContributionForm {
    /// The seven days' raw hour fields, in ISO order (Monday first).
    pub fn hours_fields(&self) -> [DayFields; 7] {
        [
            DayFields::new(
                &self.h_mon_state,
                &self.h_mon_1_open,
                &self.h_mon_1_close,
                &self.h_mon_2_open,
                &self.h_mon_2_close,
            ),
            DayFields::new(
                &self.h_tue_state,
                &self.h_tue_1_open,
                &self.h_tue_1_close,
                &self.h_tue_2_open,
                &self.h_tue_2_close,
            ),
            DayFields::new(
                &self.h_wed_state,
                &self.h_wed_1_open,
                &self.h_wed_1_close,
                &self.h_wed_2_open,
                &self.h_wed_2_close,
            ),
            DayFields::new(
                &self.h_thu_state,
                &self.h_thu_1_open,
                &self.h_thu_1_close,
                &self.h_thu_2_open,
                &self.h_thu_2_close,
            ),
            DayFields::new(
                &self.h_fri_state,
                &self.h_fri_1_open,
                &self.h_fri_1_close,
                &self.h_fri_2_open,
                &self.h_fri_2_close,
            ),
            DayFields::new(
                &self.h_sat_state,
                &self.h_sat_1_open,
                &self.h_sat_1_close,
                &self.h_sat_2_open,
                &self.h_sat_2_close,
            ),
            DayFields::new(
                &self.h_sun_state,
                &self.h_sun_1_open,
                &self.h_sun_1_close,
                &self.h_sun_2_open,
                &self.h_sun_2_close,
            ),
        ]
    }

    /// The eight security radio values, in [`SECURITY_FEATURE_CODES`] order.
    pub fn security_fields(&self) -> [String; 8] {
        [
            self.sec_dedicated_locking_point.clone(),
            self.sec_indoor.clone(),
            self.sec_cctv.clone(),
            self.sec_staffed.clone(),
            self.sec_security_guard.clone(),
            self.sec_controlled_access.clone(),
            self.sec_well_lit.clone(),
            self.sec_restricted_access.clone(),
        ]
    }

    /// Every posted value as a hidden input, so the duplicate interstitial can
    /// re-submit the whole form (plus `confirm=1`) without a session stash.
    /// `csrf` and `confirm` are rendered by the template itself.
    pub fn hidden_fields(&self) -> Vec<HiddenField> {
        let mut out = Vec::new();
        let mut push = |name: &str, value: &str| {
            out.push(HiddenField {
                name: name.to_string(),
                value: value.to_string(),
            });
        };
        push("name", &self.name);
        push("address", &self.address);
        push("description", &self.description);
        push("parking_type", &self.parking_type);
        push("cost_kind", &self.cost_kind);
        push("price", &self.price);
        push("price_currency", &self.price_currency);
        push("price_unit", &self.price_unit);
        push("lat", &self.lat);
        push("lon", &self.lon);
        push("timezone", &self.timezone);
        for (key, f) in DAY_KEYS.iter().zip(self.hours_fields()) {
            push(&format!("h_{key}_state"), &f.state);
            push(&format!("h_{key}_1_open"), &f.r1_open);
            push(&format!("h_{key}_1_close"), &f.r1_close);
            push(&format!("h_{key}_2_open"), &f.r2_open);
            push(&format!("h_{key}_2_close"), &f.r2_close);
        }
        for (code, state) in SECURITY_FEATURE_CODES.iter().zip(self.security_fields()) {
            push(&format!("sec_{code}"), &state);
        }
        out
    }
}

/// One `name`/`value` pair the interstitial re-posts. Askama escapes `value`,
/// so user-supplied text cannot break out of the attribute.
#[derive(Debug, Clone)]
pub struct HiddenField {
    pub name: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Hours
// ---------------------------------------------------------------------------

/// What a day's `h_{day}_state` select says. `Unknown` is the default, so a
/// form that posts nothing at all means "hours unknown" — never "closed".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DayState {
    Unknown,
    Closed,
    AllDay,
    Ranges,
}

impl DayState {
    pub fn from_code(raw: &str) -> Self {
        match raw.trim() {
            "closed" => DayState::Closed,
            "all_day" => DayState::AllDay,
            "ranges" => DayState::Ranges,
            _ => DayState::Unknown,
        }
    }

    pub fn as_code(self) -> &'static str {
        match self {
            DayState::Unknown => "unknown",
            DayState::Closed => "closed",
            DayState::AllDay => "all_day",
            DayState::Ranges => "ranges",
        }
    }
}

/// One day's five raw fields, as posted or as pre-filled from stored hours.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DayFields {
    pub state: String,
    pub r1_open: String,
    pub r1_close: String,
    pub r2_open: String,
    pub r2_close: String,
}

impl DayFields {
    fn new(state: &str, r1_open: &str, r1_close: &str, r2_open: &str, r2_close: &str) -> Self {
        Self {
            state: state.to_string(),
            r1_open: r1_open.to_string(),
            r1_close: r1_close.to_string(),
            r2_open: r2_open.to_string(),
            r2_close: r2_close.to_string(),
        }
    }

    pub fn day_state(&self) -> DayState {
        DayState::from_code(&self.state)
    }
}

/// A rejected day, reported next to that day's row rather than as one opaque
/// "invalid field" banner at the top of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoursError {
    /// ISO day 1..=7.
    pub day: u8,
    /// i18n key of the message.
    pub key: &'static str,
}

pub const HOURS_INVALID_RANGE: &str = "form.hours.invalid_range";
pub const HOURS_OVERLAP: &str = "form.hours.overlap";

/// Parse the flat per-day fields into the domain's opening hours.
///
/// Every day `unknown` is the explicit unknown state. Otherwise the result is a
/// `Weekly` set in which a day with no rows is closed — which is exactly how
/// [`OpeningHours::status_at`] reads a day it finds no row for, so `closed` and
/// "unknown in an otherwise-known week" both collapse to "no rows" without
/// changing any evaluation.
pub fn parse_hours(fields: &[DayFields; 7]) -> Result<OpeningHours, HoursError> {
    if fields.iter().all(|f| f.day_state() == DayState::Unknown) {
        return Ok(OpeningHours::Unknown);
    }
    let mut rows: Vec<(u8, TimeRange)> = Vec::new();
    for (index, f) in fields.iter().enumerate() {
        let day = (index + 1) as u8;
        let invalid = HoursError {
            day,
            key: HOURS_INVALID_RANGE,
        };
        match f.day_state() {
            DayState::Unknown | DayState::Closed => {}
            DayState::AllDay => rows.push((day, TimeRange::all_day())),
            DayState::Ranges => {
                let mut day_rows: Vec<TimeRange> = Vec::new();
                for (open, close) in [
                    (f.r1_open.trim(), f.r1_close.trim()),
                    (f.r2_open.trim(), f.r2_close.trim()),
                ] {
                    if open.is_empty() && close.is_empty() {
                        continue;
                    }
                    let (Some(opens), Some(closes)) = (parse_time(open), parse_time(close)) else {
                        return Err(invalid);
                    };
                    // An empty range is not a schedule; the way to say "open
                    // all day" is the `all_day` state.
                    if opens == closes {
                        return Err(invalid);
                    }
                    day_rows.push(TimeRange::new(opens, closes));
                }
                if day_rows.is_empty() {
                    return Err(invalid);
                }
                if day_rows.len() == 2 && ranges_overlap(&day_rows[0], &day_rows[1]) {
                    return Err(HoursError {
                        day,
                        key: HOURS_OVERLAP,
                    });
                }
                rows.extend(day_rows.into_iter().map(|r| (day, r)));
            }
        }
    }
    Ok(OpeningHours::weekly(rows))
}

/// Render stored hours back into the form's flat fields (the edit page's
/// pre-fill, and the round trip the unit tests pin).
pub fn hours_fields_from(hours: &OpeningHours) -> [DayFields; 7] {
    let mut out: [DayFields; 7] = Default::default();
    if hours.is_unknown() {
        for f in &mut out {
            f.state = DayState::Unknown.as_code().to_string();
        }
        return out;
    }
    let by_day = hours.rows_by_day();
    for (index, day) in out.iter_mut().enumerate() {
        let ranges = &by_day[index];
        if ranges.is_empty() {
            day.state = DayState::Closed.as_code().to_string();
            continue;
        }
        if ranges.iter().any(|r| r.all_day) {
            day.state = DayState::AllDay.as_code().to_string();
            continue;
        }
        day.state = DayState::Ranges.as_code().to_string();
        if let Some(r) = ranges.first() {
            day.r1_open = time_value(r.opens_at);
            day.r1_close = time_value(r.closes_at);
        }
        // The form carries two ranges per day; a third stored row (only
        // reachable through a direct write) is dropped rather than silently
        // rewritten into one of the two the editor can show.
        if let Some(r) = ranges.get(1) {
            day.r2_open = time_value(r.opens_at);
            day.r2_close = time_value(r.closes_at);
        }
    }
    out
}

/// `<input type="time">` accepts `HH:MM` and `HH:MM:SS`.
fn parse_time(raw: &str) -> Option<chrono::NaiveTime> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    chrono::NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| chrono::NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .ok()
}

fn time_value(t: chrono::NaiveTime) -> String {
    format!("{:02}:{:02}", t.hour(), t.minute())
}

/// Minutes a range covers, as one or two half-open segments of the local day.
/// A range that closes at or before it opens runs past midnight, so it covers
/// the tail of the day and the head of the next.
fn segments(r: &TimeRange) -> [(u32, u32); 2] {
    if r.all_day {
        return [(0, 24 * 60), (0, 0)];
    }
    let opens = r.opens_at.hour() * 60 + r.opens_at.minute();
    let closes = r.closes_at.hour() * 60 + r.closes_at.minute();
    if closes > opens {
        [(opens, closes), (0, 0)]
    } else {
        [(opens, 24 * 60), (0, closes)]
    }
}

/// Do two ranges on the same day cover any of the same minutes? Overnight
/// ranges are compared by their segments, so 22:00–02:00 and 01:00–03:00 do
/// overlap while 22:00–02:00 and 06:00–09:00 do not.
fn ranges_overlap(a: &TimeRange, b: &TimeRange) -> bool {
    for (start_a, end_a) in segments(a) {
        for (start_b, end_b) in segments(b) {
            if start_a < end_b && start_b < end_a {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Security (tri-state)
// ---------------------------------------------------------------------------

/// Parse the eight radio groups into domain features. `unknown` (the default,
/// and what a missing group deserializes to) contributes no feature: the store
/// writes every code it is not told about as an explicit unknown anyway, so
/// omitting it here and recording it there agree.
pub fn parse_security(states: &[String; 8]) -> Vec<SecurityFeature> {
    SECURITY_FEATURE_CODES
        .iter()
        .zip(states)
        .filter_map(|(code, raw)| match raw.trim() {
            "yes" => Some(SecurityFeature::new(*code, SecurityState::Yes)),
            "no" => Some(SecurityFeature::new(*code, SecurityState::No)),
            _ => None,
        })
        .collect()
}

/// The eight radio values for a location's current attributes (edit pre-fill).
pub fn security_fields_from(security: &[SecurityFeature]) -> [String; 8] {
    let code_state = |code: &str| {
        match security
            .iter()
            .find(|f| f.code() == code)
            .map(SecurityFeature::state)
        {
            Some(SecurityState::Yes) => "yes",
            Some(SecurityState::No) => "no",
            _ => "unknown",
        }
        .to_string()
    };
    std::array::from_fn(|i| code_state(SECURITY_FEATURE_CODES[i]))
}

// ---------------------------------------------------------------------------
// View models
// ---------------------------------------------------------------------------

/// The two proposal forms under the edit page ("move the pin" and "mark
/// gone"). A rejected proposal re-renders the edit page with what the rider
/// entered and the error next to the form they submitted.
#[derive(Debug, Clone, Default)]
pub struct ProposalFormsVm {
    pub move_lat: String,
    pub move_lon: String,
    pub move_timezone: String,
    pub move_reason: String,
    pub move_error: Option<String>,
    pub removal_confirmed: bool,
    pub removal_reason: String,
    pub removal_error: Option<String>,
}

impl ProposalFormsVm {
    /// Blank forms, with the "move" inputs seeded at the spot's current position.
    pub fn at(lat: f64, lon: f64) -> Self {
        Self {
            move_lat: lat.to_string(),
            move_lon: lon.to_string(),
            ..Self::default()
        }
    }
}

/// One day's row in the hours editor.
#[derive(Debug, Clone)]
pub struct HoursDayVm {
    /// Wire key ("mon"), which the field names and the Alpine component use.
    pub key: &'static str,
    pub day_label: &'static str,
    pub state: String,
    pub state_options: Vec<OptionVm>,
    pub r1_open: String,
    pub r1_close: String,
    pub r2_open: String,
    pub r2_close: String,
    /// Whether the range inputs start visible (Alpine takes over from here).
    pub show_ranges: bool,
    pub error: Option<String>,
}

pub fn hours_editor_vm(
    tr: Translator,
    fields: &[DayFields; 7],
    error: Option<HoursError>,
) -> Vec<HoursDayVm> {
    fields
        .iter()
        .enumerate()
        .map(|(index, f)| {
            let state = f.day_state();
            HoursDayVm {
                key: DAY_KEYS[index],
                day_label: tr.t(DAY_LABEL_KEYS[index]),
                state: state.as_code().to_string(),
                state_options: day_state_options(tr, state),
                r1_open: f.r1_open.clone(),
                r1_close: f.r1_close.clone(),
                r2_open: f.r2_open.clone(),
                r2_close: f.r2_close.clone(),
                show_ranges: state == DayState::Ranges,
                error: error
                    .filter(|e| e.day as usize == index + 1)
                    .map(|e| tr.t(e.key).to_string()),
            }
        })
        .collect()
}

fn day_state_options(tr: Translator, selected: DayState) -> Vec<OptionVm> {
    [
        (DayState::Unknown, "hours.unknown"),
        (DayState::Closed, "hours.closed"),
        (DayState::AllDay, "hours.all_day"),
        (DayState::Ranges, "form.hours.ranges"),
    ]
    .into_iter()
    .map(|(state, key)| OptionVm {
        value: state.as_code(),
        label: tr.t(key),
        checked: state == selected,
    })
    .collect()
}

/// One security attribute's `yes` / `no` / `don't know` segmented control.
#[derive(Debug, Clone)]
pub struct TriStateVm {
    /// Field name (`sec_cctv`).
    pub name: String,
    pub label: &'static str,
    /// Currently selected value (`yes` | `no` | `unknown`).
    pub state: String,
    pub options: Vec<OptionVm>,
}

pub fn security_editor_vm(tr: Translator, states: &[String; 8]) -> Vec<TriStateVm> {
    SECURITY_FEATURE_CODES
        .iter()
        .zip(states)
        .map(|(code, raw)| {
            let state = match raw.trim() {
                "yes" => "yes",
                "no" => "no",
                _ => "unknown",
            };
            TriStateVm {
                name: format!("sec_{code}"),
                label: tr.security(code),
                state: state.to_string(),
                options: [
                    ("yes", "form.tri.yes"),
                    ("no", "form.tri.no"),
                    ("unknown", "form.tri.unknown"),
                ]
                .into_iter()
                .map(|(value, key)| OptionVm {
                    value,
                    label: tr.t(key),
                    checked: value == state,
                })
                .collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use bikesnest_domain::hms;

    fn day(state: &str, r1: (&str, &str), r2: (&str, &str)) -> DayFields {
        DayFields::new(state, r1.0, r1.1, r2.0, r2.1)
    }

    fn unknown_week() -> [DayFields; 7] {
        std::array::from_fn(|_| day("unknown", ("", ""), ("", "")))
    }

    #[test]
    fn every_day_unknown_is_the_unknown_state() {
        assert_eq!(parse_hours(&unknown_week()).unwrap(), OpeningHours::Unknown);
        // A form that posts nothing at all means the same thing.
        let empty: [DayFields; 7] = Default::default();
        assert_eq!(parse_hours(&empty).unwrap(), OpeningHours::Unknown);
    }

    #[test]
    fn a_closed_day_contributes_no_rows() {
        let mut week = unknown_week();
        week[0] = day("all_day", ("", ""), ("", ""));
        week[1] = day("closed", ("", ""), ("", ""));
        let hours = parse_hours(&week).unwrap();
        assert_eq!(
            hours,
            OpeningHours::weekly(vec![(1, TimeRange::all_day())]),
            "Tuesday is closed, which is a day with no row"
        );
        assert!(hours.rows_by_day()[1].is_empty());
    }

    #[test]
    fn two_ranges_on_one_day_are_both_kept() {
        let mut week = unknown_week();
        week[0] = day("ranges", ("06:00", "09:00"), ("17:00", "22:00"));
        assert_eq!(
            parse_hours(&week).unwrap(),
            OpeningHours::weekly(vec![
                (1, TimeRange::new(hms(6, 0), hms(9, 0))),
                (1, TimeRange::new(hms(17, 0), hms(22, 0))),
            ])
        );
    }

    #[test]
    fn an_overnight_range_is_accepted() {
        let mut week = unknown_week();
        week[0] = day("ranges", ("22:00", "02:00"), ("", ""));
        assert_eq!(
            parse_hours(&week).unwrap(),
            OpeningHours::weekly(vec![(1, TimeRange::new(hms(22, 0), hms(2, 0)))])
        );
    }

    #[test]
    fn overlapping_ranges_are_rejected_on_their_own_day() {
        let mut week = unknown_week();
        week[2] = day("ranges", ("09:00", "18:00"), ("17:00", "20:00"));
        let err = parse_hours(&week).unwrap_err();
        assert_eq!(err.day, 3, "Wednesday");
        assert_eq!(err.key, HOURS_OVERLAP);

        // An overnight range overlaps an early-morning one that runs into it.
        let mut week = unknown_week();
        week[0] = day("ranges", ("22:00", "02:00"), ("01:00", "03:00"));
        assert_eq!(parse_hours(&week).unwrap_err().key, HOURS_OVERLAP);

        // …but not one that ends before it wraps around.
        let mut week = unknown_week();
        week[0] = day("ranges", ("22:00", "02:00"), ("06:00", "09:00"));
        assert!(parse_hours(&week).is_ok());
    }

    #[test]
    fn an_empty_or_unparsable_range_is_rejected() {
        let mut week = unknown_week();
        week[4] = day("ranges", ("09:00", "09:00"), ("", ""));
        let err = parse_hours(&week).unwrap_err();
        assert_eq!(err.day, 5);
        assert_eq!(err.key, HOURS_INVALID_RANGE);

        // Half a range is not a range.
        let mut week = unknown_week();
        week[0] = day("ranges", ("09:00", ""), ("", ""));
        assert_eq!(parse_hours(&week).unwrap_err().key, HOURS_INVALID_RANGE);

        // "Set hours" with nothing filled in is a mistake, not "closed".
        let mut week = unknown_week();
        week[0] = day("ranges", ("", ""), ("", ""));
        assert_eq!(parse_hours(&week).unwrap_err().key, HOURS_INVALID_RANGE);
    }

    #[test]
    fn stored_hours_round_trip_through_the_form_fields() {
        let hours = OpeningHours::weekly(vec![
            (1, TimeRange::new(hms(6, 0), hms(9, 0))),
            (1, TimeRange::new(hms(17, 0), hms(22, 0))),
            (2, TimeRange::all_day()),
            (5, TimeRange::new(hms(22, 0), hms(2, 0))),
        ]);
        let fields = hours_fields_from(&hours);
        assert_eq!(fields[0].state, "ranges");
        assert_eq!(fields[0].r1_open, "06:00");
        assert_eq!(fields[0].r2_close, "22:00");
        assert_eq!(fields[1].state, "all_day");
        assert_eq!(fields[2].state, "closed");
        assert_eq!(fields[4].state, "ranges");
        assert_eq!(parse_hours(&fields).unwrap(), hours);
    }

    #[test]
    fn unknown_hours_round_trip_as_seven_unknown_days() {
        let fields = hours_fields_from(&OpeningHours::Unknown);
        assert!(fields.iter().all(|f| f.state == "unknown"));
        assert_eq!(parse_hours(&fields).unwrap(), OpeningHours::Unknown);
    }

    #[test]
    fn security_radios_record_yes_and_no_and_skip_unknown() {
        let mut states: [String; 8] = Default::default();
        // SECURITY_FEATURE_CODES order: dedicated_locking_point, indoor, cctv, …
        states[1] = "yes".to_string();
        states[2] = "no".to_string();
        states[3] = "unknown".to_string();
        let parsed = parse_security(&states);
        assert_eq!(parsed.len(), 2, "unknown and missing groups add nothing");
        assert_eq!(parsed[0].code(), "indoor");
        assert_eq!(parsed[0].state(), SecurityState::Yes);
        assert_eq!(parsed[1].code(), "cctv");
        assert_eq!(
            parsed[1].state(),
            SecurityState::No,
            "a definitive no is recordable"
        );
    }

    #[test]
    fn security_fields_round_trip() {
        let features = vec![
            SecurityFeature::new("cctv", SecurityState::No),
            SecurityFeature::new("well_lit", SecurityState::Yes),
        ];
        let fields = security_fields_from(&features);
        assert_eq!(fields[2], "no", "cctv");
        assert_eq!(fields[6], "yes", "well_lit");
        assert_eq!(fields[0], "unknown", "not recorded");
        assert_eq!(parse_security(&fields), features);
    }

    #[test]
    fn hidden_fields_carry_every_hours_and_security_value() {
        let form = ContributionForm {
            name: "Spot".to_string(),
            h_mon_state: "ranges".to_string(),
            h_mon_1_open: "09:00".to_string(),
            h_mon_1_close: "18:00".to_string(),
            sec_cctv: "no".to_string(),
            ..Default::default()
        };
        let hidden = form.hidden_fields();
        let find = |name: &str| {
            hidden
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.value.clone())
        };
        assert_eq!(find("name").as_deref(), Some("Spot"));
        assert_eq!(find("h_mon_state").as_deref(), Some("ranges"));
        assert_eq!(find("h_mon_1_close").as_deref(), Some("18:00"));
        assert_eq!(find("sec_cctv").as_deref(), Some("no"));
        assert_eq!(find("h_sun_2_close").as_deref(), Some(""));
        assert!(
            find("csrf").is_none() && find("confirm").is_none(),
            "the interstitial renders those two itself"
        );
    }
}
