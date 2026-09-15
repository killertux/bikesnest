//! SQL-backed retention job repository.
//!
//! Every purge is a `DELETE WHERE expires_at|revoked_at|cursor < now()` so a
//! re-run is a no-op (idempotent). The two config-gated steps are no-ops when
//! their TTL is 0 (the caller — [`RetentionJob`] — decides whether to invoke
//! them at all; this repo conservatively returns 0 if asked directly).

use crate::Db;
use crate::privacy::SqlxAnonymizationRepository;
use async_trait::async_trait;
use bikesnest_application::{
    AnonymizationRepository, ObjectStorage, PrivacyError, RetentionRepository, StorageError,
};
use bikesnest_domain::{RetentionPolicy, UserId};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashSet;
use std::sync::Arc;

/// Key prefix every uploaded derivative lives under (`uploads/{id}/full.jpg`,
/// `uploads/{id}/thumb.jpg`). Seeded objects live under `seed/` and are never
/// swept: they belong to the dev dataset, not to a user upload.
const UPLOAD_PREFIX: &str = "uploads/";

/// A `PENDING_REVIEW` photo row younger than this is left alone: its objects
/// may still be mid-write.
const PENDING_RECONCILE_GRACE: Duration = Duration::hours(1);

pub struct SqlxRetentionRepository {
    db: Db,
    policy: RetentionPolicy,
    storage: Arc<dyn ObjectStorage>,
}

impl SqlxRetentionRepository {
    pub fn new(db: Db, policy: RetentionPolicy, storage: Arc<dyn ObjectStorage>) -> Self {
        Self {
            db,
            policy,
            storage,
        }
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"retention.sweep"`.
fn db_err(context: &'static str, e: sqlx::Error) -> PrivacyError {
    crate::db_error::classify_and_log(context, e).into()
}

/// Log + map an object-storage failure. A retention step must never report a
/// successful zero when the store could not answer — that is exactly how the
/// old filesystem sweep went silently dead.
fn storage_err(context: &'static str, e: StorageError) -> PrivacyError {
    tracing::warn!(error = %e, context, "retention: object storage failed");
    match e {
        StorageError::Unavailable => PrivacyError::Unavailable,
        _ => PrivacyError::Internal,
    }
}

#[async_trait]
impl RetentionRepository for SqlxRetentionRepository {
    async fn purge_expired_password_reset_tokens(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("retention.purge_expired_password_reset_tokens", e))?;
        let count: i64 = sqlx::query_scalar(r#"
            WITH scrub AS (
              UPDATE background_job SET payload='{}'::jsonb,
                state=CASE WHEN state IN ('pending','running') THEN 'failed' ELSE state END,
                last_error=CASE WHEN state IN ('pending','running','failed') THEN 'mail credential expired' ELSE NULL END,
                finished_at=CASE WHEN state IN ('pending','running') THEN COALESCE(finished_at,$1) ELSE finished_at END,
                claimed_by=NULL, lease_expires_at=NULL, heartbeat_at=NULL,
                mail_recipient_hash=NULL, mail_transition_audit_id=NULL,
                payload_redacted_at=COALESCE(payload_redacted_at,$1), updated_at=now()
              WHERE kind='email.send' AND mail_purpose='reset' AND mail_token_expires_at < $1
            ), deleted AS (DELETE FROM password_reset_tokens WHERE expires_at < $1 RETURNING 1)
            SELECT count(*) FROM deleted"#)
            .bind(now)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| db_err("retention.purge_expired_password_reset_tokens", e))?;
        Ok(count as u64)
    }

    async fn purge_expired_email_verification_tokens(
        &self,
        now: DateTime<Utc>,
    ) -> Result<u64, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("retention.purge_expired_email_verification_tokens", e))?;
        let count: i64 = sqlx::query_scalar(r#"
            WITH scrub AS (
              UPDATE background_job SET payload='{}'::jsonb,
                state=CASE WHEN state IN ('pending','running') THEN 'failed' ELSE state END,
                last_error=CASE WHEN state IN ('pending','running','failed') THEN 'mail credential expired' ELSE NULL END,
                finished_at=CASE WHEN state IN ('pending','running') THEN COALESCE(finished_at,$1) ELSE finished_at END,
                claimed_by=NULL, lease_expires_at=NULL, heartbeat_at=NULL,
                mail_recipient_hash=NULL, mail_transition_audit_id=NULL,
                payload_redacted_at=COALESCE(payload_redacted_at,$1), updated_at=now()
              WHERE kind='email.send' AND mail_purpose IN ('verify','change') AND mail_token_expires_at < $1
            ), deleted AS (DELETE FROM email_verification_tokens WHERE expires_at < $1 RETURNING 1)
            SELECT count(*) FROM deleted"#)
            .bind(now)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| db_err("retention.purge_expired_email_verification_tokens", e))?;
        Ok(count as u64)
    }

    async fn purge_expired_sessions(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("retention.purge_expired_sessions", e))?;
        let idle_cutoff = now - self.policy.session_idle;
        let res = sqlx::query("DELETE FROM sessions WHERE expires_at < $1 OR last_seen_at < $2")
            .bind(now)
            .bind(idle_cutoff)
            .execute(&mut *conn)
            .await
            .map_err(|e| db_err("retention.purge_expired_sessions", e))?;
        Ok(res.rows_affected())
    }

    async fn purge_expired_parked_here(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("retention.purge_expired_parked_here", e))?;
        let res = sqlx::query("DELETE FROM verification WHERE kind = 'parked_here' AND expires_at IS NOT NULL AND expires_at < $1").bind(now)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_err("retention.purge_expired_parked_here", e))?;
        Ok(res.rows_affected())
    }

    async fn purge_expired_exports(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("retention.purge_expired_exports", e))?;
        let res = sqlx::query("DELETE FROM personal_data_export WHERE expires_at < $1")
            .bind(now)
            .execute(&mut *conn)
            .await
            .map_err(|e| db_err("retention.purge_expired_exports", e))?;
        Ok(res.rows_affected())
    }

    /// Deletes aged, unreferenced upload objects.
    ///
    /// The authority on which objects exist is the **object store**, listed a
    /// page at a time under `uploads/`. (This used to walk a local `media_root`
    /// directory; once media moved to S3 that directory stopped existing,
    /// `read_dir` failed, the error was swallowed and the step reported a
    /// contented `Ok(0)` forever — media retention was a silent no-op.)
    ///
    /// Per page: gate on age first (only objects past the orphan TTL can ever
    /// be deleted), then probe the database once for the whole batch to see
    /// which of those keys a photo row still references. Anything aged and
    /// unreferenced is deleted. A listing or delete failure propagates — a
    /// step that cannot see the store reports an error, never zero.
    async fn purge_orphan_uploads(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError> {
        let cutoff = now - self.policy.upload_orphan_ttl;
        let mut purged = 0u64;
        let mut after: Option<String> = None;
        loop {
            let page = self
                .storage
                .list(UPLOAD_PREFIX, after.as_deref())
                .await
                .map_err(|e| storage_err("retention.purge_orphan_uploads", e))?;
            let aged: Vec<String> = page
                .objects
                .iter()
                .filter(|o| o.last_modified < cutoff)
                .map(|o| o.key.clone())
                .collect();
            purged += self.purge_unreferenced(aged).await?;
            match page.next {
                Some(next) => after = Some(next),
                None => break,
            }
        }
        Ok(purged)
    }

    /// Belt and braces for the upload path: a `PENDING_REVIEW` row whose full
    /// derivative is not in the store can never render, and the moderation
    /// queue would show it as a broken image forever. Rows younger than
    /// [`PENDING_RECONCILE_GRACE`] are left alone (an in-flight upload).
    ///
    /// With the keys now minted before any write, this should find nothing —
    /// which is the point: it is the check that says so.
    async fn reconcile_pending_photos(&self, now: DateTime<Utc>) -> Result<u64, PrivacyError> {
        let cutoff = now - PENDING_RECONCILE_GRACE;
        let mut deleted = 0u64;
        for (table, context) in [
            (
                "parking_photo",
                "retention.reconcile_pending_photos.parking",
            ),
            ("review_photo", "retention.reconcile_pending_photos.review"),
        ] {
            let rows: Vec<(i64, String)> = {
                let mut conn = self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("retention.reconcile_pending_photos", e))?;
                sqlx::query_as(&format!(
                    "SELECT id, storage_key FROM {table} \
                     WHERE moderation_state = 'PENDING_REVIEW' AND created_at < $1"
                ))
                .bind(cutoff)
                .fetch_all(&mut *conn)
                .await
                .map_err(|e| db_err("retention.reconcile_pending_photos", e))?
            };

            for (id, key) in rows {
                if self
                    .storage
                    .exists(&key)
                    .await
                    .map_err(|e| storage_err(context, e))?
                {
                    continue;
                }
                let mut conn = self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("retention.reconcile_pending_photos", e))?;
                let res = sqlx::query(&format!("DELETE FROM {table} WHERE id = $1"))
                    .bind(id)
                    .execute(&mut *conn)
                    .await
                    .map_err(|e| db_err("retention.reconcile_pending_photos", e))?;
                deleted += res.rows_affected();
            }
        }
        Ok(deleted)
    }

    async fn anonymize_inactive_accounts(
        &self,
        cutoff: DateTime<Utc>,
    ) -> Result<u64, PrivacyError> {
        #[derive(sqlx::FromRow)]
        struct IdRow {
            id: i64,
        }
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("retention.anonymize_inactive_accounts", e))?;
        let candidates: Vec<i64> = sqlx::query_as::<_, IdRow>(r#"
            SELECT u.id FROM users u
            WHERE u.account_state <> 'DELETED'
              AND NOT EXISTS (SELECT 1 FROM user_roles r WHERE r.user_id = u.id AND r.role = 'ADMIN')
              AND COALESCE(
                    (SELECT max(s.last_seen_at) FROM sessions s WHERE s.user_id = u.id),
                    u.created_at
                  ) < $1
            "#).bind(cutoff)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| db_err("retention.anonymize_inactive_accounts", e))?
        .into_iter()
        .map(|r| r.id)
        .collect();
        drop(conn);

        let anonymizer = SqlxAnonymizationRepository::new(self.db.clone());
        let mut count = 0u64;
        for id in candidates {
            let _ = anonymizer.anonymize(UserId(id), Utc::now()).await?;
            count += 1;
        }
        Ok(count)
    }

    async fn purge_deleted_accounts(&self, cutoff: DateTime<Utc>) -> Result<u64, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("retention.purge_deleted_accounts", e))?;
        let res =
            sqlx::query("DELETE FROM users WHERE account_state = 'DELETED' AND deleted_at < $1")
                .bind(cutoff)
                .execute(&mut *conn)
                .await
                .map_err(|e| db_err("retention.purge_deleted_accounts", e))?;
        Ok(res.rows_affected())
    }
}

impl SqlxRetentionRepository {
    /// Of `candidate_keys`, the subset still referenced by some photo row
    /// (as either `storage_key` or `thumbnail_key`) — probed in one query per
    /// batch instead of loading every referenced key in the database. Safe to
    /// call with an empty batch (short-circuits without a round trip).
    async fn referenced(&self, candidate_keys: &[String]) -> Result<HashSet<String>, PrivacyError> {
        if candidate_keys.is_empty() {
            return Ok(HashSet::new());
        }
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("retention.referenced", e))?;
        let rows = sqlx::query_scalar::<_, String>(
            r#"
            SELECT storage_key FROM parking_photo WHERE storage_key = ANY($1)
            UNION
            SELECT storage_key FROM review_photo WHERE storage_key = ANY($1)
            UNION
            SELECT thumbnail_key FROM parking_photo WHERE thumbnail_key = ANY($1)
            UNION
            SELECT thumbnail_key FROM review_photo WHERE thumbnail_key = ANY($1)
            "#,
        )
        .bind(candidate_keys)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| db_err("retention.referenced", e))?;
        Ok(rows.into_iter().collect())
    }

    /// Deletes every key in `candidates` that [`Self::referenced`] does not
    /// report as still in use. Returns the number actually deleted; a delete
    /// failure propagates rather than being counted as a skip, so a store that
    /// is refusing writes cannot masquerade as a clean sweep.
    async fn purge_unreferenced(&self, candidates: Vec<String>) -> Result<u64, PrivacyError> {
        let referenced = self.referenced(&candidates).await?;
        let mut purged = 0u64;
        for key in candidates {
            if referenced.contains(&key) {
                continue;
            }
            self.storage
                .delete(&key)
                .await
                .map_err(|e| storage_err("retention.purge_unreferenced", e))?;
            purged += 1;
        }
        Ok(purged)
    }
}
