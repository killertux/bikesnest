//! SQL-backed anonymize-in-place repository.
//!
//! One transaction: scrub the `users` row (PII → non-attributable), delete
//! private *activity* + *identity*, and NULL every attribution column on the
//! community content that must remain visible but *unattributed*. The row
//! counts let tests and the audit trail assert completeness.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{AnonymizationReport, AnonymizationRepository, PrivacyError};
use bikesnest_domain::{UserId, anonymized_email};
use chrono::{DateTime, Utc};

pub struct SqlxAnonymizationRepository {
    db: Db,
}

impl SqlxAnonymizationRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

/// Locks the ADMIN role rows and answers "would removing `user_id` leave the
/// system with no administrator?".
///
/// `FOR UPDATE` is the whole point: it is taken **inside** the anonymization
/// transaction, so two deletions racing for the last two admin seats serialize
/// on these rows instead of both reading `count = 2` and both proceeding. The
/// second one to arrive blocks until the first commits, then sees the
/// post-commit truth and refuses.
async fn last_admin_locked(
    tx: &mut sqlx::PgConnection,
    user_id: UserId,
) -> Result<bool, PrivacyError> {
    let admins: Vec<i64> = sqlx::query_scalar::<_, i64>(
        "SELECT user_id FROM user_roles WHERE role = 'ADMIN' FOR UPDATE",
    )
    .fetch_all(&mut *tx)
    .await
    .map_err(|e| db_err("anonymize.last_admin_locked", e))?;
    Ok(admins.contains(&user_id.0) && admins.len() == 1)
}

#[async_trait]
impl AnonymizationRepository for SqlxAnonymizationRepository {
    /// A lock-free pre-check so the caller can refuse before it writes a
    /// privacy-request row. It is **not** the guard: [`Self::anonymize`] holds
    /// that inside its own transaction (see [`last_admin_locked`]).
    async fn is_last_admin(&self, user_id: UserId) -> Result<bool, PrivacyError> {
        // True only when the deleting user IS an ADMIN *and* no other ADMIN exists.
        // A non-admin must never be blocked by the last-admin guard even if the
        // system happens to have zero admins.
        let res = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM user_roles WHERE role = 'ADMIN' AND user_id = $1)\
             AND (SELECT count(*) FROM user_roles WHERE role = 'ADMIN' AND user_id <> $1) = 0",
        )
        .bind(user_id.0)
        .fetch_one(self.db.pool())
        .await
        .map_err(|e| db_err("anonymize.is_last_admin", e))?;
        Ok(res)
    }

    async fn anonymize(
        &self,
        user_id: UserId,
        now: DateTime<Utc>,
    ) -> Result<AnonymizationReport, PrivacyError> {
        let mut tx = self
            .db
            .pool()
            .begin()
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?;

        // The last-admin guard runs here, holding a row lock, rather than as a
        // separate query before the transaction: otherwise two simultaneous
        // deletions of the last two admins both pass their own check and the
        // system ends up with none.
        if last_admin_locked(&mut tx, user_id).await? {
            return Err(PrivacyError::LastAdmin);
        }

        // `audit_events` is append-only (migration 0019). Erasure is one of the
        // two sanctioned exceptions, and announces itself for this transaction
        // only — the trigger reads this setting.
        sqlx::query("SET LOCAL app.audit_purge = 'on'")
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?;

        let scrub_email = anonymized_email(user_id);
        // The account's current email, read before it is scrubbed: a failed
        // login is audited with the *email* as `target_id` (there is no user id
        // to record when the credentials never resolved), so the audit trail
        // holds direct personal data that nulling `actor_user_id` never reaches.
        let old_email: Option<String> =
            sqlx::query_scalar::<_, String>("SELECT email FROM users WHERE id = $1")
                .bind(user_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?;

        // 1) Scrub the users row (PII gone; the shell stays for FK stability).
        sqlx::query(
            r#"
            UPDATE users
            SET email = $2, display_name = NULL, email_verified_at = NULL,
                suspended_at = NULL, deleted_at = $3, account_state = 'DELETED',
                public_contribution_name = FALSE, public_contribution_name_updated_at = NULL
            WHERE id = $1
            "#,
        )
        .bind(user_id.0)
        .bind(&scrub_email)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("anonymize.anonymize", e))?;

        let identities = sqlx::query("DELETE FROM authentication_identities WHERE user_id = $1")
            .bind(user_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?
            .rows_affected();
        let roles = sqlx::query("DELETE FROM user_roles WHERE user_id = $1")
            .bind(user_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?
            .rows_affected();
        // Roles this account granted to *other* people still name it. The
        // column's `ON DELETE SET NULL` only fires on a hard delete, which
        // anonymize-in-place never performs — so the reference survives unless
        // it is nulled here.
        let roles_granted_by_anonymized =
            sqlx::query("UPDATE user_roles SET granted_by = NULL WHERE granted_by = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        let sessions = sqlx::query("DELETE FROM sessions WHERE user_id = $1")
            .bind(user_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?
            .rows_affected();
        let email_verification_tokens =
            sqlx::query("DELETE FROM email_verification_tokens WHERE user_id = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        let password_reset_tokens =
            sqlx::query("DELETE FROM password_reset_tokens WHERE user_id = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        let favorites = sqlx::query("DELETE FROM favorite WHERE user_id = $1")
            .bind(user_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?
            .rows_affected();
        // Parked-here is personal activity → deleted.
        let parked_here =
            sqlx::query("DELETE FROM verification WHERE user_id = $1 AND kind = 'parked_here'")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        let exports = sqlx::query("DELETE FROM personal_data_export WHERE user_id = $1")
            .bind(user_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?
            .rows_affected();
        let consent_records = sqlx::query("DELETE FROM consent_record WHERE user_id = $1")
            .bind(user_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?
            .rows_affected();

        // 2) Community content is retained but unattributed.
        let reviews_anonymized = sqlx::query(
            "UPDATE review SET author_id = NULL, public_author = FALSE WHERE author_id = $1",
        )
        .bind(user_id.0)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("anonymize.anonymize", e))?
        .rows_affected();
        let verifications_anonymized = sqlx::query(
            "UPDATE verification SET user_id = NULL WHERE user_id = $1 AND kind <> 'parked_here'",
        )
        .bind(user_id.0)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("anonymize.anonymize", e))?
        .rows_affected();
        // A deleted moderator must not erase the proposer attribution of a
        // different person's contribution (and vice versa).  These are two
        // independent nullable relationships, so scrub them independently.
        let proposals_anonymized =
            sqlx::query("UPDATE parking_proposal SET proposer_id = NULL WHERE proposer_id = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected()
                + sqlx::query(
                    "UPDATE parking_proposal SET resolved_by = NULL WHERE resolved_by = $1",
                )
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        // Votes are a private, mutable eligibility signal rather than retained
        // community content. Remove them on erasure; this also keeps a deleted
        // account from contributing to an approval threshold.
        sqlx::query("DELETE FROM parking_proposal_vote WHERE voter_id = $1")
            .bind(user_id.0)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?;
        let reports_anonymized = sqlx::query(
            r#"
            UPDATE report SET reporter_id = NULL, claimed_by = NULL, resolved_by = NULL
            WHERE reporter_id = $1 OR claimed_by = $1 OR resolved_by = $1
            "#,
        )
        .bind(user_id.0)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("anonymize.anonymize", e))?
        .rows_affected();
        let locations_anonymized =
            sqlx::query("UPDATE parking_location SET creator_id = NULL WHERE creator_id = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        let revisions_anonymized =
            sqlx::query("UPDATE parking_revision SET editor_id = NULL WHERE editor_id = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        let parking_photos_anonymized =
            sqlx::query("UPDATE parking_photo SET uploader_id = NULL WHERE uploader_id = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        let review_photos_anonymized =
            sqlx::query("UPDATE review_photo SET uploader_id = NULL WHERE uploader_id = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        let audit_events_anonymized =
            sqlx::query("UPDATE audit_events SET actor_user_id = NULL WHERE actor_user_id = $1")
                .bind(user_id.0)
                .execute(&mut *tx)
                .await
                .map_err(|e| db_err("anonymize.anonymize", e))?
                .rows_affected();
        // `metadata` is deliberately *not* rewritten: the audit log writes a
        // closed set of keys, none of which can hold an email, IP, user agent
        // or display name (`AUDIT_METADATA_KEYS` in
        // `crates/infrastructure/src/auth/audit.rs` pins that set, and a test
        // fails if a new key appears without being classified). `target_id`
        // does hold personal data — the attempted email on a failed login — so
        // replace it with the anonymized form: the row stays countable and
        // correlatable, it just stops naming a person.
        let audit_targets_anonymized = match old_email.as_deref() {
            Some(email) => sqlx::query(
                "UPDATE audit_events SET target_id = $2 WHERE target_type = 'user' AND target_id = $1",
            )
            .bind(email)
            .bind(&scrub_email)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?
            .rows_affected(),
            None => 0,
        };
        let privacy_requests_anonymized = sqlx::query("UPDATE privacy_request SET user_id = NULL, fulfilled_by = NULL WHERE user_id = $1 OR fulfilled_by = $1").bind(user_id.0)
        .execute(&mut *tx).await.map_err(|e| db_err("anonymize.anonymize", e))?.rows_affected();

        tx.commit()
            .await
            .map_err(|e| db_err("anonymize.anonymize", e))?;

        Ok(AnonymizationReport {
            identities,
            roles,
            roles_granted_by_anonymized,
            sessions,
            email_verification_tokens,
            password_reset_tokens,
            favorites,
            parked_here,
            exports,
            consent_records,
            reviews_anonymized,
            verifications_anonymized,
            proposals_anonymized,
            reports_anonymized,
            locations_anonymized,
            revisions_anonymized,
            parking_photos_anonymized,
            review_photos_anonymized,
            audit_events_anonymized,
            audit_targets_anonymized,
            privacy_requests_anonymized,
        })
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"anonymize.anonymize"`.
fn db_err(context: &'static str, e: sqlx::Error) -> PrivacyError {
    crate::db_error::classify_and_log(context, e).into()
}
