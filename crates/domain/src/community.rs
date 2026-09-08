//! Community contribution domain.
//!
//! Pure, no I/O. Models the value objects and the confidence-resolution rule
//! for the write flows (add/edit/propose/review/verify/favorite). Persistence
//! and orchestration live in the application / infrastructure layers.

use crate::{DomainError, FreshnessThresholds, UserId};
use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// StarRating
// ---------------------------------------------------------------------------

/// A five-star rating, validated to `1..=5`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct StarRating(u8);

impl StarRating {
    pub fn new(value: u8) -> Result<Self, DomainError> {
        if !(1..=5).contains(&value) {
            return Err(DomainError::Invalid(format!(
                "rating must be between 1 and 5: {value}"
            )));
        }
        Ok(Self(value))
    }

    /// From a DB `SMALLINT`. Frees the repository from checking bounds.
    pub fn from_smallint(value: i16) -> Result<Self, DomainError> {
        if !(1..=5).contains(&value) {
            return Err(DomainError::Invalid(format!(
                "rating must be between 1 and 5: {value}"
            )));
        }
        Ok(Self(value as u8))
    }

    pub fn value(self) -> u8 {
        self.0
    }

    pub fn as_i16(self) -> i16 {
        i16::from(self.0)
    }
}

// ---------------------------------------------------------------------------
// ReviewBody
// ---------------------------------------------------------------------------

/// A review body, trimmed to `1..=2000` chars.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewBody(String);

impl ReviewBody {
    pub fn new(raw: &str) -> Result<Self, DomainError> {
        let trimmed = raw.trim();
        let len = trimmed.chars().count();
        if !(1..=2000).contains(&len) {
            return Err(DomainError::Invalid(format!(
                "review body must be 1..=2000 characters: {len}"
            )));
        }
        Ok(Self(trimmed.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

// ---------------------------------------------------------------------------
// Verification kinds / results
// ---------------------------------------------------------------------------

/// The kind of a verification signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VerificationKind {
    /// A rider confirms whether the location still exists / matches the listing.
    Existence,
    /// A rider confirms or disputes one attribute (name/address/type/…).
    Attribute,
    /// A rider "parked here" — a private, short-lived usage signal.
    ParkedHere,
}

impl VerificationKind {
    pub fn as_code(&self) -> &'static str {
        match self {
            VerificationKind::Existence => "existence",
            VerificationKind::Attribute => "attribute",
            VerificationKind::ParkedHere => "parked_here",
        }
    }

    pub fn from_code(code: &str) -> Result<Self, DomainError> {
        match code {
            "existence" => Ok(VerificationKind::Existence),
            "attribute" => Ok(VerificationKind::Attribute),
            "parked_here" => Ok(VerificationKind::ParkedHere),
            other => Err(DomainError::Invalid(format!(
                "unknown verification kind: {other}"
            ))),
        }
    }
}

/// The result of an *existence* verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExistenceResult {
    /// Confirmed still exists as listed.
    StillExists,
    /// Confirmed the location is gone.
    NoLongerExists,
    /// Exists, but some information is now wrong.
    InfoChanged,
}

impl ExistenceResult {
    pub fn as_code(&self) -> &'static str {
        match self {
            ExistenceResult::StillExists => "still_exists",
            ExistenceResult::NoLongerExists => "no_longer_exists",
            ExistenceResult::InfoChanged => "info_changed",
        }
    }

    pub fn from_code(code: &str) -> Result<Self, DomainError> {
        match code {
            "still_exists" => Ok(ExistenceResult::StillExists),
            "no_longer_exists" => Ok(ExistenceResult::NoLongerExists),
            "info_changed" => Ok(ExistenceResult::InfoChanged),
            other => Err(DomainError::Invalid(format!(
                "unknown existence result: {other}"
            ))),
        }
    }
}

/// The result of an *attribute* verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttributeResult {
    Correct,
    Incorrect,
}

impl AttributeResult {
    pub fn as_code(&self) -> &'static str {
        match self {
            AttributeResult::Correct => "correct",
            AttributeResult::Incorrect => "incorrect",
        }
    }

    pub fn from_code(code: &str) -> Result<Self, DomainError> {
        match code {
            "correct" => Ok(AttributeResult::Correct),
            "incorrect" => Ok(AttributeResult::Incorrect),
            other => Err(DomainError::Invalid(format!(
                "unknown attribute result: {other}"
            ))),
        }
    }
}

/// The per-attribute codes that an *attribute* verification may target.
pub const ATTRIBUTE_CODES: &[&str] = &[
    "name", "address", "type", "cost", "hours", "security", "location",
];

/// Whether `code` is a recognized attribute target.
pub fn is_known_attribute_code(code: &str) -> bool {
    ATTRIBUTE_CODES.contains(&code)
}

// ---------------------------------------------------------------------------
// Proposals
// ---------------------------------------------------------------------------

/// The kind of listing change a rider can submit for approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProposalKind {
    EditDetails,
    /// Move the pin / change the coordinates or timezone.
    MoveLocation,
    /// Change the existence (e.g. propose removal).
    ChangeExistence,
}

impl ProposalKind {
    pub fn as_code(&self) -> &'static str {
        match self {
            ProposalKind::EditDetails => "edit_details",
            ProposalKind::MoveLocation => "move_location",
            ProposalKind::ChangeExistence => "change_existence",
        }
    }

    pub fn from_code(code: &str) -> Result<Self, DomainError> {
        match code {
            "edit_details" => Ok(ProposalKind::EditDetails),
            "move_location" => Ok(ProposalKind::MoveLocation),
            "change_existence" => Ok(ProposalKind::ChangeExistence),
            other => Err(DomainError::Invalid(format!(
                "unknown proposal kind: {other}"
            ))),
        }
    }
}

/// The typed body of a proposal — what the proposer actually wants changed.
///
/// Stored as one JSONB object in `parking_proposal.proposed`. The wire shape is
/// exactly the one already writes, so existing rows parse unchanged and no
/// migration is needed:
///
/// - `move_location` → `{"lat": -25.43, "lon": -49.27, "timezone": "America/Sao_Paulo"}`
/// - `change_existence` → `{"existence": "exists"}` or `{"existence": "removed"}`
///
/// The discriminator is the row's `kind` column, never a field inside the
/// object, so parsing is kind-directed ([`ProposedChange::from_json`]) instead
/// of serde-tagged. Anything the current build cannot read becomes
/// [`ProposedChange::Unknown`] rather than an error, so one corrupt row cannot
/// take the whole moderation queue down.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposedChange {
    EditDetails(crate::ParkingEdit),
    MoveLocation {
        lat: f64,
        lon: f64,
        /// IANA timezone name. Every row written so far carries one; `None`
        /// means the payload omitted it and the moderator must supply it.
        timezone: Option<String>,
    },
    /// `true` = the spot exists (restore to `ACTIVE`); `false` = it is gone
    /// (`REMOVED`).
    ChangeExistence {
        exists: bool,
    },
    /// A payload this build cannot interpret. The queue renders it as "needs
    /// manual review" and approval requires the moderator to fill every value.
    Unknown,
}

impl ProposedChange {
    /// The two `existence` codes the JSON payload uses.
    pub const EXISTS: &'static str = "exists";
    pub const REMOVED: &'static str = "removed";

    /// The proposal kind this change belongs to, or `None` for
    /// [`ProposedChange::Unknown`] (whose kind only the row's column knows).
    pub fn kind(&self) -> Option<ProposalKind> {
        match self {
            ProposedChange::EditDetails(_) => Some(ProposalKind::EditDetails),
            ProposedChange::MoveLocation { .. } => Some(ProposalKind::MoveLocation),
            ProposedChange::ChangeExistence { .. } => Some(ProposalKind::ChangeExistence),
            ProposedChange::Unknown => None,
        }
    }

    /// Parse a stored payload for `kind`. Infallible by design: an
    /// unreadable/legacy-incompatible object yields [`ProposedChange::Unknown`].
    pub fn from_json(kind: ProposalKind, raw: &serde_json::Value) -> Self {
        match kind {
            ProposalKind::EditDetails => crate::ParkingEdit::from_json(raw)
                .map(ProposedChange::EditDetails)
                .unwrap_or(ProposedChange::Unknown),
            ProposalKind::MoveLocation => {
                let lat = raw.get("lat").and_then(|v| v.as_f64());
                let lon = raw.get("lon").and_then(|v| v.as_f64());
                match (lat, lon) {
                    (Some(lat), Some(lon)) if Self::coords_in_range(lat, lon) => {
                        ProposedChange::MoveLocation {
                            lat,
                            lon,
                            timezone: raw
                                .get("timezone")
                                .and_then(|v| v.as_str())
                                .map(str::trim)
                                .filter(|s| !s.is_empty())
                                .map(str::to_string),
                        }
                    }
                    _ => ProposedChange::Unknown,
                }
            }
            ProposalKind::ChangeExistence => {
                match raw.get("existence").and_then(|v| v.as_str()) {
                    Some(Self::EXISTS) => ProposedChange::ChangeExistence { exists: true },
                    Some(Self::REMOVED) => ProposedChange::ChangeExistence { exists: false },
                    // A bool payload was never written, but accepting it costs
                    // nothing and makes the type its own canonical form.
                    None => match raw.get("exists").and_then(|v| v.as_bool()) {
                        Some(exists) => ProposedChange::ChangeExistence { exists },
                        None => ProposedChange::Unknown,
                    },
                    Some(_) => ProposedChange::Unknown,
                }
            }
        }
    }

    /// Render back to the stored wire shape — byte-for-byte the shape wrote,
    /// which is what keeps this a code change rather than a migration.
    /// [`ProposedChange::Unknown`] has nothing to say, so it renders `{}`.
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            ProposedChange::EditDetails(edit) => edit.to_json(),
            ProposedChange::MoveLocation { lat, lon, timezone } => match timezone {
                Some(tz) => serde_json::json!({ "lat": lat, "lon": lon, "timezone": tz }),
                None => serde_json::json!({ "lat": lat, "lon": lon }),
            },
            ProposedChange::ChangeExistence { exists } => {
                let code = if *exists { Self::EXISTS } else { Self::REMOVED };
                serde_json::json!({ "existence": code })
            }
            ProposedChange::Unknown => serde_json::json!({}),
        }
    }

    fn coords_in_range(lat: f64, lon: f64) -> bool {
        lat.is_finite()
            && lon.is_finite()
            && (-90.0..=90.0).contains(&lat)
            && (-180.0..=180.0).contains(&lon)
    }
}

/// A proposal's whole stored payload: the typed change plus the proposer's
/// free-text note, which shares the same JSON object (there is no column for
/// it) and is what the moderation queue shows as "why".
#[derive(Debug, Clone, PartialEq)]
pub struct ProposalPayload {
    pub change: ProposedChange,
    pub reason: Option<String>,
}

impl ProposalPayload {
    pub fn new(change: ProposedChange, reason: Option<&str>) -> Self {
        Self {
            change,
            reason: reason
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
        }
    }

    pub fn from_json(kind: ProposalKind, raw: &serde_json::Value) -> Self {
        Self::new(
            ProposedChange::from_json(kind, raw),
            raw.get("reason").and_then(|v| v.as_str()),
        )
    }

    pub fn to_json(&self) -> serde_json::Value {
        let mut out = self.change.to_json();
        if let (Some(obj), Some(reason)) = (out.as_object_mut(), self.reason.as_deref()) {
            obj.insert("reason".to_string(), serde_json::json!(reason));
        }
        out
    }
}

/// Lifecycle of a proposal. creates `Pending`; resolution is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProposalStatus {
    Pending,
    Approved,
    Rejected,
    Superseded,
}

impl ProposalStatus {
    pub fn as_code(&self) -> &'static str {
        match self {
            ProposalStatus::Pending => "PENDING",
            ProposalStatus::Approved => "APPROVED",
            ProposalStatus::Rejected => "REJECTED",
            ProposalStatus::Superseded => "SUPERSEDED",
        }
    }

    pub fn from_code(code: &str) -> Result<Self, DomainError> {
        match code {
            "PENDING" => Ok(ProposalStatus::Pending),
            "APPROVED" => Ok(ProposalStatus::Approved),
            "REJECTED" => Ok(ProposalStatus::Rejected),
            "SUPERSEDED" => Ok(ProposalStatus::Superseded),
            other => Err(DomainError::Invalid(format!(
                "unknown proposal status: {other}"
            ))),
        }
    }
}

/// The kind of a `parking_revision` row (field-level history, ).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChangeKind {
    Create,
    Edit,
    Moderation,
    Verification,
}

impl ChangeKind {
    pub fn as_code(&self) -> &'static str {
        match self {
            ChangeKind::Create => "create",
            ChangeKind::Edit => "edit",
            ChangeKind::Moderation => "moderation",
            ChangeKind::Verification => "verification",
        }
    }

    pub fn from_code(code: &str) -> Result<Self, DomainError> {
        match code {
            "create" => Ok(ChangeKind::Create),
            "edit" => Ok(ChangeKind::Edit),
            "moderation" => Ok(ChangeKind::Moderation),
            "verification" => Ok(ChangeKind::Verification),
            other => Err(DomainError::Invalid(format!(
                "unknown change kind: {other}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Confidence
// ---------------------------------------------------------------------------

/// Computed, per-detail-read confidence in a location's reported existence and
/// freshness. Never denormalized; conflicts are surfaced, not averaged away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Only the listing's own data — no community existence signal yet.
    Reported,
    /// Positive existence confirmation, not recently.
    Verified,
    /// Positive existence confirmation within the fresh window.
    RecentlyVerified,
    /// Positive existence confirmation that has since aged out.
    Stale,
    /// A community member reports the location no longer exists, contradicting
    /// the active listing. Never silently averaged.
    Conflicting,
}

impl Confidence {
    pub fn as_code(&self) -> &'static str {
        match self {
            Confidence::Reported => "reported",
            Confidence::Verified => "verified",
            Confidence::RecentlyVerified => "recently_verified",
            Confidence::Stale => "stale",
            Confidence::Conflicting => "conflicting",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "reported" => Some(Confidence::Reported),
            "verified" => Some(Confidence::Verified),
            "recently_verified" => Some(Confidence::RecentlyVerified),
            "stale" => Some(Confidence::Stale),
            "conflicting" => Some(Confidence::Conflicting),
            _ => None,
        }
    }
}

/// The latest existence verification per user, already deduped by the reader
/// (`DISTINCT ON (user_id) … ORDER BY user_id, created_at DESC`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExistenceSignal {
    pub user: UserId,
    pub result: ExistenceResult,
    pub at: DateTime<Utc>,
}

impl ExistenceSignal {
    pub fn new(user: UserId, result: ExistenceResult, at: DateTime<Utc>) -> Self {
        Self { user, result, at }
    }
}

/// Resolve the [`Confidence`] of a location from its latest-per-user existence
/// signals.
///
/// 1. No existence signals → [`Confidence::Reported`].
/// 2. Any `no_longer_exists` → [`Confidence::Conflicting`] (the DB says active;
///    the community says gone — never silently averaged).
/// 3. Otherwise (all positives are `still_exists`): classify the **latest**
///    `still_exists` by freshness → `Fresh` ⇒ `RecentlyVerified`;
///    `RecentlyVerified`/`Aging` ⇒ `Verified`; `Stale`/`VeryStale` ⇒ `Stale`.
///
/// `info_changed` (and per-attribute `incorrect`) do **not** change the enum;
/// they feed a separate `disputed` flag. `parked_here` is excluded entirely.
pub fn confidence(
    signals: &[ExistenceSignal],
    now: DateTime<Utc>,
    thresholds: &FreshnessThresholds,
) -> Confidence {
    if signals.is_empty() {
        return Confidence::Reported;
    }
    if signals
        .iter()
        .any(|s| s.result == ExistenceResult::NoLongerExists)
    {
        return Confidence::Conflicting;
    }
    // Only still_exists confirms existence. info_changed-only signals neither
    // confirm nor deny → stay Reported (the caller flags `disputed`).
    let positives: Vec<&ExistenceSignal> = signals
        .iter()
        .filter(|s| s.result == ExistenceResult::StillExists)
        .collect();
    let Some(latest) = positives.iter().max_by(|a, b| a.at.cmp(&b.at)) else {
        return Confidence::Reported;
    };
    let category = crate::freshness::categorize(Some(latest.at), now, thresholds);
    use crate::freshness::FreshnessCategory::*;
    match category {
        Fresh => Confidence::RecentlyVerified,
        RecentlyVerified | Aging => Confidence::Verified,
        Stale | VeryStale => Confidence::Stale,
        Never => Confidence::Reported,
    }
}

// ---------------------------------------------------------------------------
// ChangeKind / code round-trip helpers for history summaries
// ---------------------------------------------------------------------------

/// A short, human-readable change summary for contribution history (localization happens at the
/// web boundary; this is a machine code + a generic label).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionSummary {
    pub version: i64,
    pub change_kind: ChangeKind,
    pub summary: Option<String>,
    pub at: DateTime<Utc>,
    pub snapshot: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn signal(
        user: i64,
        result: ExistenceResult,
        days_ago: i64,
        now: DateTime<Utc>,
    ) -> ExistenceSignal {
        ExistenceSignal::new(UserId(user), result, now - chrono::Duration::days(days_ago))
    }

    // --- StarRating / ReviewBody -----------------------------------------

    #[test]
    fn star_rating_boundaries() {
        assert_eq!(StarRating::new(1).unwrap().value(), 1);
        assert_eq!(StarRating::new(5).unwrap().value(), 5);
        assert!(StarRating::new(0).is_err());
        assert!(StarRating::new(6).is_err());
        assert_eq!(StarRating::from_smallint(3).unwrap().as_i16(), 3);
        assert!(StarRating::from_smallint(0).is_err());
    }

    #[test]
    fn review_body_trims_and_validates_length() {
        let body = ReviewBody::new("  Great rack near the station  ").unwrap();
        assert_eq!(body.as_str(), "Great rack near the station");
        assert!(ReviewBody::new("").is_err());
        assert!(ReviewBody::new("   ").is_err());
        let long = "x".repeat(2001);
        assert!(ReviewBody::new(&long).is_err());
        assert!(ReviewBody::new("ok").is_ok());
    }

    // --- code round-trips -------------------------------------------------

    #[test]
    fn verification_kind_round_trips() {
        for kind in [
            VerificationKind::Existence,
            VerificationKind::Attribute,
            VerificationKind::ParkedHere,
        ] {
            assert_eq!(VerificationKind::from_code(kind.as_code()).unwrap(), kind);
        }
        assert!(VerificationKind::from_code("photo").is_err());
    }

    #[test]
    fn existence_result_round_trips() {
        for r in [
            ExistenceResult::StillExists,
            ExistenceResult::NoLongerExists,
            ExistenceResult::InfoChanged,
        ] {
            assert_eq!(ExistenceResult::from_code(r.as_code()).unwrap(), r);
        }
        assert!(ExistenceResult::from_code("maybe").is_err());
    }

    #[test]
    fn attribute_result_round_trips() {
        assert_eq!(
            AttributeResult::from_code("correct").unwrap(),
            AttributeResult::Correct
        );
        assert_eq!(
            AttributeResult::from_code("incorrect").unwrap(),
            AttributeResult::Incorrect
        );
        assert!(AttributeResult::from_code("fine").is_err());
    }

    #[test]
    fn attribute_code_catalog_is_known() {
        for code in ATTRIBUTE_CODES {
            assert!(is_known_attribute_code(code), "{code}");
        }
        assert!(!is_known_attribute_code("photo"));
    }

    #[test]
    fn proposal_kind_and_status_round_trip() {
        for k in [ProposalKind::MoveLocation, ProposalKind::ChangeExistence] {
            assert_eq!(ProposalKind::from_code(k.as_code()).unwrap(), k);
        }
        for s in [
            ProposalStatus::Pending,
            ProposalStatus::Approved,
            ProposalStatus::Rejected,
            ProposalStatus::Superseded,
        ] {
            assert_eq!(ProposalStatus::from_code(s.as_code()).unwrap(), s);
        }
    }

    #[test]
    fn change_kind_round_trips() {
        for k in [
            ChangeKind::Create,
            ChangeKind::Edit,
            ChangeKind::Moderation,
            ChangeKind::Verification,
        ] {
            assert_eq!(ChangeKind::from_code(k.as_code()).unwrap(), k);
        }
    }

    // --- confidence rule --------------------------------------------------

    fn tz() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap()
    }

    #[test]
    fn confidence_empty_signals_is_reported() {
        assert_eq!(
            confidence(&[], tz(), &crate::freshness::DEFAULT_THRESHOLDS),
            Confidence::Reported
        );
    }

    #[test]
    fn confidence_positive_classifies_by_freshness() {
        let now = tz();
        let thresholds = crate::freshness::DEFAULT_THRESHOLDS;
        // Fresh (0 days ago) → RecentlyVerified.
        assert_eq!(
            confidence(
                &[signal(1, ExistenceResult::StillExists, 0, now)],
                now,
                &thresholds
            ),
            Confidence::RecentlyVerified
        );
        // RecentlyVerified (45 days) → Verified.
        assert_eq!(
            confidence(
                &[signal(1, ExistenceResult::StillExists, 45, now)],
                now,
                &thresholds
            ),
            Confidence::Verified
        );
        // Aging (120 days) → Verified.
        assert_eq!(
            confidence(
                &[signal(1, ExistenceResult::StillExists, 120, now)],
                now,
                &thresholds
            ),
            Confidence::Verified
        );
        // Stale (200 days) → Stale.
        assert_eq!(
            confidence(
                &[signal(1, ExistenceResult::StillExists, 200, now)],
                now,
                &thresholds
            ),
            Confidence::Stale
        );
        // VeryStale (400 days) → Stale.
        assert_eq!(
            confidence(
                &[signal(1, ExistenceResult::StillExists, 400, now)],
                now,
                &thresholds
            ),
            Confidence::Stale
        );
    }

    #[test]
    fn confidence_latest_positive_wins() {
        let now = tz();
        let thresholds = crate::freshness::DEFAULT_THRESHOLDS;
        // One stale user + one fresh user → fresh (recent) confirms.
        let signals = vec![
            signal(1, ExistenceResult::StillExists, 200, now),
            signal(2, ExistenceResult::StillExists, 0, now),
        ];
        assert_eq!(
            confidence(&signals, now, &thresholds),
            Confidence::RecentlyVerified
        );
    }

    #[test]
    fn confidence_no_longer_exists_is_conflicting_regardless_of_positives() {
        let now = tz();
        let thresholds = crate::freshness::DEFAULT_THRESHOLDS;
        let signals = vec![
            signal(1, ExistenceResult::StillExists, 0, now),
            signal(2, ExistenceResult::NoLongerExists, 1, now),
        ];
        assert_eq!(
            confidence(&signals, now, &thresholds),
            Confidence::Conflicting
        );
    }

    #[test]
    fn confidence_info_changed_only_is_reported_not_conflicting() {
        let now = tz();
        let thresholds = crate::freshness::DEFAULT_THRESHOLDS;
        let signals = vec![signal(1, ExistenceResult::InfoChanged, 0, now)];
        assert_eq!(confidence(&signals, now, &thresholds), Confidence::Reported);
    }
}

#[cfg(test)]
mod proposed_change_tests {
    use super::*;

    /// The exact payload every seeded `change_existence` row in the dev
    /// database carries today (`select proposed from parking_proposal`).
    const LEGACY_EXISTENCE: &str = r#"{"existence": "exists"}"#;
    /// The shape `parking_proposal_post` has written for moves since.
    const LEGACY_MOVE: &str = r#"{"lat": -25.4284, "lon": -49.2733, "timezone": "America/Sao_Paulo", "reason": "pin is off"}"#;

    #[test]
    fn parses_the_real_legacy_existence_payload() {
        let raw: serde_json::Value = serde_json::from_str(LEGACY_EXISTENCE).unwrap();
        assert_eq!(
            ProposedChange::from_json(ProposalKind::ChangeExistence, &raw),
            ProposedChange::ChangeExistence { exists: true }
        );
        let payload = ProposalPayload::from_json(ProposalKind::ChangeExistence, &raw);
        assert_eq!(payload.reason, None, "legacy rows carry no reason");
    }

    #[test]
    fn parses_the_legacy_move_payload_including_its_reason() {
        let raw: serde_json::Value = serde_json::from_str(LEGACY_MOVE).unwrap();
        let payload = ProposalPayload::from_json(ProposalKind::MoveLocation, &raw);
        assert_eq!(
            payload.change,
            ProposedChange::MoveLocation {
                lat: -25.4284,
                lon: -49.2733,
                timezone: Some("America/Sao_Paulo".to_string()),
            }
        );
        assert_eq!(payload.reason.as_deref(), Some("pin is off"));
    }

    #[test]
    fn removal_payload_round_trips_through_json() {
        for exists in [true, false] {
            let change = ProposedChange::ChangeExistence { exists };
            let back = ProposedChange::from_json(ProposalKind::ChangeExistence, &change.to_json());
            assert_eq!(back, change);
        }
    }

    #[test]
    fn move_payload_round_trips_with_and_without_a_timezone() {
        for timezone in [Some("Europe/Lisbon".to_string()), None] {
            let change = ProposedChange::MoveLocation {
                lat: 38.72,
                lon: -9.14,
                timezone,
            };
            let back = ProposedChange::from_json(ProposalKind::MoveLocation, &change.to_json());
            assert_eq!(back, change);
        }
    }

    #[test]
    fn payload_round_trip_keeps_the_reason_in_the_same_object() {
        let payload = ProposalPayload::new(
            ProposedChange::ChangeExistence { exists: false },
            Some("  the rack was removed  "),
        );
        let json = payload.to_json();
        assert_eq!(json["existence"], "removed");
        assert_eq!(json["reason"], "the rack was removed", "reason is trimmed");
        assert_eq!(
            ProposalPayload::from_json(ProposalKind::ChangeExistence, &json),
            payload
        );
    }

    #[test]
    fn unreadable_payloads_become_unknown_rather_than_failing() {
        let cases = [
            // A move with no coordinates at all.
            (
                ProposalKind::MoveLocation,
                r#"{"reason": "please move it"}"#,
            ),
            // A move missing one coordinate.
            (ProposalKind::MoveLocation, r#"{"lat": -25.4}"#),
            // Coordinates outside the valid range.
            (ProposalKind::MoveLocation, r#"{"lat": 991.0, "lon": 0.0}"#),
            // An existence code this build does not know.
            (
                ProposalKind::ChangeExistence,
                r#"{"existence": "maybe_gone"}"#,
            ),
            // An empty object (a row written by a future/other version).
            (ProposalKind::ChangeExistence, r#"{}"#),
        ];
        for (kind, raw) in cases {
            let value: serde_json::Value = serde_json::from_str(raw).unwrap();
            assert_eq!(
                ProposedChange::from_json(kind, &value),
                ProposedChange::Unknown,
                "{raw} should not parse for {kind:?}"
            );
        }
    }

    #[test]
    fn unknown_reports_no_kind_but_keeps_the_reason_readable() {
        assert_eq!(ProposedChange::Unknown.kind(), None);
        let raw: serde_json::Value =
            serde_json::from_str(r#"{"existence": "???", "reason": "look at this"}"#).unwrap();
        let payload = ProposalPayload::from_json(ProposalKind::ChangeExistence, &raw);
        assert_eq!(payload.change, ProposedChange::Unknown);
        assert_eq!(
            payload.reason.as_deref(),
            Some("look at this"),
            "a moderator still needs the note to review it by hand"
        );
    }

    #[test]
    fn change_reports_the_kind_it_belongs_to() {
        assert_eq!(
            ProposedChange::MoveLocation {
                lat: 0.0,
                lon: 0.0,
                timezone: None
            }
            .kind(),
            Some(ProposalKind::MoveLocation)
        );
        assert_eq!(
            ProposedChange::ChangeExistence { exists: true }.kind(),
            Some(ProposalKind::ChangeExistence)
        );
    }
}
