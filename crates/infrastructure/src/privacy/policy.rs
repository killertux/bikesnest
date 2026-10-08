//! SQL-backed policy reader. Versioned legal
//! pages per locale: `current` (latest effective, not superseded) +
//! `history` (all). Locale fallback (→ pt-BR) is the caller's job.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{
    PendingTermsNotice, PolicyDocument, PolicyReader, PrivacyError, TermsAcknowledgementStore,
    TermsProof,
};
use bikesnest_domain::PolicyKind;
use chrono::{DateTime, Utc};

pub struct SqlxPolicyReader {
    db: Db,
}

impl SqlxPolicyReader {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct PolicyRow {
    id: i64,
    kind: String,
    locale: String,
    version: String,
    effective_at: DateTime<Utc>,
    superseded_at: Option<DateTime<Utc>>,
    requires_acknowledgement: bool,
    content: String,
}

impl PolicyRow {
    fn into_document(self) -> Result<PolicyDocument, PrivacyError> {
        let kind = PolicyKind::from_code(&self.kind).map_err(|_| PrivacyError::Internal)?;
        Ok(PolicyDocument {
            id: self.id,
            kind,
            locale: self.locale,
            version: self.version,
            effective_at: self.effective_at,
            superseded_at: self.superseded_at,
            requires_acknowledgement: self.requires_acknowledgement,
            content: self.content,
        })
    }
}

#[async_trait]
impl PolicyReader for SqlxPolicyReader {
    async fn current(
        &self,
        kind: PolicyKind,
        locale: &str,
    ) -> Result<Option<PolicyDocument>, PrivacyError> {
        let row = sqlx::query_as::<_, PolicyRow>(
            r#"
            SELECT id, kind, locale, version, effective_at, superseded_at, content,
                   requires_acknowledgement
            FROM policy_version
            WHERE kind = $1 AND locale = $2
              AND effective_at <= clock_timestamp()
              AND (superseded_at IS NULL OR superseded_at > clock_timestamp())
            ORDER BY effective_at DESC
            LIMIT 1
            "#,
        )
        .bind(kind.as_code())
        .bind(locale)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("policy.current", e))?,
        )
        .await
        .map_err(|e| db_err("policy.current", e))?;
        match row {
            Some(r) => Ok(Some(r.into_document()?)),
            None => Ok(None),
        }
    }

    async fn history(
        &self,
        kind: PolicyKind,
        locale: &str,
    ) -> Result<Vec<PolicyDocument>, PrivacyError> {
        let rows = sqlx::query_as::<_, PolicyRow>(
            r#"
            SELECT id, kind, locale, version, effective_at, superseded_at, content,
                   requires_acknowledgement
            FROM policy_version
            WHERE kind = $1 AND locale = $2
            ORDER BY effective_at DESC, id DESC
            "#,
        )
        .bind(kind.as_code())
        .bind(locale)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("policy.history", e))?,
        )
        .await
        .map_err(|e| db_err("policy.history", e))?;
        rows.into_iter().map(PolicyRow::into_document).collect()
    }

    async fn by_id(&self, id: i64) -> Result<Option<PolicyDocument>, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("policy.by_id", e))?;
        sqlx::query_as::<_, PolicyRow>(
            "SELECT id,kind,locale,version,effective_at,superseded_at,content,requires_acknowledgement FROM policy_version WHERE id=$1",
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_err("policy.by_id", e))?
        .map(PolicyRow::into_document)
        .transpose()
    }

    async fn upcoming_material_terms(
        &self,
        locale: &str,
    ) -> Result<Option<PolicyDocument>, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("policy.upcoming", e))?;
        sqlx::query_as::<_, PolicyRow>(
            r#"SELECT id,kind,locale,version,effective_at,superseded_at,content,requires_acknowledgement
               FROM policy_version WHERE kind='terms' AND locale=$1
                 AND requires_acknowledgement AND effective_at>clock_timestamp()
               ORDER BY effective_at,id LIMIT 1"#,
        )
        .bind(locale)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_err("policy.upcoming", e))?
        .map(PolicyRow::into_document)
        .transpose()
    }
}

async fn lock_policy_release(conn: &mut sqlx::PgConnection) -> Result<(), PrivacyError> {
    sqlx::query("SELECT pg_advisory_xact_lock(726_159_001)")
        .execute(conn)
        .await
        .map_err(|e| db_err("terms.lock_release", e))?;
    Ok(())
}

async fn lock_eligible_account(
    conn: &mut sqlx::PgConnection,
    user_id: i64,
) -> Result<bool, PrivacyError> {
    let state: Option<String> =
        sqlx::query_scalar("SELECT account_state::text FROM users WHERE id=$1 FOR UPDATE")
            .bind(user_id)
            .fetch_optional(conn)
            .await
            .map_err(|e| db_err("terms.lock_account", e))?;
    Ok(matches!(
        state.as_deref(),
        Some("ACTIVE" | "PENDING_EMAIL_VERIFICATION")
    ))
}

#[async_trait]
impl TermsAcknowledgementStore for SqlxPolicyReader {
    async fn pending_notices(
        &self,
        user_id: bikesnest_domain::UserId,
        locale: &str,
    ) -> Result<Vec<PendingTermsNotice>, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("terms.pending", e))?;
        let mut tx = conn.begin().await.map_err(|e| db_err("terms.pending", e))?;
        if !lock_eligible_account(&mut tx, user_id.0).await? {
            return Ok(Vec::new());
        }
        lock_policy_release(&mut tx).await?;
        let decision_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| db_err("terms.pending", e))?;
        let row = sqlx::query_as::<_, PolicyRow>(
            r#"SELECT p.id,p.kind,p.locale,p.version,p.effective_at,p.superseded_at,p.content,p.requires_acknowledgement
               FROM policy_version p
               WHERE p.kind='terms' AND p.locale=$2 AND p.requires_acknowledgement
                 AND (
                   p.id = (SELECT future.id FROM policy_version future
                           WHERE future.kind='terms' AND future.locale=$2
                             AND future.requires_acknowledgement
                             AND future.effective_at>$3
                           ORDER BY future.effective_at ASC, future.id ASC
                           LIMIT 1)
                   OR (p.id = (SELECT current.id FROM policy_version current
                               WHERE current.kind='terms' AND current.locale=$2
                                 AND current.requires_acknowledgement
                                 AND current.effective_at<=$3
                                 AND (current.superseded_at IS NULL OR current.superseded_at>$3)
                               ORDER BY current.effective_at DESC, current.id DESC
                               LIMIT 1)
                       AND NOT EXISTS (SELECT 1 FROM terms_acknowledgement a
                                       WHERE a.user_id=$1 AND a.terms_version=p.version))
                 )
               ORDER BY (p.effective_at>$3) ASC, p.effective_at ASC"#,
        )
        .bind(user_id.0)
        .bind(locale)
        .bind(decision_at)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| db_err("terms.pending", e))?;
        let result = row
            .into_iter()
            .map(|row| {
                let may_acknowledge = row.effective_at <= decision_at;
                row.into_document().map(|document| PendingTermsNotice {
                    document,
                    may_acknowledge,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await.map_err(|e| db_err("terms.pending", e))?;
        Ok(result)
    }

    async fn present(
        &self,
        user_id: bikesnest_domain::UserId,
        proof: &TermsProof,
    ) -> Result<(), PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("terms.present", e))?;
        let mut tx = conn.begin().await.map_err(|e| db_err("terms.present", e))?;
        if !lock_eligible_account(&mut tx, user_id.0).await? {
            return Err(PrivacyError::NotAuthorized);
        }
        lock_policy_release(&mut tx).await?;
        let decision_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| db_err("terms.present", e))?;
        let valid = sqlx::query_scalar::<_, bool>(
            r#"SELECT kind='terms' AND requires_acknowledgement AND version=$2 AND locale=$3
               AND (superseded_at IS NULL OR superseded_at>$4)
               FROM policy_version WHERE id=$1"#,
        )
        .bind(proof.policy_version_id)
        .bind(&proof.terms_version)
        .bind(&proof.shown_locale)
        .bind(decision_at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("terms.present", e))?
        .unwrap_or(false);
        if !valid {
            return Err(PrivacyError::Conflict);
        }
        sqlx::query("INSERT INTO terms_notice_presentation(user_id,policy_version_id,terms_version,shown_locale,presented_at) VALUES($1,$2,$3,$4,$5) ON CONFLICT(user_id,terms_version) DO NOTHING")
            .bind(user_id.0).bind(proof.policy_version_id).bind(&proof.terms_version).bind(&proof.shown_locale).bind(decision_at)
            .execute(&mut *tx).await.map_err(|e| db_err("terms.present", e))?;
        tx.commit().await.map_err(|e| db_err("terms.present", e))
    }

    async fn acknowledge_current(
        &self,
        user_id: bikesnest_domain::UserId,
        proof: &TermsProof,
    ) -> Result<(), PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("terms.ack", e))?;
        let mut tx = conn.begin().await.map_err(|e| db_err("terms.ack", e))?;
        if !lock_eligible_account(&mut tx, user_id.0).await? {
            return Err(PrivacyError::NotAuthorized);
        }
        lock_policy_release(&mut tx).await?;
        let decision_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| db_err("terms.ack", e))?;
        let valid = sqlx::query_scalar::<_, bool>(
            r#"SELECT kind='terms' AND requires_acknowledgement AND version=$2 AND locale=$3
               AND effective_at<=$4
               AND (superseded_at IS NULL OR superseded_at>$4)
               FROM policy_version WHERE id=$1 FOR UPDATE"#,
        )
        .bind(proof.policy_version_id)
        .bind(&proof.terms_version)
        .bind(&proof.shown_locale)
        .bind(decision_at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("terms.ack", e))?
        .unwrap_or(false);
        if !valid {
            return Err(PrivacyError::Conflict);
        }
        sqlx::query("INSERT INTO terms_acknowledgement(user_id,policy_version_id,terms_version,shown_locale,acknowledged_at,source) VALUES($1,$2,$3,$4,$5,'in_product') ON CONFLICT(user_id,terms_version) DO NOTHING")
            .bind(user_id.0).bind(proof.policy_version_id).bind(&proof.terms_version).bind(&proof.shown_locale).bind(decision_at)
            .execute(&mut *tx).await.map_err(|e| db_err("terms.ack", e))?;
        tx.commit().await.map_err(|e| db_err("terms.ack", e))
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"policy.current"`.
fn db_err(context: &'static str, e: sqlx::Error) -> PrivacyError {
    crate::db_error::classify_and_log(context, e).into()
}
