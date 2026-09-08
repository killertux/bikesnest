//! moderation service tests (pure, with fakes): report submission + rate
//! limit, the self-resolve guard, content hide/restore, parking invalidation and
//! proposal apply/reject. Persistence is faked; the orchestrating service rules
//! are what's under test.

use async_trait::async_trait;
use bikesnest_application::{
    AuditFilter, AuditLog, AuditLogReader, AuditPage, AuthenticatedUser, ContributionHistoryReader,
    ContributionItem, ModerationDeps, ModerationError, ModerationRepository, ModerationService,
    PhotoKind, Proposal, ProposalApplication, ProposalField, ProposalOverride, RateLimitError,
    RateLimiter, Report, ReportRepository, ReportTargetPreview,
};
use bikesnest_domain::{
    AccountState, ModerationLimits, ModerationState, ProposalKind, ProposalStatus, ProposedChange,
    ReportOutcome, ReportState, ReportTargetType, Role, UserEmail, UserId,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct FakeReports {
    next_id: Arc<std::sync::atomic::AtomicI64>,
    rows: Arc<Mutex<HashMap<i64, Report>>>,
    claim_fails: Arc<Mutex<bool>>,
    resolve_fails: Arc<Mutex<bool>>,
}

impl Default for FakeReports {
    fn default() -> Self {
        Self {
            next_id: Arc::new(std::sync::atomic::AtomicI64::new(100)),
            rows: Arc::new(Mutex::new(HashMap::new())),
            claim_fails: Arc::new(Mutex::new(false)),
            resolve_fails: Arc::new(Mutex::new(false)),
        }
    }
}

impl FakeReports {
    fn new() -> Self {
        Self::default()
    }
    fn fail_claim(self) -> Self {
        *self.claim_fails.lock().unwrap() = true;
        self
    }
}

#[async_trait]
impl ReportRepository for FakeReports {
    async fn create(&self, r: &bikesnest_application::NewReport) -> Result<i64, ModerationError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.rows.lock().unwrap().insert(
            id,
            Report {
                id,
                reporter_id: Some(r.reporter_id),
                target_type: r.target_type,
                target_id: r.target_id,
                reason: r.reason.clone(),
                description: Some(r.description.as_str().to_string()),
                state: ReportState::Open,
                claimed_by: None,
                resolved_by: None,
                resolution_note: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            },
        );
        Ok(id)
    }
    async fn list(
        &self,
        state: Option<ReportState>,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Report>, ModerationError> {
        let mut rows: Vec<Report> = self
            .rows
            .lock()
            .unwrap()
            .values()
            .filter(|r| state.is_none_or(|s| r.state == s))
            .filter(|r| after_id.is_none_or(|after| r.id > after))
            .cloned()
            .collect();
        rows.sort_by_key(|r| r.id);
        rows.truncate(limit.clamp(1, 200) as usize);
        Ok(rows)
    }
    async fn get(&self, id: i64) -> Result<Option<Report>, ModerationError> {
        Ok(self.rows.lock().unwrap().get(&id).cloned())
    }
    async fn claim(&self, id: i64, moderator: UserId) -> Result<(), ModerationError> {
        if *self.claim_fails.lock().unwrap() {
            return Err(ModerationError::InvalidState);
        }
        let mut rows = self.rows.lock().unwrap();
        let r = rows.get_mut(&id).ok_or(ModerationError::NotFound)?;
        r.state = ReportState::UnderReview;
        r.claimed_by = Some(moderator);
        Ok(())
    }
    async fn resolve(
        &self,
        id: i64,
        moderator: UserId,
        note: &str,
        outcome: ReportOutcome,
    ) -> Result<(), ModerationError> {
        if *self.resolve_fails.lock().unwrap() {
            return Err(ModerationError::InvalidState);
        }
        let mut rows = self.rows.lock().unwrap();
        let r = rows.get_mut(&id).ok_or(ModerationError::NotFound)?;
        r.state = match outcome {
            ReportOutcome::Resolved => ReportState::Resolved,
            ReportOutcome::Dismissed => ReportState::Dismissed,
        };
        r.resolved_by = Some(moderator);
        r.resolution_note = Some(note.to_string());
        Ok(())
    }
}

#[derive(Clone)]
struct FakeModeration {
    target_exists: Arc<Mutex<bool>>,
    hidden_reviews: Arc<Mutex<Vec<i64>>>,
    restored_reviews: Arc<Mutex<Vec<i64>>>,
    hidden_photos: Arc<Mutex<Vec<(PhotoKind, i64)>>>,
    restored_photos: Arc<Mutex<Vec<(PhotoKind, i64)>>>,
    parking_states: Arc<Mutex<HashMap<i64, ModerationState>>>,
    pending_proposals: Arc<Mutex<Vec<Proposal>>>,
    /// What `approve_proposal` was actually asked to apply — the merge rule's
    /// output, which is the thing under test.
    applied: Arc<Mutex<Vec<ProposalApplication>>>,
}

impl Default for FakeModeration {
    fn default() -> Self {
        Self {
            // Default to targets existing so happy-path submit tests pass.
            target_exists: Arc::new(Mutex::new(true)),
            hidden_reviews: Arc::new(Mutex::new(Vec::new())),
            restored_reviews: Arc::new(Mutex::new(Vec::new())),
            hidden_photos: Arc::new(Mutex::new(Vec::new())),
            restored_photos: Arc::new(Mutex::new(Vec::new())),
            parking_states: Arc::new(Mutex::new(HashMap::new())),
            pending_proposals: Arc::new(Mutex::new(Vec::new())),
            applied: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl ModerationRepository for FakeModeration {
    async fn target_exists(&self, _t: ReportTargetType, _id: i64) -> Result<bool, ModerationError> {
        Ok(*self.target_exists.lock().unwrap())
    }
    async fn report_previews(
        &self,
        targets: &[(ReportTargetType, i64)],
    ) -> Result<HashMap<(ReportTargetType, i64), ReportTargetPreview>, ModerationError> {
        Ok(targets
            .iter()
            .map(|&(t, id)| {
                (
                    (t, id),
                    ReportTargetPreview {
                        location_id: Some(id),
                        location_name: Some(format!("Loc {id}")),
                        ..Default::default()
                    },
                )
            })
            .collect())
    }
    async fn hide_review(&self, id: i64, _m: UserId) -> Result<(), ModerationError> {
        self.hidden_reviews.lock().unwrap().push(id);
        Ok(())
    }
    async fn restore_review(&self, id: i64, _m: UserId) -> Result<(), ModerationError> {
        self.restored_reviews.lock().unwrap().push(id);
        Ok(())
    }
    async fn hide_photo(
        &self,
        kind: PhotoKind,
        id: i64,
        _m: UserId,
    ) -> Result<(), ModerationError> {
        self.hidden_photos.lock().unwrap().push((kind, id));
        Ok(())
    }
    async fn restore_photo(
        &self,
        kind: PhotoKind,
        id: i64,
        _m: UserId,
    ) -> Result<(), ModerationError> {
        self.restored_photos.lock().unwrap().push((kind, id));
        Ok(())
    }
    async fn set_parking_state(
        &self,
        id: i64,
        _from: &[ModerationState],
        to: ModerationState,
        _m: UserId,
    ) -> Result<(), ModerationError> {
        self.parking_states.lock().unwrap().insert(id, to);
        Ok(())
    }
    async fn list_pending_proposals(
        &self,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Proposal>, ModerationError> {
        let mut props: Vec<Proposal> = self
            .pending_proposals
            .lock()
            .unwrap()
            .iter()
            .filter(|p| after_id.is_none_or(|after| p.id > after))
            .cloned()
            .collect();
        props.sort_by_key(|p| p.id);
        props.truncate(limit.clamp(1, 200) as usize);
        Ok(props)
    }
    async fn queue_counts(&self) -> Result<bikesnest_application::QueueCounts, ModerationError> {
        Ok(bikesnest_application::QueueCounts {
            pending_photos: 0,
            open_reports: 0,
            under_review_reports: 0,
            pending_proposals: self.pending_proposals.lock().unwrap().len() as i64,
        })
    }
    async fn get_proposal(&self, id: i64) -> Result<Option<Proposal>, ModerationError> {
        Ok(self
            .pending_proposals
            .lock()
            .unwrap()
            .iter()
            .find(|p| p.id == id)
            .cloned())
    }
    async fn approve_proposal(
        &self,
        id: i64,
        _m: UserId,
        applied: ProposalApplication,
    ) -> Result<(), ModerationError> {
        let mut props = self.pending_proposals.lock().unwrap();
        let p = props
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(ModerationError::NotFound)?;
        if p.status != ProposalStatus::Pending {
            return Err(ModerationError::InvalidState);
        }
        p.status = ProposalStatus::Approved;
        self.applied.lock().unwrap().push(applied);
        Ok(())
    }
    async fn reject_proposal(&self, id: i64, _m: UserId, _r: &str) -> Result<(), ModerationError> {
        let mut props = self.pending_proposals.lock().unwrap();
        let p = props
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(ModerationError::NotFound)?;
        if p.status != ProposalStatus::Pending {
            return Err(ModerationError::InvalidState);
        }
        p.status = ProposalStatus::Rejected;
        Ok(())
    }
}

#[derive(Clone, Default)]
struct FakeAuditReader;
#[async_trait]
impl AuditLogReader for FakeAuditReader {
    async fn list(&self, _f: AuditFilter) -> Result<AuditPage, bikesnest_application::AuditError> {
        Ok(AuditPage {
            items: Vec::new(),
            next_cursor: None,
        })
    }
}

#[derive(Clone, Default)]
struct FakeHistory;
#[async_trait]
impl ContributionHistoryReader for FakeHistory {
    async fn history(
        &self,
        _u: UserId,
        _after: Option<(chrono::DateTime<chrono::Utc>, i64)>,
        _limit: i64,
    ) -> Result<Vec<ContributionItem>, bikesnest_application::ContributionError> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
struct FakeRate {
    allow: bool,
}
#[async_trait]
impl RateLimiter for FakeRate {
    async fn check(&self, _k: &str, _l: u32, _w: Duration) -> Result<bool, RateLimitError> {
        Ok(self.allow)
    }
}

#[derive(Clone, Default)]
struct FakeAudit(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl AuditLog for FakeAudit {
    async fn record(
        &self,
        e: bikesnest_application::AuditEvent,
    ) -> Result<(), bikesnest_application::AuditError> {
        self.0.lock().unwrap().push(e.action);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn user(id: i64, roles: Vec<Role>) -> AuthenticatedUser {
    AuthenticatedUser {
        id: UserId(id),
        email: UserEmail::parse("u@example.com").unwrap(),
        display_name: None,
        account_state: AccountState::Active,
        is_verified: true,
        roles,
    }
}
fn moderator() -> AuthenticatedUser {
    user(2, vec![Role::Moderator])
}
fn reporter() -> AuthenticatedUser {
    user(1, vec![])
}

fn proposal(id: i64, kind: ProposalKind, change: ProposedChange) -> Proposal {
    Proposal {
        id,
        location_id: 10,
        location_name: "Test".to_string(),
        location_address: "1 Test St".to_string(),
        proposer_id: Some(UserId(3)),
        base_version: 1,
        location_version: 1,
        kind,
        change,
        reason: None,
        current_lat: Some(-25.0),
        current_lon: Some(-49.0),
        current_timezone: "America/Sao_Paulo".to_string(),
        current_state: ModerationState::Active,
        current_snapshot: serde_json::json!({}),
        status: ProposalStatus::Pending,
        created_at: chrono::Utc::now(),
    }
}

/// A move proposal the merge tests share: the proposer asked for this.
fn proposed_move() -> ProposedChange {
    ProposedChange::MoveLocation {
        lat: -25.5,
        lon: -49.5,
        timezone: Some("America/Sao_Paulo".to_string()),
    }
}

struct Harness {
    service: ModerationService,
    moderation: Arc<FakeModeration>,
    audit: Arc<FakeAudit>,
}

fn harness(allow_rate: bool) -> Harness {
    let reports = Arc::new(FakeReports::new());
    let moderation = Arc::new(FakeModeration::default());
    let audit = Arc::new(FakeAudit::default());
    let service = ModerationService::new(ModerationDeps {
        reports: Box::new(reports.as_ref().clone()),
        moderation: Box::new(moderation.as_ref().clone()),
        audit: Box::new(audit.as_ref().clone()),
        audit_reader: Box::new(FakeAuditReader),
        history: Box::new(FakeHistory),
        rate_limiter: Box::new(FakeRate { allow: allow_rate }),
        limits: ModerationLimits::default(),
    });
    Harness {
        service,
        moderation,
        audit,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn submit_report_happy_path_persists_open_and_audits() {
    let h = harness(true);
    h.moderation.target_exists.lock().unwrap().clone_from(&true);
    let id = h
        .service
        .submit_report(
            &reporter(),
            "1.2.3.4",
            ReportTargetType::Review,
            5,
            "spam",
            Some("bad".to_string()),
        )
        .await
        .unwrap();
    assert!(id > 0);
    let stored = h
        .service
        .get_report(&moderator(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, ReportState::Open);
    assert_eq!(stored.reason, "spam");
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"report.created".to_string())
    );
}

#[tokio::test]
async fn submit_report_rejects_unknown_reason() {
    let h = harness(true);
    h.moderation.target_exists.lock().unwrap().clone_from(&true);
    assert!(matches!(
        h.service
            .submit_report(
                &reporter(),
                "1.2.3.4",
                ReportTargetType::Parking,
                1,
                "bogus",
                None
            )
            .await,
        Err(ModerationError::InvalidReason)
    ));
}

#[tokio::test]
async fn submit_report_rejects_reason_not_allowed_for_target() {
    let h = harness(true);
    h.moderation.target_exists.lock().unwrap().clone_from(&true);
    // inappropriate_photo is not allowed on a parking location.
    assert!(matches!(
        h.service
            .submit_report(
                &reporter(),
                "1.2.3.4",
                ReportTargetType::Parking,
                1,
                "inappropriate_photo",
                None
            )
            .await,
        Err(ModerationError::InvalidReason)
    ));
}

#[tokio::test]
async fn submit_report_returns_target_not_found() {
    let h = harness(true);
    h.moderation
        .target_exists
        .lock()
        .unwrap()
        .clone_from(&false);
    assert!(matches!(
        h.service
            .submit_report(
                &reporter(),
                "1.2.3.4",
                ReportTargetType::Review,
                999,
                "spam",
                None
            )
            .await,
        Err(ModerationError::TargetNotFound)
    ));
}

#[tokio::test]
async fn submit_report_is_rate_limited() {
    let h = harness(false);
    h.moderation.target_exists.lock().unwrap().clone_from(&true);
    assert!(matches!(
        h.service
            .submit_report(
                &reporter(),
                "1.2.3.4",
                ReportTargetType::Parking,
                1,
                "spam",
                None
            )
            .await,
        Err(ModerationError::RateLimited)
    ));
}

#[tokio::test]
async fn resolve_own_report_is_self_resolve() {
    let h = harness(true);
    // A moderator is still the reporter → blocks at the self-resolve guard.
    let mod_reporter = user(1, vec![Role::Moderator]);
    let id = h
        .service
        .submit_report(
            &mod_reporter,
            "1.2.3.4",
            ReportTargetType::Parking,
            1,
            "spam",
            None,
        )
        .await
        .unwrap();
    let err = h
        .service
        .resolve_report(&mod_reporter, id, ReportOutcome::Resolved, "nope")
        .await
        .unwrap_err();
    assert!(matches!(err, ModerationError::SelfResolve));
}

#[tokio::test]
async fn claim_then_resolve_flow_audits_both() {
    let h = harness(true);
    let id = h
        .service
        .submit_report(
            &reporter(),
            "1.2.3.4",
            ReportTargetType::Parking,
            1,
            "spam",
            None,
        )
        .await
        .unwrap();
    h.service.claim_report(&moderator(), id).await.unwrap();
    h.service
        .resolve_report(&moderator(), id, ReportOutcome::Resolved, "removed listing")
        .await
        .unwrap();
    let stored = h
        .service
        .get_report(&moderator(), id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, ReportState::Resolved);
    assert_eq!(stored.resolved_by, Some(UserId(2)));
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"report.claimed".to_string())
    );
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"report.resolved".to_string())
    );
}

#[tokio::test]
async fn claim_wrong_state_is_invalid_state() {
    let service = ModerationService::new(ModerationDeps {
        reports: Box::new(FakeReports::new().fail_claim()),
        moderation: Box::new(FakeModeration::default()),
        audit: Box::new(FakeAudit::default()),
        audit_reader: Box::new(FakeAuditReader),
        history: Box::new(FakeHistory),
        rate_limiter: Box::new(FakeRate { allow: true }),
        limits: ModerationLimits::default(),
    });
    assert!(matches!(
        service.claim_report(&moderator(), 1).await,
        Err(ModerationError::InvalidState)
    ));
}

#[tokio::test]
async fn hide_and_restore_review_audits() {
    let h = harness(true);
    h.service.hide_review(&moderator(), 7).await.unwrap();
    h.service.restore_review(&moderator(), 7).await.unwrap();
    assert_eq!(h.moderation.hidden_reviews.lock().unwrap().as_slice(), &[7]);
    assert_eq!(
        h.moderation.restored_reviews.lock().unwrap().as_slice(),
        &[7]
    );
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"review.hidden".to_string())
    );
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"review.restored".to_string())
    );
}

#[tokio::test]
async fn invalidate_and_restore_parking_audits() {
    let h = harness(true);
    h.service
        .invalidate_parking(&moderator(), 10)
        .await
        .unwrap();
    // The fake only keeps the latest state (restore overwrites invalid), so we
    // assert on the audit trail + final state rather than both intermediate ones.
    h.service.restore_parking(&moderator(), 10).await.unwrap();
    let states = h.moderation.parking_states.lock().unwrap();
    assert_eq!(states.get(&10), Some(&ModerationState::Active));
    drop(states);
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"parking.invalidated".to_string())
    );
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"parking.restored".to_string())
    );
}

#[tokio::test]
async fn approve_and_reject_proposal() {
    let h = harness(true);
    h.moderation
        .pending_proposals
        .lock()
        .unwrap()
        .push(proposal(1, ProposalKind::MoveLocation, proposed_move()));
    h.moderation
        .pending_proposals
        .lock()
        .unwrap()
        .push(proposal(
            2,
            ProposalKind::ChangeExistence,
            ProposedChange::ChangeExistence { exists: false },
        ));

    h.service
        .approve_proposal(&moderator(), 1, ProposalOverride::default())
        .await
        .unwrap();
    h.service
        .reject_proposal(&moderator(), 2, "not right")
        .await
        .unwrap();

    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"proposal.approved".to_string())
    );
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"proposal.rejected".to_string())
    );
}

#[tokio::test]
async fn without_moderator_role_all_actions_are_unauthorized() {
    let h = harness(true);
    let plain = user(9, vec![]);
    assert!(matches!(
        h.service.claim_report(&plain, 1).await,
        Err(ModerationError::NotAuthorized)
    ));
    assert!(matches!(
        h.service.hide_review(&plain, 1).await,
        Err(ModerationError::NotAuthorized)
    ));
    assert!(matches!(
        h.service.invalidate_parking(&plain, 1).await,
        Err(ModerationError::NotAuthorized)
    ));
    assert!(matches!(
        h.service.list_pending_proposals(&plain, None, 50).await,
        Err(ModerationError::NotAuthorized)
    ));
    assert!(matches!(
        h.service.list_reports(&plain, None, None, 50).await,
        Err(ModerationError::NotAuthorized)
    ));
}

#[tokio::test]
async fn submission_has_no_role_gate() {
    // Reporting is open to any authenticated user (even unverified brand-new).
    let h = harness(true);
    h.moderation.target_exists.lock().unwrap().clone_from(&true);
    let fresh = user(77, vec![]);
    assert!(
        h.service
            .submit_report(
                &fresh,
                "9.9.9.9",
                ReportTargetType::Parking,
                1,
                "spam",
                None
            )
            .await
            .is_ok()
    );
}

// ---------------------------------------------------------------------------
// The override-merge rule. It used to live in the HTTP handler, where no
// test could reach it; these are the cases that rule has to get right.
// ---------------------------------------------------------------------------

/// Approve proposal 1 with `over` and return what the repository was handed.
async fn approve_move_with(
    change: ProposedChange,
    over: ProposalOverride,
) -> Result<ProposalApplication, ModerationError> {
    let h = harness(true);
    h.moderation
        .pending_proposals
        .lock()
        .unwrap()
        .push(proposal(1, ProposalKind::MoveLocation, change));
    h.service.approve_proposal(&moderator(), 1, over).await?;
    Ok(h.moderation.applied.lock().unwrap()[0].clone())
}

#[tokio::test]
async fn no_override_applies_exactly_what_the_proposer_asked_for() {
    let applied = approve_move_with(proposed_move(), ProposalOverride::default())
        .await
        .unwrap();
    assert_eq!(
        applied,
        ProposalApplication::MoveLocation {
            lat: -25.5,
            lon: -49.5,
            timezone: chrono_tz::America::Sao_Paulo,
        }
    );
}

#[tokio::test]
async fn an_override_wins_field_by_field() {
    // Only latitude is retyped: longitude and timezone stay the proposer's.
    let applied = approve_move_with(
        proposed_move(),
        ProposalOverride {
            lat: Some(-26.0),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        applied,
        ProposalApplication::MoveLocation {
            lat: -26.0,
            lon: -49.5,
            timezone: chrono_tz::America::Sao_Paulo,
        }
    );

    // All three retyped.
    let applied = approve_move_with(
        proposed_move(),
        ProposalOverride {
            lat: Some(38.72),
            lon: Some(-9.14),
            timezone: Some("Europe/Lisbon".to_string()),
            exists: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        applied,
        ProposalApplication::MoveLocation {
            lat: 38.72,
            lon: -9.14,
            timezone: chrono_tz::Europe::Lisbon,
        }
    );
}

#[tokio::test]
async fn an_unreadable_payload_needs_every_value_from_the_moderator() {
    // No override at all: the missing latitude is reported as a typed field
    // error, not as an English message assembled in the core.
    let err = approve_move_with(ProposedChange::Unknown, ProposalOverride::default())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ModerationError::InvalidProposalField(ProposalField::Lat)
        ),
        "expected InvalidProposalField(Lat), got {err:?}"
    );

    // Latitude only: longitude is now the missing one.
    let err = approve_move_with(
        ProposedChange::Unknown,
        ProposalOverride {
            lat: Some(-25.0),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            ModerationError::InvalidProposalField(ProposalField::Lon)
        ),
        "expected InvalidProposalField(Lon), got {err:?}"
    );

    // Coordinates but no timezone.
    let err = approve_move_with(
        ProposedChange::Unknown,
        ProposalOverride {
            lat: Some(-25.0),
            lon: Some(-49.0),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(
            err,
            ModerationError::InvalidProposalField(ProposalField::Timezone)
        ),
        "expected InvalidProposalField(Timezone), got {err:?}"
    );

    // Everything supplied: the moderator can still push it through.
    let applied = approve_move_with(
        ProposedChange::Unknown,
        ProposalOverride {
            lat: Some(-25.0),
            lon: Some(-49.0),
            timezone: Some("America/Sao_Paulo".to_string()),
            exists: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(
        applied,
        ProposalApplication::MoveLocation {
            lat: -25.0,
            lon: -49.0,
            timezone: chrono_tz::America::Sao_Paulo,
        }
    );
}

#[tokio::test]
async fn out_of_range_and_unknown_timezone_overrides_are_refused() {
    for (over, field) in [
        (
            ProposalOverride {
                lat: Some(120.0),
                ..Default::default()
            },
            ProposalField::Lat,
        ),
        (
            ProposalOverride {
                lon: Some(-400.0),
                ..Default::default()
            },
            ProposalField::Lon,
        ),
        (
            ProposalOverride {
                timezone: Some("Mars/Olympus".to_string()),
                ..Default::default()
            },
            ProposalField::Timezone,
        ),
    ] {
        let err = approve_move_with(proposed_move(), over).await.unwrap_err();
        assert!(
            matches!(err, ModerationError::InvalidProposalField(f) if f == field),
            "expected InvalidProposalField({field:?}), got {err:?}"
        );
    }
}

#[tokio::test]
async fn existence_override_flips_the_proposers_choice() {
    let h = harness(true);
    h.moderation
        .pending_proposals
        .lock()
        .unwrap()
        .push(proposal(
            1,
            ProposalKind::ChangeExistence,
            ProposedChange::ChangeExistence { exists: false },
        ));
    // The moderator disagrees: the spot is still there.
    h.service
        .approve_proposal(
            &moderator(),
            1,
            ProposalOverride {
                exists: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        h.moderation.applied.lock().unwrap()[0],
        ProposalApplication::ChangeExistence { exists: true }
    );
}

#[tokio::test]
async fn an_unreadable_existence_payload_needs_an_explicit_choice() {
    let h = harness(true);
    h.moderation
        .pending_proposals
        .lock()
        .unwrap()
        .push(proposal(
            1,
            ProposalKind::ChangeExistence,
            ProposedChange::Unknown,
        ));
    let err = h
        .service
        .approve_proposal(&moderator(), 1, ProposalOverride::default())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            ModerationError::InvalidProposalField(ProposalField::Existence)
        ),
        "expected InvalidProposalField(Existence), got {err:?}"
    );
}

#[tokio::test]
async fn approving_a_missing_proposal_is_not_found() {
    let h = harness(true);
    let err = h
        .service
        .approve_proposal(&moderator(), 999, ProposalOverride::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, ModerationError::NotFound),
        "expected NotFound, got {err:?}"
    );
}

#[tokio::test]
async fn a_proposal_is_stale_when_the_location_moved_on() {
    let fresh = proposal(1, ProposalKind::MoveLocation, proposed_move());
    assert!(!fresh.is_stale());
    let stale = Proposal {
        location_version: 5,
        ..proposal(2, ProposalKind::MoveLocation, proposed_move())
    };
    assert!(stale.is_stale(), "base_version 1 against a v5 location");
}

#[tokio::test]
async fn report_previews_are_fetched_once_for_the_whole_page() {
    let h = harness(true);
    let reports = vec![
        report_row(1, ReportTargetType::Parking, 10),
        report_row(2, ReportTargetType::Parking, 10),
        report_row(3, ReportTargetType::Review, 77),
    ];
    let previews = h
        .service
        .report_previews(&moderator(), &reports)
        .await
        .unwrap();
    // Two reports on the same target ask for one preview, not two.
    assert_eq!(previews.len(), 2);
    assert_eq!(
        previews[&(ReportTargetType::Parking, 10)]
            .location_name
            .as_deref(),
        Some("Loc 10")
    );

    // The role gate applies to the preview lookup as much as to the list.
    assert!(matches!(
        h.service.report_previews(&user(9, vec![]), &reports).await,
        Err(ModerationError::NotAuthorized)
    ));
}

fn report_row(id: i64, target_type: ReportTargetType, target_id: i64) -> Report {
    Report {
        id,
        reporter_id: Some(UserId(3)),
        target_type,
        target_id,
        reason: "spam".to_string(),
        description: None,
        state: ReportState::Open,
        claimed_by: None,
        resolved_by: None,
        resolution_note: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}
