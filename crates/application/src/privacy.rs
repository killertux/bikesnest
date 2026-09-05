//! Privacy and account lifecycle use cases.
//!
//! Ports + read models + [`PrivacyService`] + [`RetentionJob`]. Infrastructure
//! implements the ports; the web layer calls the service for every export /
//! deletion / rights-request / retention action.
//!
//! Design decisions are documented alongside the privacy policy:
//! - **Deletion is anonymize-in-place** — the `users` row survives with PII
//!   scrubbed; community content is retained *unattributed*, never hard-deleted.
//! - **The export payload is a versioned JSON document** (`schema_version: 1`);
//!   credential/session/token hashes and audit rows are excluded by construction.
//! - **The download link is owner-only + single-use + expiring** (two gates).

use crate::audit::{AuditEvent, AuditLog};
use crate::auth::{
    AccountRepository, AuthError, AuthenticatedUser, Clock, PasswordHasher, SessionStore,
    TokenGenerator,
};
use async_trait::async_trait;
use bikesnest_domain::{
    AuthenticationProvider, ExportState, Password, PolicyKind, PrivacyRequestKind,
    PrivacyRequestState, Role, UserId,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PrivacyError {
    #[error("you are not permitted to perform this action")]
    NotAuthorized,
    #[error("not found")]
    NotFound,
    #[error("you cannot delete the last administrator")]
    LastAdmin,
    #[error("re-authentication required")]
    ReauthRequired,
    #[error("invalid download token")]
    InvalidToken,
    #[error("this export has expired")]
    Expired,
    #[error("this export has already been downloaded")]
    AlreadyDownloaded,
    #[error("invalid request kind for this flow")]
    InvalidKind,
    #[error("invalid input: {0}")]
    InvalidField(String),
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

impl From<AuthError> for PrivacyError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::InvalidCredentials => PrivacyError::ReauthRequired,
            _ => PrivacyError::Internal,
        }
    }
}

impl From<crate::audit::AuditError> for PrivacyError {
    fn from(_: crate::audit::AuditError) -> Self {
        PrivacyError::Internal
    }
}

impl From<bikesnest_domain::DomainError> for PrivacyError {
    fn from(e: bikesnest_domain::DomainError) -> Self {
        PrivacyError::InvalidField(e.to_string())
    }
}

impl From<crate::ports::ReaderError> for PrivacyError {
    fn from(_: crate::ports::ReaderError) -> Self {
        PrivacyError::Internal
    }
}

// ---------------------------------------------------------------------------
// Export payload — a versioned, machine-readable document
// ---------------------------------------------------------------------------

/// The versioned personal-data export document. `schema_version: 1`.
/// Excludes credential/session/token hashes, CSRF tokens and audit rows by
/// construction — the infrastructure assemblers simply never select them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPayload {
    pub schema_version: u32,
    pub exported_at: DateTime<Utc>,
    pub account: ExportAccount,
    pub authentication: Vec<ExportProvider>,
    pub sessions: Vec<ExportSession>,
    pub favorites: Vec<ExportFavorite>,
    pub reviews: Vec<ExportReview>,
    pub verifications: Vec<ExportVerification>,
    pub proposals: Vec<ExportProposal>,
    pub proposal_votes: Vec<ExportProposalVote>,
    pub reports: Vec<ExportReport>,
    pub photos: Vec<ExportPhoto>,
}

impl ExportPayload {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        account: ExportAccount,
        authentication: Vec<ExportProvider>,
        sessions: Vec<ExportSession>,
        favorites: Vec<ExportFavorite>,
        reviews: Vec<ExportReview>,
        verifications: Vec<ExportVerification>,
        proposals: Vec<ExportProposal>,
        proposal_votes: Vec<ExportProposalVote>,
        reports: Vec<ExportReport>,
        photos: Vec<ExportPhoto>,
        exported_at: DateTime<Utc>,
    ) -> Self {
        Self {
            schema_version: 1,
            exported_at,
            account,
            authentication,
            sessions,
            favorites,
            reviews,
            verifications,
            proposals,
            proposal_votes,
            reports,
            photos,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportAccount {
    pub user_id: i64,
    pub email: String,
    pub display_name: Option<String>,
    #[serde(default)]
    pub public_contribution_name: bool,
    #[serde(default)]
    pub public_contribution_name_updated_at: Option<DateTime<Utc>>,
    pub account_state: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportProvider {
    pub provider: String,
    /// Provider subject (e.g. Google `sub`) — **never** `credential_hash`.
    pub subject: String,
    /// True when the provider asserted a verified email at link time.
    pub email_verified: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportSession {
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportFavorite {
    pub location_id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportReview {
    pub id: i64,
    pub location_id: i64,
    pub rating: i16,
    pub body: String,
    #[serde(default)]
    pub public_author: bool,
    pub moderation_state: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub revisions: Vec<ExportReviewRevision>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportReviewRevision {
    pub rating: i16,
    pub body: String,
    pub edited_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportVerification {
    pub location_id: i64,
    pub kind: String,
    pub result: String,
    pub attribute_code: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportProposal {
    pub location_id: i64,
    pub base_version: i64,
    pub kind: String,
    pub proposed: serde_json::Value,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportProposalVote {
    pub proposal_id: i64,
    pub vote: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportReport {
    pub target_type: String,
    pub target_id: i64,
    pub reason: String,
    pub description: Option<String>,
    pub state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportPhoto {
    /// `"parking"` | `"review"`.
    pub kind: String,
    pub location_id: Option<i64>,
    pub review_id: Option<i64>,
    pub storage_key: String,
    pub thumbnail_key: Option<String>,
    pub content_type: Option<String>,
    pub moderation_state: String,
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Ports: export read/write
// ---------------------------------------------------------------------------

/// A new export to persist. `token` is the raw 32-byte download token; the
/// infrastructure stores only its SHA-256 hash and compares in constant time.
pub struct NewExport {
    pub user_id: UserId,
    pub token: [u8; 32],
    pub payload: ExportPayload,
    pub expires_at: DateTime<Utc>,
}

/// An export row as listed on C7 (no payload — never rendered inline).
#[derive(Debug, Clone)]
pub struct Export {
    pub id: i64,
    pub user_id: UserId,
    pub state: ExportState,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub downloaded_at: Option<DateTime<Utc>>,
}

/// The payload returned only on a successful single-use download.
#[derive(Debug)]
pub struct ExportDownload {
    pub payload: ExportPayload,
}

#[async_trait]
pub trait ExportRepository: Send + Sync {
    /// Assemble the full versioned payload for a user across all tables.
    async fn assemble_payload(&self, user_id: UserId) -> Result<ExportPayload, PrivacyError>;
    async fn create(&self, e: &NewExport) -> Result<i64, PrivacyError>;
    async fn list_for_user(&self, user_id: UserId) -> Result<Vec<Export>, PrivacyError>;
    async fn get(&self, id: i64) -> Result<Option<Export>, PrivacyError>;
    /// Validate the token (constant-time SHA-256 compare) + not expired + not
    /// downloaded; mark `DOWNLOADED` on success and return the payload once.
    async fn consume_download(
        &self,
        id: i64,
        token: &[u8; 32],
        now: DateTime<Utc>,
    ) -> Result<ExportDownload, PrivacyError>;
    /// Delete `state = 'READY' AND expires_at < now()` rows. Returns count.
    async fn purge_expired(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError>;
}

// ---------------------------------------------------------------------------
// Ports: manual rights workflow
// ---------------------------------------------------------------------------

pub struct NewPrivacyRequest {
    pub user_id: UserId,
    pub kind: PrivacyRequestKind,
    pub details: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct PrivacyRequest {
    pub id: i64,
    pub user_id: Option<UserId>,
    pub kind: PrivacyRequestKind,
    pub state: PrivacyRequestState,
    pub details: serde_json::Value,
    pub fulfilled_by: Option<UserId>,
    pub fulfilled_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[async_trait]
pub trait PrivacyRequestRepository: Send + Sync {
    async fn create(&self, r: &NewPrivacyRequest) -> Result<i64, PrivacyError>;
    /// Optional state filter; ordered by `created_at`.
    async fn list(
        &self,
        state: Option<PrivacyRequestState>,
    ) -> Result<Vec<PrivacyRequest>, PrivacyError>;
    async fn get(&self, id: i64) -> Result<Option<PrivacyRequest>, PrivacyError>;
    /// `OPEN|IN_PROGRESS → COMPLETED`, setting `fulfilled_by` (`None` = automated)
    /// and `fulfilled_at`. 0 rows → `NotFound`/`InvalidKind`.
    async fn fulfill(&self, id: i64, by: Option<UserId>) -> Result<(), PrivacyError>;
}

// ---------------------------------------------------------------------------
// Ports: anonymize-in-place
// ---------------------------------------------------------------------------

/// Per-table row counts from one anonymization transaction. The counts let both
/// tests and the audit trail assert completeness (a partial apply where PII is
/// scrubbed but a contribution is still attributed is the main correctness
/// hazard).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnonymizationReport {
    pub identities: u64,
    pub roles: u64,
    /// `user_roles.granted_by` rows that named this account (roles it granted
    /// to *other* people) and were nulled.
    pub roles_granted_by_anonymized: u64,
    pub sessions: u64,
    pub email_verification_tokens: u64,
    pub password_reset_tokens: u64,
    pub favorites: u64,
    pub parked_here: u64,
    pub exports: u64,
    pub consent_records: u64,
    pub reviews_anonymized: u64,
    pub verifications_anonymized: u64,
    pub proposals_anonymized: u64,
    pub reports_anonymized: u64,
    pub locations_anonymized: u64,
    pub revisions_anonymized: u64,
    pub parking_photos_anonymized: u64,
    pub review_photos_anonymized: u64,
    pub audit_events_anonymized: u64,
    /// Audit rows whose `target_id` held the account's email (failed logins,
    /// which have no user id to record) and were rewritten to the anonymized
    /// form.
    pub audit_targets_anonymized: u64,
    pub privacy_requests_anonymized: u64,
}

#[async_trait]
pub trait AnonymizationRepository: Send + Sync {
    /// True when `user_id` is the sole remaining ADMIN (last-admin guard).
    async fn is_last_admin(&self, user_id: UserId) -> Result<bool, PrivacyError>;
    /// One transaction: scrub the `users` row, delete private activity, and
    /// NULL every attribution column across community tables. Returns counts.
    async fn anonymize(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
    ) -> Result<AnonymizationReport, PrivacyError>;
}

// ---------------------------------------------------------------------------
// Ports: retention job
// ---------------------------------------------------------------------------

#[async_trait]
pub trait RetentionRepository: Send + Sync {
    async fn purge_expired_password_reset_tokens(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, PrivacyError>;
    async fn purge_expired_email_verification_tokens(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, PrivacyError>;
    async fn purge_expired_sessions(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError>;
    async fn purge_expired_parked_here(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError>;
    async fn purge_expired_exports(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError>;
    /// Delete orphaned upload objects older than the orphan TTL (media sweep).
    /// Returns the number of objects removed. A store that cannot be listed is
    /// an error, never a quiet zero.
    async fn purge_orphan_uploads(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError>;
    /// Delete `PENDING_REVIEW` photo rows older than the reconciliation grace
    /// period whose stored object does not exist — a row that can never render.
    /// Returns the number of rows removed.
    async fn reconcile_pending_photos(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError>;
    /// Anonymize accounts inactive before `cutoff` (config-gated; TTL=0 → caller
    /// must not invoke). Returns the number of accounts anonymized.
    async fn anonymize_inactive_accounts(&self, cutoff: DateTime<Utc>)
    -> Result<u64, PrivacyError>;
    /// Hard-purge the shell of accounts deleted before `cutoff` (config-gated).
    /// Returns the number of shells purged.
    async fn purge_deleted_accounts(&self, cutoff: DateTime<Utc>) -> Result<u64, PrivacyError>;
}

// ---------------------------------------------------------------------------
// Ports: versioned legal pages
// ---------------------------------------------------------------------------

/// Locale code stored in `policy_version.locale` (BCP 47 form, as used for
/// `<html lang>`): `"pt-BR"` (default/fallback) or `"en"`.
pub const POLICY_FALLBACK_LOCALE: &str = "pt-BR";

/// One `policy_version` row, as read by the public legal pages.
#[derive(Debug, Clone)]
pub struct PolicyDocument {
    pub id: i64,
    pub kind: PolicyKind,
    /// `"pt-BR"` | `"en"` — see [`POLICY_FALLBACK_LOCALE`].
    pub locale: String,
    pub version: String,
    pub effective_at: DateTime<Utc>,
    pub superseded_at: Option<DateTime<Utc>>,
    /// Markdown content. The web layer renders it through its own markdown
    /// renderer, which escapes any raw HTML in the source; templates
    /// never mark the stored text itself as safe.
    pub content: String,
}

#[async_trait]
pub trait PolicyReader: Send + Sync {
    /// The current version for `locale` (latest `effective_at`,
    /// `superseded_at IS NULL`). `None` when that locale has no document —
    /// callers fall back to [`POLICY_FALLBACK_LOCALE`].
    async fn current(
        &self,
        kind: PolicyKind,
        locale: &str,
    ) -> Result<Option<PolicyDocument>, PrivacyError>;
    /// All versions for `locale`, newest first (for the version-history page).
    async fn history(
        &self,
        kind: PolicyKind,
        locale: &str,
    ) -> Result<Vec<PolicyDocument>, PrivacyError>;
}

// ---------------------------------------------------------------------------
// Manual-rights set: the non-self-serve kinds recorded for operators.
// ---------------------------------------------------------------------------

/// Kinds that go through the manual operator-fulfilled queue rather than the
/// automatic export/deletion flows.
pub const MANUAL_REQUEST_KINDS: &[PrivacyRequestKind] = &[
    PrivacyRequestKind::Rectification,
    PrivacyRequestKind::Restriction,
    PrivacyRequestKind::Objection,
    PrivacyRequestKind::ConsentWithdrawal,
];

// ---------------------------------------------------------------------------
// PrivacyService
// ---------------------------------------------------------------------------

/// Everything the privacy use cases depend on, bundled for construction.
#[allow(clippy::too_many_arguments)]
pub struct PrivacyDeps {
    pub exports: Box<dyn ExportRepository>,
    pub requests: Box<dyn PrivacyRequestRepository>,
    pub anonymization: Box<dyn AnonymizationRepository>,
    pub accounts: Box<dyn AccountRepository>,
    pub sessions: Box<dyn SessionStore>,
    pub audit: Box<dyn AuditLog>,
    pub hasher: Box<dyn PasswordHasher>,
    pub tokens_gen: Box<dyn TokenGenerator>,
    pub clock: Box<dyn Clock>,
}

pub struct PrivacyService {
    deps: PrivacyDeps,
}

fn b64url_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn b64url_decode(s: &str) -> Option<[u8; 32]> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(arr)
}

impl PrivacyService {
    pub fn new(deps: PrivacyDeps) -> Self {
        Self { deps }
    }

    fn now(&self) -> DateTime<Utc> {
        self.deps.clock.now()
    }

    fn require_admin(&self, actor: &AuthenticatedUser) -> Result<(), PrivacyError> {
        if actor.has_role(Role::Admin) {
            Ok(())
        } else {
            Err(PrivacyError::NotAuthorized)
        }
    }

    async fn audit(
        &self,
        actor: Option<UserId>,
        action: &str,
        target_type: &str,
        target_id: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Result<(), PrivacyError> {
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

    // -----------------------------------------------------------------------
    // Export / access right
    // -----------------------------------------------------------------------

    /// Request a personal-data export: assemble the payload, mint a single-use
    /// token, store the export as `READY` (24h TTL), audit. Returns the id, the
    /// owner-only token and the expiry so the web layer can render the link.
    pub async fn request_export(
        &self,
        user: &AuthenticatedUser,
    ) -> Result<ExportRequested, PrivacyError> {
        let now = self.now();
        let payload = self.deps.exports.assemble_payload(user.id).await?;
        let token: [u8; 32] = self.deps.tokens_gen.generate();
        let token_str = b64url_encode(&token);
        let expires_at = now + Duration::hours(24);
        let id = self
            .deps
            .exports
            .create(&NewExport {
                user_id: user.id,
                token,
                payload,
                expires_at,
            })
            .await?;
        self.audit(
            Some(user.id),
            "privacy.export_requested",
            "personal_data_export",
            id.to_string(),
            serde_json::json!({ "expires_at": expires_at }),
        )
        .await?;
        Ok(ExportRequested {
            id,
            token: token_str,
            created_at: now,
            expires_at,
        })
    }

    /// List a user's own exports (C7 status). Owner-only.
    pub async fn list_exports(
        &self,
        user: &AuthenticatedUser,
    ) -> Result<Vec<Export>, PrivacyError> {
        self.deps.exports.list_for_user(user.id).await
    }

    /// Download an export. Two independent gates: (a) the authenticated **owner**
    /// session, and (b) an unexpired, single-use token. `/account/export/{id}`
    /// and the download are both owner-scoped; the token prevents replay.
    pub async fn download_export(
        &self,
        user: &AuthenticatedUser,
        id: i64,
        raw_token: &str,
    ) -> Result<ExportDownload, PrivacyError> {
        let export = self
            .deps
            .exports
            .get(id)
            .await?
            .ok_or(PrivacyError::NotFound)?;
        if export.user_id != user.id {
            return Err(PrivacyError::NotAuthorized);
        }
        let token = b64url_decode(raw_token).ok_or(PrivacyError::InvalidToken)?;
        let now = self.now();
        let download = self.deps.exports.consume_download(id, &token, now).await?;
        self.audit(
            Some(user.id),
            "privacy.export_downloaded",
            "personal_data_export",
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(download)
    }

    // -----------------------------------------------------------------------
    // Deletion / anonymization
    // -----------------------------------------------------------------------

    /// Request account deletion: re-authenticate, last-admin guard, create the
    /// rights request, anonymize-in-place (one tx), complete the request, audit,
    /// and revoke every session.
    ///
    /// `password` is required for password accounts (verified against the stored
    /// hash); `confirm_email` is always required (both account types must type
    /// the account email). OAuth-only accounts rely on the active session as the
    /// second factor (it is already 2FA'd upstream at Google).
    pub async fn request_deletion(
        &self,
        user: &AuthenticatedUser,
        password: Option<&str>,
        confirm_email: &str,
    ) -> Result<(), PrivacyError> {
        // 1) Re-authentication.
        if confirm_email.trim().to_lowercase() != user.email.as_str() {
            return Err(PrivacyError::ReauthRequired);
        }
        let identity = self
            .deps
            .accounts
            .find_identity(AuthenticationProvider::Password, user.email.as_str())
            .await?;
        if let Some(hash) = identity.and_then(|i| i.credential_hash) {
            let pw = password.ok_or(PrivacyError::ReauthRequired)?;
            if !self.deps.hasher.verify(&Password::new(pw), &hash).await? {
                return Err(PrivacyError::ReauthRequired);
            }
        }

        // 2) Last-admin guard: the sole ADMIN cannot delete themselves.
        if self.deps.anonymization.is_last_admin(user.id).await? {
            return Err(PrivacyError::LastAdmin);
        }

        let now = self.now();

        // 3) Record the request (evidence before the destructive change).
        let request_id = self
            .deps
            .requests
            .create(&NewPrivacyRequest {
                user_id: user.id,
                kind: PrivacyRequestKind::Deletion,
                details: serde_json::json!({}),
            })
            .await?;
        self.audit(
            Some(user.id),
            "account.deletion_requested",
            "user",
            user.id.0.to_string(),
            serde_json::json!({}),
        )
        .await?;

        // 4) Anonymize-in-place (one transaction).
        self.deps.anonymization.anonymize(user.id, now).await?;

        // 5) Close the now-anonymized request (automated → `fulfilled_by = NULL`).
        self.deps.requests.fulfill(request_id, None).await?;
        self.audit(
            Some(user.id),
            "account.anonymized",
            "user",
            user.id.0.to_string(),
            serde_json::json!({}),
        )
        .await?;

        // 6) Invalidate every session so the deletion takes effect immediately.
        self.deps.sessions.revoke_all_for_user(user.id).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Manual rights requests
    // -----------------------------------------------------------------------

    /// Submit a manual rights request (rectification/restriction/objection/
    /// consent-withdrawal). The automatic flows (export/deletion) are separate.
    pub async fn submit_request(
        &self,
        user: &AuthenticatedUser,
        kind: PrivacyRequestKind,
        details: serde_json::Value,
    ) -> Result<i64, PrivacyError> {
        if !MANUAL_REQUEST_KINDS.contains(&kind) {
            return Err(PrivacyError::InvalidKind);
        }
        let id = self
            .deps
            .requests
            .create(&NewPrivacyRequest {
                user_id: user.id,
                kind,
                details,
            })
            .await?;
        self.audit(
            Some(user.id),
            "privacy.request_created",
            "privacy_request",
            id.to_string(),
            serde_json::json!({ "kind": kind.as_code() }),
        )
        .await?;
        Ok(id)
    }

    /// The admin privacy-request queue (optional state filter). ADMIN-only.
    pub async fn list_requests(
        &self,
        actor: &AuthenticatedUser,
        state: Option<PrivacyRequestState>,
    ) -> Result<Vec<PrivacyRequest>, PrivacyError> {
        self.require_admin(actor)?;
        self.deps.requests.list(state).await
    }

    /// Fulfill a manual request (`OPEN|IN_PROGRESS → COMPLETED`). ADMIN-only.
    pub async fn fulfill_request(
        &self,
        actor: &AuthenticatedUser,
        id: i64,
    ) -> Result<(), PrivacyError> {
        self.require_admin(actor)?;
        self.deps.requests.fulfill(id, Some(actor.id)).await?;
        self.audit(
            Some(actor.id),
            "privacy.request_fulfilled",
            "privacy_request",
            id.to_string(),
            serde_json::json!({}),
        )
        .await?;
        Ok(())
    }
}

/// The returned handle for a newly requested export (C7 renders the link).
#[derive(Debug, Clone)]
pub struct ExportRequested {
    pub id: i64,
    pub token: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Retention job
// ---------------------------------------------------------------------------

/// Config-gated retention knobs. Zero (the default) disables the corresponding
/// step — the retention periods are legal/product decisions, not engineering
/// ones, so they default to "off" until approved.
#[derive(Debug, Clone, Copy, Default)]
pub struct RetentionConfig {
    pub inactive_account_anonymize_after_days: u32,
    pub deleted_account_purge_after_days: u32,
}

/// One retention step's result.
#[derive(Debug, Clone)]
pub struct RetentionStep {
    pub name: String,
    pub purged: u64,
}

/// The whole-run result (for the CLI output + audit metadata).
#[derive(Debug, Clone)]
pub struct RetentionSummary {
    pub steps: Vec<RetentionStep>,
}

pub struct RetentionJob {
    retention: Box<dyn RetentionRepository>,
    audit: Box<dyn AuditLog>,
    clock: Box<dyn Clock>,
    config: RetentionConfig,
}

impl RetentionJob {
    pub fn new(
        retention: Box<dyn RetentionRepository>,
        audit: Box<dyn AuditLog>,
        clock: Box<dyn Clock>,
        config: RetentionConfig,
    ) -> Self {
        Self {
            retention,
            audit,
            clock,
            config,
        }
    }

    /// Run every purge step. The seven default steps always run; the two
    /// config-gated steps are skipped when their TTL is `0`. Idempotent: every
    /// purge is a `DELETE WHERE expires_at < now()` so a re-run is a no-op.
    pub async fn run(&self) -> Result<RetentionSummary, PrivacyError> {
        let now = self.clock.now();
        let mut counts: Vec<(&str, u64)> = vec![
            (
                "password_reset_tokens",
                self.retention
                    .purge_expired_password_reset_tokens(now)
                    .await?,
            ),
            (
                "email_verification_tokens",
                self.retention
                    .purge_expired_email_verification_tokens(now)
                    .await?,
            ),
            (
                "sessions",
                self.retention.purge_expired_sessions(now).await?,
            ),
            (
                "parked_here",
                self.retention.purge_expired_parked_here(now).await?,
            ),
            ("exports", self.retention.purge_expired_exports(now).await?),
            (
                "orphan_uploads",
                self.retention.purge_orphan_uploads(now).await?,
            ),
            (
                "pending_photos",
                self.retention.reconcile_pending_photos(now).await?,
            ),
        ];

        if self.config.inactive_account_anonymize_after_days > 0 {
            let cutoff =
                now - Duration::days(self.config.inactive_account_anonymize_after_days as i64);
            counts.push((
                "inactive_accounts",
                self.retention.anonymize_inactive_accounts(cutoff).await?,
            ));
        }
        if self.config.deleted_account_purge_after_days > 0 {
            let cutoff = now - Duration::days(self.config.deleted_account_purge_after_days as i64);
            counts.push((
                "deleted_accounts",
                self.retention.purge_deleted_accounts(cutoff).await?,
            ));
        }

        let mut step_map = serde_json::Map::new();
        for (n, c) in &counts {
            step_map.insert((*n).to_string(), serde_json::json!(*c));
        }
        let metadata = serde_json::json!({ "steps": step_map });
        self.audit
            .record(AuditEvent::new(
                None,
                "retention.purged",
                "system",
                "retention",
                "success",
                metadata,
            ))
            .await?;

        Ok(RetentionSummary {
            steps: counts
                .iter()
                .map(|(n, c)| RetentionStep {
                    name: n.to_string(),
                    purged: *c,
                })
                .collect(),
        })
    }
}

// ---------------------------------------------------------------------------
// Tests (with fakes)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthenticatedUser, IdentityRecord, Session};
    use bikesnest_domain::{AccountState, CsrfToken, SessionId};

    // Fixed actor for service-level tests.
    fn actor() -> AuthenticatedUser {
        AuthenticatedUser {
            id: UserId(1),
            email: bikesnest_domain::UserEmail::parse("a@example.com").unwrap(),
            display_name: None,
            account_state: AccountState::Active,
            is_verified: true,
            roles: vec![Role::User],
        }
    }

    fn empty_payload() -> ExportPayload {
        ExportPayload::new(
            ExportAccount {
                user_id: 1,
                email: "a@example.com".to_string(),
                display_name: None,
                public_contribution_name: false,
                public_contribution_name_updated_at: None,
                account_state: "ACTIVE".to_string(),
                email_verified_at: None,
                created_at: Utc::now(),
                roles: vec![],
            },
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            Utc::now(),
        )
    }

    #[allow(clippy::type_complexity)]
    struct FakeExportRepo {
        stored: std::sync::Mutex<
            std::collections::HashMap<i64, (Vec<u8>, ExportPayload, DateTime<Utc>)>,
        >,
        downloaded: std::sync::Mutex<bool>,
        next_id: std::sync::Mutex<i64>,
    }

    impl FakeExportRepo {
        fn new() -> Self {
            Self {
                stored: Default::default(),
                downloaded: std::sync::Mutex::new(false),
                next_id: std::sync::Mutex::new(1),
            }
        }
    }

    #[async_trait]
    impl ExportRepository for FakeExportRepo {
        async fn assemble_payload(&self, _u: UserId) -> Result<ExportPayload, PrivacyError> {
            Ok(empty_payload())
        }
        async fn create(&self, e: &NewExport) -> Result<i64, PrivacyError> {
            let mut stored = self.stored.lock().unwrap();
            let mut next = self.next_id.lock().unwrap();
            stored.insert(*next, (e.token.to_vec(), e.payload.clone(), e.expires_at));
            let id = *next;
            *next += 1;
            Ok(id)
        }
        async fn list_for_user(&self, _u: UserId) -> Result<Vec<Export>, PrivacyError> {
            Ok(vec![])
        }
        async fn get(&self, id: i64) -> Result<Option<Export>, PrivacyError> {
            let stored = self.stored.lock().unwrap();
            let Some((_, _, expires)) = stored.get(&id) else {
                return Ok(None);
            };
            let downloaded = *self.downloaded.lock().unwrap();
            Ok(Some(Export {
                id,
                user_id: UserId(1),
                state: if downloaded {
                    ExportState::Downloaded
                } else {
                    ExportState::Ready
                },
                created_at: Utc::now(),
                expires_at: *expires,
                downloaded_at: None,
            }))
        }
        async fn consume_download(
            &self,
            id: i64,
            token: &[u8; 32],
            now: DateTime<Utc>,
        ) -> Result<ExportDownload, PrivacyError> {
            let stored = self.stored.lock().unwrap();
            let Some((stored, payload, expires)) = stored.get(&id) else {
                return Err(PrivacyError::NotFound);
            };
            if *self.downloaded.lock().unwrap() {
                return Err(PrivacyError::AlreadyDownloaded);
            }
            if now > *expires {
                return Err(PrivacyError::Expired);
            }
            if stored != token {
                return Err(PrivacyError::InvalidToken);
            }
            Ok(ExportDownload {
                payload: payload.clone(),
            })
        }
        async fn purge_expired(&self, _n: DateTime<Utc>) -> Result<u64, PrivacyError> {
            Ok(0)
        }
    }

    struct FakeRequestRepo {
        rows: std::sync::Mutex<std::collections::HashMap<i64, PrivacyRequest>>,
        next_id: std::sync::Mutex<i64>,
    }

    impl FakeRequestRepo {
        fn new() -> Self {
            Self {
                rows: Default::default(),
                next_id: std::sync::Mutex::new(1),
            }
        }
    }

    #[async_trait]
    impl PrivacyRequestRepository for FakeRequestRepo {
        async fn create(&self, r: &NewPrivacyRequest) -> Result<i64, PrivacyError> {
            let mut rows = self.rows.lock().unwrap();
            let mut next = self.next_id.lock().unwrap();
            let id = *next;
            *next += 1;
            rows.insert(
                id,
                PrivacyRequest {
                    id,
                    user_id: Some(r.user_id),
                    kind: r.kind,
                    state: PrivacyRequestState::Open,
                    details: r.details.clone(),
                    fulfilled_by: None,
                    fulfilled_at: None,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            );
            Ok(id)
        }
        async fn list(
            &self,
            _s: Option<PrivacyRequestState>,
        ) -> Result<Vec<PrivacyRequest>, PrivacyError> {
            Ok(self.rows.lock().unwrap().values().cloned().collect())
        }
        async fn get(&self, id: i64) -> Result<Option<PrivacyRequest>, PrivacyError> {
            Ok(self.rows.lock().unwrap().get(&id).cloned())
        }
        async fn fulfill(&self, id: i64, by: Option<UserId>) -> Result<(), PrivacyError> {
            let mut rows = self.rows.lock().unwrap();
            let Some(row) = rows.get_mut(&id) else {
                return Err(PrivacyError::NotFound);
            };
            row.state = PrivacyRequestState::Completed;
            row.fulfilled_by = by;
            row.fulfilled_at = Some(Utc::now());
            Ok(())
        }
    }

    struct FakeAnonymization;
    #[async_trait]
    impl AnonymizationRepository for FakeAnonymization {
        async fn is_last_admin(&self, _u: UserId) -> Result<bool, PrivacyError> {
            Ok(false)
        }
        async fn anonymize(
            &self,
            _u: UserId,
            _n: DateTime<Utc>,
        ) -> Result<AnonymizationReport, PrivacyError> {
            Ok(AnonymizationReport::default())
        }
    }

    struct FakeRetention;
    #[async_trait]
    impl RetentionRepository for FakeRetention {
        async fn purge_expired_password_reset_tokens(
            &self,
            _n: DateTime<Utc>,
        ) -> Result<u64, PrivacyError> {
            Ok(1)
        }
        async fn purge_expired_email_verification_tokens(
            &self,
            _n: DateTime<Utc>,
        ) -> Result<u64, PrivacyError> {
            Ok(2)
        }
        async fn purge_expired_sessions(&self, _n: DateTime<Utc>) -> Result<u64, PrivacyError> {
            Ok(3)
        }
        async fn purge_expired_parked_here(&self, _n: DateTime<Utc>) -> Result<u64, PrivacyError> {
            Ok(4)
        }
        async fn purge_expired_exports(&self, _n: DateTime<Utc>) -> Result<u64, PrivacyError> {
            Ok(5)
        }
        async fn purge_orphan_uploads(&self, _n: DateTime<Utc>) -> Result<u64, PrivacyError> {
            Ok(6)
        }
        async fn reconcile_pending_photos(&self, _n: DateTime<Utc>) -> Result<u64, PrivacyError> {
            Ok(9)
        }
        async fn anonymize_inactive_accounts(
            &self,
            _c: DateTime<Utc>,
        ) -> Result<u64, PrivacyError> {
            Ok(7)
        }
        async fn purge_deleted_accounts(&self, _c: DateTime<Utc>) -> Result<u64, PrivacyError> {
            Ok(8)
        }
    }

    struct FakeAccounts;
    #[async_trait]
    impl AccountRepository for FakeAccounts {
        async fn find_by_email(
            &self,
            _e: &bikesnest_domain::UserEmail,
        ) -> Result<Option<bikesnest_domain::User>, AuthError> {
            Ok(None)
        }
        async fn find_by_id(
            &self,
            _i: UserId,
        ) -> Result<Option<bikesnest_domain::User>, AuthError> {
            Ok(None)
        }
        async fn create(&self, _n: crate::auth::NewAccount<'_>) -> Result<UserId, AuthError> {
            Ok(UserId(1))
        }
        async fn set_state(&self, _i: UserId, _s: AccountState) -> Result<(), AuthError> {
            Ok(())
        }
        async fn mark_email_verified(
            &self,
            _i: UserId,
            _a: DateTime<Utc>,
        ) -> Result<(), AuthError> {
            Ok(())
        }
        async fn update_canonical_email(
            &self,
            _i: UserId,
            _e: &bikesnest_domain::UserEmail,
        ) -> Result<(), AuthError> {
            Ok(())
        }
        async fn confirm_email(
            &self,
            _i: UserId,
            _a: DateTime<Utc>,
            _e: &bikesnest_domain::UserEmail,
        ) -> Result<(), AuthError> {
            Ok(())
        }
        async fn set_password(&self, _i: UserId, _h: &str) -> Result<(), AuthError> {
            Ok(())
        }
        async fn set_locale(
            &self,
            _i: UserId,
            _l: bikesnest_domain::LocaleCode,
        ) -> Result<(), AuthError> {
            Ok(())
        }
        async fn link_identity(
            &self,
            _u: UserId,
            _p: AuthenticationProvider,
            _s: &str,
            _h: Option<&str>,
        ) -> Result<(), AuthError> {
            Ok(())
        }
        async fn find_identity(
            &self,
            _p: AuthenticationProvider,
            _s: &str,
        ) -> Result<Option<IdentityRecord>, AuthError> {
            Ok(Some(IdentityRecord {
                id: 1,
                user_id: UserId(1),
                provider: AuthenticationProvider::Password,
                provider_subject: "a@example.com".to_string(),
                credential_hash: Some("hash".to_string()),
            }))
        }
        async fn roles(&self, _i: UserId) -> Result<Vec<Role>, AuthError> {
            Ok(vec![Role::User])
        }
        async fn count_admins(&self) -> Result<i64, AuthError> {
            Ok(2)
        }
        async fn grant_role(&self, _i: UserId, _r: Role, _b: UserId) -> Result<(), AuthError> {
            Ok(())
        }
        async fn revoke_role_guarded(&self, _i: UserId, _r: Role) -> Result<bool, AuthError> {
            Ok(true)
        }
        async fn list_users(&self) -> Result<Vec<bikesnest_domain::User>, AuthError> {
            Ok(vec![])
        }
        async fn search_users(
            &self,
            _s: crate::auth::UserSearch<'_>,
        ) -> Result<Vec<bikesnest_domain::User>, AuthError> {
            Ok(vec![])
        }
        async fn labels_for(
            &self,
            _ids: &[i64],
        ) -> Result<std::collections::HashMap<i64, String>, AuthError> {
            Ok(std::collections::HashMap::new())
        }
        async fn activity_for(
            &self,
            _ids: &[i64],
        ) -> Result<std::collections::HashMap<i64, crate::auth::UserActivity>, AuthError> {
            Ok(std::collections::HashMap::new())
        }
    }

    struct FakeHasher;
    #[async_trait]
    impl PasswordHasher for FakeHasher {
        async fn hash(&self, _p: &Password) -> Result<String, AuthError> {
            Ok("hash".to_string())
        }
        async fn verify(&self, _p: &Password, _h: &str) -> Result<bool, AuthError> {
            Ok(true)
        }
    }

    struct FakeTokens(bool);
    impl TokenGenerator for FakeTokens {
        fn generate(&self) -> [u8; 32] {
            [self.0 as u8; 32]
        }
    }

    struct FakeClock(DateTime<Utc>);
    impl Clock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    struct FakeSessions;
    #[async_trait]
    impl SessionStore for FakeSessions {
        async fn create(
            &self,
            _u: UserId,
            _r: &SessionId,
            _c: &CsrfToken,
            _n: DateTime<Utc>,
        ) -> Result<(), AuthError> {
            Ok(())
        }
        async fn resolve(
            &self,
            _r: &SessionId,
            _n: DateTime<Utc>,
        ) -> Result<Option<Session>, AuthError> {
            Ok(None)
        }
        async fn revoke(&self, _r: &SessionId) -> Result<(), AuthError> {
            Ok(())
        }
        async fn revoke_all_for_user_except(
            &self,
            _u: UserId,
            _k: &SessionId,
        ) -> Result<(), AuthError> {
            Ok(())
        }
        async fn revoke_all_for_user(&self, _u: UserId) -> Result<(), AuthError> {
            Ok(())
        }
    }

    struct FakeAudit;
    #[async_trait]
    impl AuditLog for FakeAudit {
        async fn record(&self, _e: AuditEvent) -> Result<(), crate::audit::AuditError> {
            Ok(())
        }
    }

    fn deps(now: DateTime<Utc>) -> PrivacyDeps {
        PrivacyDeps {
            exports: Box::new(FakeExportRepo::new()),
            requests: Box::new(FakeRequestRepo::new()),
            anonymization: Box::new(FakeAnonymization),
            accounts: Box::new(FakeAccounts),
            sessions: Box::new(FakeSessions),
            audit: Box::new(FakeAudit),
            hasher: Box::new(FakeHasher),
            tokens_gen: Box::new(FakeTokens(false)),
            clock: Box::new(FakeClock(now)),
        }
    }

    #[tokio::test]
    async fn request_export_returns_id_and_token() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let svc = PrivacyService::new(deps(now));
        let req = svc.request_export(&actor()).await.expect("export");
        assert_eq!(req.id, 1);
        assert!(!req.token.is_empty());
        assert_eq!(req.expires_at - now, Duration::hours(24));
    }

    #[tokio::test]
    async fn download_export_wrong_token_is_invalid() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let mut d = deps(now);
        // Force a stored token differing from the one the actor submits.
        d.tokens_gen = Box::new(FakeTokens(false));
        let svc = PrivacyService::new(d);
        svc.request_export(&actor()).await.unwrap();
        let bad = b64url_encode(&[1u8; 32]);
        let res = svc.download_export(&actor(), 1, &bad).await;
        assert!(matches!(res, Err(PrivacyError::InvalidToken)));
    }

    #[tokio::test]
    async fn request_deletion_rejects_wrong_email() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let svc = PrivacyService::new(deps(now));
        let res = svc
            .request_deletion(
                &actor(),
                Some("correct-password"),
                "not-my-email@example.com",
            )
            .await;
        assert!(matches!(res, Err(PrivacyError::ReauthRequired)));
    }

    #[tokio::test]
    async fn request_deletion_happy_path() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let svc = PrivacyService::new(deps(now));
        // Password identity exists but FakeHasher always verifies true.
        svc.request_deletion(&actor(), Some("correct-password"), "a@example.com")
            .await
            .expect("deletion");
    }

    #[tokio::test]
    async fn submit_request_rejects_automatic_kind() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let svc = PrivacyService::new(deps(now));
        let res = svc
            .submit_request(
                &actor(),
                PrivacyRequestKind::Deletion,
                serde_json::json!({}),
            )
            .await;
        assert!(matches!(res, Err(PrivacyError::InvalidKind)));
    }

    #[tokio::test]
    async fn retention_job_skips_config_gated_steps() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let job = RetentionJob::new(
            Box::new(FakeRetention),
            Box::new(FakeAudit),
            Box::new(FakeClock(now)),
            RetentionConfig::default(), // both 0 → only the 7 default steps
        );
        let summary = job.run().await.expect("retention");
        assert_eq!(summary.steps.len(), 7);
        assert_eq!(summary.steps[0].purged, 1);
    }

    #[tokio::test]
    async fn retention_job_includes_config_gated_steps_when_enabled() {
        let now = DateTime::<Utc>::from_timestamp(1_700_000_000, 0).unwrap();
        let job = RetentionJob::new(
            Box::new(FakeRetention),
            Box::new(FakeAudit),
            Box::new(FakeClock(now)),
            RetentionConfig {
                inactive_account_anonymize_after_days: 90,
                deleted_account_purge_after_days: 180,
            },
        );
        let summary = job.run().await.expect("retention");
        assert_eq!(summary.steps.len(), 9);
    }
}
