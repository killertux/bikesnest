//! Moderation and reporting use cases.
//!
//! Ports + read models + [`ModerationService`]. Infrastructure implements the
//! ports; the web layer calls the service for every report / moderation action.
//! The report state machine, the self-resolve guard, the parking invalidation
//! invariant and the proposal-apply correctness all live here.

use crate::audit::{AuditEvent, AuditFilter, AuditLog, AuditLogReader, AuditPage};
use crate::photo::PhotoKind;
use crate::rate_limit::{RateLimitError, RateLimiter};
use async_trait::async_trait;
use bikesnest_domain::{
    ModerationLimits, ModerationState, ProposalKind, ProposalStatus, ProposedChange,
    ReportDescription, ReportOutcome, ReportState, ReportTargetType, Role, UserId,
    is_known_report_reason, reason_allowed_for,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ModerationError {
    #[error("you are not permitted to perform this action")]
    NotAuthorized,
    #[error("you cannot resolve your own report")]
    SelfResolve,
    #[error("report not found")]
    NotFound,
    #[error("target could not be found")]
    TargetNotFound,
    #[error("that report is not in the required state")]
    InvalidState,
    /// This reporter already has an open report on this exact target.
    #[error("you already reported this")]
    AlreadyReported,
    /// The proposal was made against an older version of the location, which
    /// has changed since; applying it would silently clobber that change.
    #[error("this proposal is out of date")]
    StaleProposal,
    #[error("invalid report reason")]
    InvalidReason,
    #[error("invalid input: {0}")]
    InvalidField(String),
    /// A named field of an approve-proposal request was missing or unusable.
    /// Typed (rather than a message) so the web layer translates it instead of
    /// echoing an English literal built in the application layer.
    #[error("invalid proposal field: {0}")]
    InvalidProposalField(ProposalField),
    #[error("too many reports, try again later")]
    RateLimited,
    /// Storage refused a duplicate, or a concurrent writer won the race
    /// (unique violation, serialization failure, deadlock).
    #[error("that change conflicts with an existing record")]
    Conflict,
    /// Storage is unreachable or overloaded; the same request may work shortly.
    #[error("service temporarily unavailable")]
    Unavailable,
    #[error("internal error")]
    Internal,
}

/// The fields an approve-proposal request can fail on. Each maps to one i18n
/// key in the web layer; the enum keeps the message out of the core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalField {
    Lat,
    Lon,
    Timezone,
    Existence,
}

impl ProposalField {
    pub fn as_code(self) -> &'static str {
        match self {
            ProposalField::Lat => "lat",
            ProposalField::Lon => "lon",
            ProposalField::Timezone => "timezone",
            ProposalField::Existence => "existence",
        }
    }
}

impl std::fmt::Display for ProposalField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_code())
    }
}

impl From<RateLimitError> for ModerationError {
    fn from(_: RateLimitError) -> Self {
        ModerationError::RateLimited
    }
}

impl From<crate::audit::AuditError> for ModerationError {
    fn from(_: crate::audit::AuditError) -> Self {
        ModerationError::Internal
    }
}

impl From<bikesnest_domain::DomainError> for ModerationError {
    fn from(e: bikesnest_domain::DomainError) -> Self {
        ModerationError::InvalidField(e.to_string())
    }
}

impl From<crate::ports::ReaderError> for ModerationError {
    fn from(_: crate::ports::ReaderError) -> Self {
        ModerationError::Internal
    }
}

// ---------------------------------------------------------------------------
// Read models
// ---------------------------------------------------------------------------

/// A validated new report, ready to persist.
#[derive(Debug, Clone)]
pub struct NewReport {
    pub reporter_id: UserId,
    pub target_type: ReportTargetType,
    pub target_id: i64,
    pub reason: String,
    pub description: ReportDescription,
}

/// A report as read from the store (for the queue + detail + resolution).
/// `reporter_id` is `None` once the reporter's account is anonymized.
#[derive(Debug, Clone)]
pub struct Report {
    pub id: i64,
    pub reporter_id: Option<UserId>,
    pub target_type: ReportTargetType,
    pub target_id: i64,
    pub reason: String,
    pub description: Option<String>,
    pub state: ReportState,
    pub claimed_by: Option<UserId>,
    pub resolved_by: Option<UserId>,
    pub resolution_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A pending (or resolved) proposal in the moderator queue.
///
/// Carries both sides of the decision: the typed [`ProposedChange`] the rider
/// asked for *and* the location's current values, so the queue can show a real
/// diff without a second query per row. `location_version` is the live version;
/// comparing it to `base_version` is what makes a proposal stale.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub id: i64,
    pub location_id: i64,
    pub location_name: String,
    pub location_address: String,
    /// `None` once the proposer's account is anonymized.
    pub proposer_id: Option<UserId>,
    pub base_version: i64,
    /// The location's version *now*. Equal to `base_version` for a proposal
    /// that still applies cleanly.
    pub location_version: i64,
    pub kind: ProposalKind,
    pub change: ProposedChange,
    /// The proposer's free-text note ("why"), stored inside the same payload.
    pub reason: Option<String>,
    pub current_lat: Option<f64>,
    pub current_lon: Option<f64>,
    pub current_timezone: String,
    pub current_state: ModerationState,
    pub current_snapshot: serde_json::Value,
    pub status: ProposalStatus,
    pub created_at: DateTime<Utc>,
}

impl Proposal {
    /// The location changed after this proposal was written, so approving it
    /// would clobber an edit the proposer never saw. The repository refuses
    /// such an approval with [`ModerationError::StaleProposal`]; this is the
    /// same judgment, made cheaply enough for the queue to show up front.
    pub fn is_stale(&self) -> bool {
        self.base_version != self.location_version
    }
}

/// The values a moderator typed into the approve form. Every field is
/// optional: an absent field means "keep what the proposer proposed".
///
/// This is the whole input the web layer contributes to an approval — the
/// merge rule itself lives in [`ProposalApplication::merge`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProposalOverride {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    pub timezone: Option<String>,
    pub exists: Option<bool>,
}

/// Everything a report row needs to show *what* was reported instead of an
/// opaque `#4057`: the location it belongs to, plus whichever of a review
/// excerpt or a photo key applies to the target kind.
///
/// One value covers all four target kinds because every report resolves to a
/// location, and the queue renders whichever fields are present.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReportTargetPreview {
    pub location_id: Option<i64>,
    pub location_name: Option<String>,
    pub location_address: Option<String>,
    /// The reported review, or the review a reported review photo hangs off.
    pub review_id: Option<i64>,
    /// `None` for an anonymized author.
    pub review_author_id: Option<UserId>,
    pub review_rating: Option<i16>,
    /// The first [`REVIEW_EXCERPT_CHARS`] characters of the review body.
    pub review_excerpt: Option<String>,
    pub photo_id: Option<i64>,
    pub photo_key: Option<String>,
    pub photo_thumbnail_key: Option<String>,
    /// The reported entity's own moderation-state code (`ACTIVE`, `APPROVED`,
    /// `HIDDEN`, `PENDING_REVIEW`, `INVALID`…). The queue reads it to decide
    /// *which* action is still available — hiding an already-hidden review is
    /// not an offer worth making.
    pub target_state: Option<String>,
}

/// How much of a reported review the queue shows before the moderator opens it.
pub const REVIEW_EXCERPT_CHARS: usize = 160;

/// Truncate on a character boundary and mark the cut, so a 2 000-character
/// review does not blow up a queue row.
pub fn review_excerpt(body: &str) -> String {
    let body = body.trim();
    if body.chars().count() <= REVIEW_EXCERPT_CHARS {
        return body.to_string();
    }
    let head: String = body.chars().take(REVIEW_EXCERPT_CHARS).collect();
    format!("{}…", head.trim_end())
}

/// The moderation dashboard's four counts (one query, not four full lists).
#[derive(Debug, Clone, Copy, Default)]
pub struct QueueCounts {
    pub pending_photos: i64,
    pub open_reports: i64,
    pub under_review_reports: i64,
    pub pending_proposals: i64,
}

/// The values an approval actually writes: a validated move (with a resolved
/// timezone) or a validated existence flip. Produced only by
/// [`ProposalApplication::merge`], so the repository can never be handed a
/// half-parsed payload.
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalApplication {
    EditDetails(crate::ParkingEdit),
    MoveLocation {
        lat: f64,
        lon: f64,
        timezone: chrono_tz::Tz,
    },
    /// `true` = exists/restore to `ACTIVE`; `false` = removed → `REMOVED`.
    ChangeExistence {
        exists: bool,
    },
}

impl ProposalApplication {
    /// **The override-merge rule.** Combine what the proposer asked for with
    /// whatever the moderator retyped on the approve form: a field the
    /// moderator filled in wins, an empty one leaves the proposer's value
    /// standing. It used to live in the web handler, where no test
    /// could reach it and where "empty means keep" was an accident of string
    /// parsing rather than a decision.
    ///
    /// `kind` comes from the proposal row, not from `change`, because an
    /// unreadable payload ([`ProposedChange::Unknown`]) still has a kind — and
    /// still gets approved, provided the moderator supplies every value
    /// themselves.
    pub fn merge(
        kind: ProposalKind,
        change: &ProposedChange,
        over: &ProposalOverride,
    ) -> Result<Self, ModerationError> {
        match (kind, change) {
            (ProposalKind::EditDetails, ProposedChange::EditDetails(edit)) => {
                Ok(Self::EditDetails(edit.clone()))
            }
            (ProposalKind::EditDetails, _) => Err(ModerationError::InvalidState),
            (ProposalKind::MoveLocation, ProposedChange::MoveLocation { lat, lon, timezone }) => {
                Self::move_location(
                    over.lat.or(Some(*lat)),
                    over.lon.or(Some(*lon)),
                    over.timezone.as_deref().or(timezone.as_deref()),
                )
            }
            (ProposalKind::ChangeExistence, ProposedChange::ChangeExistence { exists }) => {
                Ok(ProposalApplication::ChangeExistence {
                    exists: over.exists.unwrap_or(*exists),
                })
            }
            // An unreadable payload: nothing to fall back on, so every value
            // has to come from the moderator.
            (ProposalKind::MoveLocation, _) => {
                Self::move_location(over.lat, over.lon, over.timezone.as_deref())
            }
            (ProposalKind::ChangeExistence, _) => Ok(ProposalApplication::ChangeExistence {
                exists: over.exists.ok_or(ModerationError::InvalidProposalField(
                    ProposalField::Existence,
                ))?,
            }),
        }
    }

    /// Validate a merged move. Coordinates are range-checked here as well as in
    /// the payload codec, because an override never went through the codec.
    fn move_location(
        lat: Option<f64>,
        lon: Option<f64>,
        timezone: Option<&str>,
    ) -> Result<Self, ModerationError> {
        let lat = lat.ok_or(ModerationError::InvalidProposalField(ProposalField::Lat))?;
        let lon = lon.ok_or(ModerationError::InvalidProposalField(ProposalField::Lon))?;
        if !lat.is_finite() || !(-90.0..=90.0).contains(&lat) {
            return Err(ModerationError::InvalidProposalField(ProposalField::Lat));
        }
        if !lon.is_finite() || !(-180.0..=180.0).contains(&lon) {
            return Err(ModerationError::InvalidProposalField(ProposalField::Lon));
        }
        let timezone = timezone
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(ModerationError::InvalidProposalField(
                ProposalField::Timezone,
            ))?
            .parse()
            .map_err(|_| ModerationError::InvalidProposalField(ProposalField::Timezone))?;
        Ok(ProposalApplication::MoveLocation { lat, lon, timezone })
    }

    pub fn kind(&self) -> ProposalKind {
        match self {
            ProposalApplication::EditDetails(_) => ProposalKind::EditDetails,
            ProposalApplication::MoveLocation { .. } => ProposalKind::MoveLocation,
            ProposalApplication::ChangeExistence { .. } => ProposalKind::ChangeExistence,
        }
    }
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// The report CRUD + state transitions.
#[async_trait]
pub trait ReportRepository: Send + Sync {
    async fn create(&self, r: &NewReport) -> Result<i64, ModerationError>;
    /// Keyset-paginated, oldest first (`id ASC` — the moderation queue is a
    /// FIFO work list). `after_id` is the last id from the previous page.
    async fn list(
        &self,
        state: Option<ReportState>,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Report>, ModerationError>;
    /// Returns the report incl. its `reporter_id` (needed by the self-resolve guard).
    async fn get(&self, id: i64) -> Result<Option<Report>, ModerationError>;
    /// `OPEN → UNDER_REVIEW`, setting `claimed_by`/`updated_at`. 0 rows → `InvalidState`.
    async fn claim(&self, id: i64, moderator: UserId) -> Result<(), ModerationError>;
    /// `UNDER_REVIEW → RESOLVED|DISMISSED`, setting `resolved_by`/note/`updated_at`.
    async fn resolve(
        &self,
        id: i64,
        moderator: UserId,
        note: &str,
        outcome: ReportOutcome,
    ) -> Result<(), ModerationError>;
}

/// The moderation repository: target existence checks and the content/flip
/// actions (hide/restore/invalidate/proposal-apply).
#[async_trait]
pub trait ModerationRepository: Send + Sync {
    /// One lookup across the four target tables (polymorphic `report.target_id`).
    async fn target_exists(
        &self,
        target_type: ReportTargetType,
        target_id: i64,
    ) -> Result<bool, ModerationError>;
    /// Previews for a whole queue page: one statement per *distinct target
    /// type* present (at most four), never one per row. Targets that no longer
    /// exist are simply absent from the map.
    async fn report_previews(
        &self,
        targets: &[(ReportTargetType, i64)],
    ) -> Result<HashMap<(ReportTargetType, i64), ReportTargetPreview>, ModerationError>;
    /// `ACTIVE → HIDDEN` an existing review. 0 rows → `InvalidState`.
    async fn hide_review(&self, id: i64, moderator: UserId) -> Result<(), ModerationError>;
    /// `HIDDEN → ACTIVE` a review. 0 rows → `InvalidState`.
    async fn restore_review(&self, id: i64, moderator: UserId) -> Result<(), ModerationError>;
    /// `APPROVED → HIDDEN` a photo. 0 rows → `InvalidState`.
    async fn hide_photo(
        &self,
        kind: PhotoKind,
        id: i64,
        moderator: UserId,
    ) -> Result<(), ModerationError>;
    /// `HIDDEN → APPROVED` a photo. 0 rows → `InvalidState`.
    async fn restore_photo(
        &self,
        kind: PhotoKind,
        id: i64,
        moderator: UserId,
    ) -> Result<(), ModerationError>;
    /// Set a parking location's moderation state (only from the allowed `from`
    /// states), bump `version`, append a ``moderation`` revision — one tx.
    async fn set_parking_state(
        &self,
        id: i64,
        from: &[ModerationState],
        to: ModerationState,
        moderator: UserId,
    ) -> Result<(), ModerationError>;
    /// Keyset-paginated, oldest first (`id ASC`). `after_id` is the last id
    /// from the previous page.
    async fn list_pending_proposals(
        &self,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Proposal>, ModerationError>;
    /// The four moderation-dashboard counts in one statement (four scalar
    /// subqueries), instead of loading and `.len()`-ing four full lists.
    async fn queue_counts(&self) -> Result<QueueCounts, ModerationError>;
    async fn get_proposal(&self, id: i64) -> Result<Option<Proposal>, ModerationError>;
    /// Apply the proposal's change (or the moderator's adjusted values), bump
    /// version + append a `moderation` revision, set status `APPROVED`, and
    /// supersede older PENDING proposals on the same location — one tx.
    async fn approve_proposal(
        &self,
        id: i64,
        moderator: UserId,
        applied: ProposalApplication,
    ) -> Result<(), ModerationError>;
    /// Set a proposal to `REJECTED` with a reason; no live change.
    async fn reject_proposal(
        &self,
        id: i64,
        moderator: UserId,
        reason: &str,
    ) -> Result<(), ModerationError>;
}

// ---------------------------------------------------------------------------
// Rate-limit keys: `report:create:user:{id}` and `report:create:ip:{ip}`.
// Limits are configured via [`ModerationLimits`]. Moderator actions
// are audited, not rate-limited.
// ---------------------------------------------------------------------------

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

// ---------------------------------------------------------------------------
// ModerationService
// ---------------------------------------------------------------------------

/// Everything the moderation use cases depend on, bundled for construction.
pub struct ModerationDeps {
    pub reports: Box<dyn ReportRepository>,
    pub moderation: Box<dyn ModerationRepository>,
    pub audit: Box<dyn AuditLog>,
    pub audit_reader: Box<dyn AuditLogReader>,
    pub history: Box<dyn crate::community::ContributionHistoryReader>,
    pub rate_limiter: Box<dyn RateLimiter>,
    /// Runtime moderation limits; defaults to the domain constants.
    pub limits: ModerationLimits,
}

pub struct ModerationService {
    deps: ModerationDeps,
}

impl ModerationService {
    pub fn new(deps: ModerationDeps) -> Self {
        Self { deps }
    }

    fn require_moderator(
        &self,
        user: &crate::auth::AuthenticatedUser,
    ) -> Result<(), ModerationError> {
        if user.has_role(Role::Moderator) || user.has_role(Role::Admin) {
            Ok(())
        } else {
            Err(ModerationError::NotAuthorized)
        }
    }

    fn require_admin(&self, user: &crate::auth::AuthenticatedUser) -> Result<(), ModerationError> {
        if user.has_role(Role::Admin) {
            Ok(())
        } else {
            Err(ModerationError::NotAuthorized)
        }
    }

    async fn allowed(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
    ) -> Result<(), ModerationError> {
        if self.deps.rate_limiter.check(key, limit, window).await? {
            Ok(())
        } else {
            Err(ModerationError::RateLimited)
        }
    }

    // -----------------------------------------------------------------------
    // Reports
    // -----------------------------------------------------------------------

    /// Submit a report. Gated to *authenticated* users (not verified — reporting
    /// abuse must work for a brand-new account); the target must exist; the reason
    /// must be known and allowed for the target; the description is optional and
    /// capped; the action is rate-limited.
    #[allow(clippy::too_many_arguments)]
    pub async fn submit_report(
        &self,
        user: &crate::auth::AuthenticatedUser,
        ip: &str,
        target_type: ReportTargetType,
        target_id: i64,
        reason: &str,
        description: Option<String>,
    ) -> Result<i64, ModerationError> {
        self.allowed(
            &format!("report:create:user:{}", user.id.0),
            self.deps.limits.report_create_user_limit,
            DAY,
        )
        .await?;
        self.allowed(
            &format!("report:create:ip:{ip}"),
            self.deps.limits.report_create_ip_limit,
            DAY,
        )
        .await?;

        if !is_known_report_reason(reason) {
            return Err(ModerationError::InvalidReason);
        }
        if !reason_allowed_for(target_type, reason) {
            return Err(ModerationError::InvalidReason);
        }
        let description = match description {
            Some(raw) if !raw.trim().is_empty() => {
                ReportDescription::new_with_len(&raw, self.deps.limits.report_description_max_len)?
            }
            _ => ReportDescription::new_with_len("", self.deps.limits.report_description_max_len)?,
        };

        if !self
            .deps
            .moderation
            .target_exists(target_type, target_id)
            .await?
        {
            return Err(ModerationError::TargetNotFound);
        }

        let new = NewReport {
            reporter_id: user.id,
            target_type,
            target_id,
            reason: reason.to_string(),
            description,
        };
        // A partial unique index (`report_dedupe_idx`) keeps one OPEN /
        // UNDER_REVIEW report per (reporter, target); its violation is not a
        // failure the user can act on, it is "you already told us".
        let id = self.deps.reports.create(&new).await.map_err(|e| match e {
            ModerationError::Conflict => ModerationError::AlreadyReported,
            other => other,
        })?;
        self.audit(
            Some(user.id),
            "report.created",
            "report",
            id.to_string(),
            serde_json::json!({ "target_type": target_type.as_code(), "target_id": target_id, "reason": reason }),
        )
        .await?;
        Ok(id)
    }

    /// The report queue, bounded + keyset-paginated. `require_moderator`.
    /// `state` of `None` lists all states; `limit` is clamped by the repo.
    pub async fn list_reports(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        state: Option<ReportState>,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Report>, ModerationError> {
        self.require_moderator(moderator)?;
        self.deps.reports.list(state, after_id, limit).await
    }

    /// The dashboard's four counts in one call (`require_moderator`).
    pub async fn queue_counts(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
    ) -> Result<QueueCounts, ModerationError> {
        self.require_moderator(moderator)?;
        self.deps.moderation.queue_counts().await
    }

    pub async fn get_report(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
    ) -> Result<Option<Report>, ModerationError> {
        self.require_moderator(moderator)?;
        self.deps.reports.get(id).await
    }

    /// Previews for the reports currently on screen, batched. Answering "what
    /// was reported?" is the queue's whole job, so this rides alongside
    /// [`Self::list_reports`] rather than being fetched row by row.
    pub async fn report_previews(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        reports: &[Report],
    ) -> Result<HashMap<(ReportTargetType, i64), ReportTargetPreview>, ModerationError> {
        self.require_moderator(moderator)?;
        if reports.is_empty() {
            return Ok(HashMap::new());
        }
        let mut targets: Vec<(ReportTargetType, i64)> = reports
            .iter()
            .map(|r| (r.target_type, r.target_id))
            .collect();
        targets.sort_by_key(|(t, id)| (t.as_code(), *id));
        targets.dedup();
        self.deps.moderation.report_previews(&targets).await
    }

    /// Claim an open report (`OPEN → UNDER_REVIEW`) and record who claimed it.
    pub async fn claim_report(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        self.deps.reports.claim(id, moderator.id).await?;
        self.audit(
            Some(moderator.id),
            "report.claimed",
            "report",
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    /// Resolve or dismiss a claimed report. **Server-side self-resolve guard**:
    /// a user cannot resolve a report they submitted.
    pub async fn resolve_report(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
        outcome: ReportOutcome,
        note: &str,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        let report = self
            .deps
            .reports
            .get(id)
            .await?
            .ok_or(ModerationError::NotFound)?;
        if report.reporter_id == Some(moderator.id) {
            return Err(ModerationError::SelfResolve);
        }
        self.deps
            .reports
            .resolve(id, moderator.id, note, outcome)
            .await?;
        // A "Resolved" report means the reported content was inappropriate, so
        // the target is hidden/invalidated too (idempotent — already-hidden is fine).
        if outcome == ReportOutcome::Resolved {
            self.hide_resolved_target(&report, moderator.id).await?;
        }
        let action = match outcome {
            ReportOutcome::Resolved => "report.resolved",
            ReportOutcome::Dismissed => "report.dismissed",
        };
        self.audit(
            Some(moderator.id),
            action,
            "report",
            id.to_string(),
            serde_json::json!({ "target_type": report.target_type.as_code(), "target_id": report.target_id }),
        )
        .await?;
        Ok(())
    }

    /// Hide/invalidate the target of a resolved report. Tolerant of a target
    /// that is already hidden (the moderator may resolve after hiding separately).
    async fn hide_resolved_target(
        &self,
        report: &Report,
        moderator: UserId,
    ) -> Result<(), ModerationError> {
        let res = match report.target_type {
            ReportTargetType::Review => {
                self.deps
                    .moderation
                    .hide_review(report.target_id, moderator)
                    .await
            }
            ReportTargetType::ParkingPhoto => {
                self.deps
                    .moderation
                    .hide_photo(PhotoKind::Parking, report.target_id, moderator)
                    .await
            }
            ReportTargetType::ReviewPhoto => {
                self.deps
                    .moderation
                    .hide_photo(PhotoKind::Review, report.target_id, moderator)
                    .await
            }
            ReportTargetType::Parking => {
                self.deps
                    .moderation
                    .set_parking_state(
                        report.target_id,
                        &[ModerationState::Active],
                        ModerationState::Invalid,
                        moderator,
                    )
                    .await
            }
        };
        match res {
            Ok(()) => Ok(()),
            // Already in the target state (e.g. already hidden) → not an error.
            Err(ModerationError::InvalidState) => Ok(()),
            // Target deleted after the report was filed → the resolve still stands.
            Err(ModerationError::NotFound) => Ok(()),
            Err(e) => Err(e),
        }
    }

    // -----------------------------------------------------------------------
    // Content moderation actions
    // -----------------------------------------------------------------------

    pub async fn hide_review(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        self.deps.moderation.hide_review(id, moderator.id).await?;
        self.audit(
            Some(moderator.id),
            "review.hidden",
            "review",
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    pub async fn restore_review(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        self.deps
            .moderation
            .restore_review(id, moderator.id)
            .await?;
        self.audit(
            Some(moderator.id),
            "review.restored",
            "review",
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    pub async fn hide_photo(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        kind: PhotoKind,
        id: i64,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        self.deps
            .moderation
            .hide_photo(kind, id, moderator.id)
            .await?;
        self.audit(
            Some(moderator.id),
            "photo.hidden",
            kind.as_code(),
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    pub async fn restore_photo(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        kind: PhotoKind,
        id: i64,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        self.deps
            .moderation
            .restore_photo(kind, id, moderator.id)
            .await?;
        self.audit(
            Some(moderator.id),
            "photo.restored",
            kind.as_code(),
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    /// Invalidate a location: `ACTIVE → INVALID` (takedown, restorable).
    pub async fn invalidate_parking(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        self.deps
            .moderation
            .set_parking_state(
                id,
                &[ModerationState::Active],
                ModerationState::Invalid,
                moderator.id,
            )
            .await?;
        self.audit(
            Some(moderator.id),
            "parking.invalidated",
            "parking_location",
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    /// Restore a location: `INVALID|REMOVED → ACTIVE`.
    pub async fn restore_parking(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        self.deps
            .moderation
            .set_parking_state(
                id,
                &[ModerationState::Invalid, ModerationState::Removed],
                ModerationState::Active,
                moderator.id,
            )
            .await?;
        self.audit(
            Some(moderator.id),
            "parking.restored",
            "parking_location",
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Proposals
    // -----------------------------------------------------------------------

    pub async fn list_pending_proposals(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Proposal>, ModerationError> {
        self.require_moderator(moderator)?;
        self.deps
            .moderation
            .list_pending_proposals(after_id, limit)
            .await
    }

    pub async fn get_proposal(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
    ) -> Result<Option<Proposal>, ModerationError> {
        self.require_moderator(moderator)?;
        self.deps.moderation.get_proposal(id).await
    }

    /// Approve a proposal, optionally with the moderator's own values.
    ///
    /// The service reads the proposal and applies
    /// [`ProposalApplication::merge`] itself — the caller only says which
    /// fields the moderator retyped. That is what keeps the "an empty input
    /// means keep the proposer's value" rule testable and identical for every
    /// caller (it used to be re-derived inside the HTTP handler).
    pub async fn approve_proposal(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
        over: ProposalOverride,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        let proposal = self
            .deps
            .moderation
            .get_proposal(id)
            .await?
            .ok_or(ModerationError::NotFound)?;
        let applied = ProposalApplication::merge(proposal.kind, &proposal.change, &over)?;
        let kind = applied.kind();
        self.deps
            .moderation
            .approve_proposal(id, moderator.id, applied)
            .await?;
        self.audit(
            Some(moderator.id),
            "proposal.approved",
            "parking_proposal",
            id.to_string(),
            serde_json::json!({ "kind": kind.as_code() }),
        )
        .await?;
        Ok(())
    }

    /// Reject a proposal with a reason (no live change).
    pub async fn reject_proposal(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        id: i64,
        reason: &str,
    ) -> Result<(), ModerationError> {
        self.require_moderator(moderator)?;
        self.deps
            .moderation
            .reject_proposal(id, moderator.id, reason)
            .await?;
        self.audit(
            Some(moderator.id),
            "proposal.rejected",
            "parking_proposal",
            id.to_string(),
            serde_json::json!({ "reason": reason }),
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Audit viewer + contribution inspection
    // -----------------------------------------------------------------------

    /// The admin audit-log viewer. ADMIN-only.
    pub async fn list_audit_events(
        &self,
        admin: &crate::auth::AuthenticatedUser,
        filter: AuditFilter,
    ) -> Result<AuditPage, ModerationError> {
        self.require_admin(admin)?;
        self.deps
            .audit_reader
            .list(filter)
            .await
            .map_err(ModerationError::from)
    }

    /// Inspect a target user's contribution history (MODERATOR/ADMIN) — the contribution history
    /// aggregation scoped to that user.
    pub async fn user_contribution_history(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        target: UserId,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<crate::community::ContributionItem>, ModerationError> {
        self.require_moderator(moderator)?;
        self.deps
            .history
            .history(target, after, limit)
            .await
            .map_err(|_| ModerationError::Internal)
    }

    async fn audit(
        &self,
        actor: Option<UserId>,
        action: &str,
        target_type: &str,
        target_id: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Result<(), ModerationError> {
        self.deps
            .audit
            .record(AuditEvent::new(
                actor,
                action,
                target_type,
                target_id,
                "success",
                metadata,
            ))
            .await?;
        Ok(())
    }
}
