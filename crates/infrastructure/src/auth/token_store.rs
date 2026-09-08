//! SQL-backed single-use token store. Tokens are stored as SHA-256
//! hashes; single-use is enforced atomically by the `used_at IS NULL` guard in
//! the `UPDATE … RETURNING` (no read-then-write race).

use crate::Db;
use crate::auth::hash::sha256_hex;
use async_trait::async_trait;
use bikesnest_application::{AuthError, TokenStore};
use bikesnest_domain::{AccountState, UserId, VerificationToken};
use chrono::{DateTime, Duration, Utc};

const VERIFICATION_TTL: Duration = Duration::hours(24);
const RESET_TTL: Duration = Duration::hours(1);

pub struct SqlxTokenStore {
    db: Db,
}

impl SqlxTokenStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl TokenStore for SqlxTokenStore {
    async fn issue_verification(
        &self,
        user_id: UserId,
        email: &str,
        raw: &VerificationToken,
        now: DateTime<Utc>,
        expected_state: AccountState,
    ) -> Result<bool, AuthError> {
        if !matches!(
            expected_state,
            AccountState::PendingEmailVerification | AccountState::Active
        ) {
            return Ok(false);
        }
        let token_hash = sha256_hex(raw.as_bytes());
        let expires_at = now + VERIFICATION_TTL;
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("token.issue_verification", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("token.issue_verification", e))?;
        let eligible = sqlx::query_scalar::<_, bool>(
            "SELECT account_state = $2 FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(user_id.0)
        .bind(expected_state.as_code())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("token.issue_verification", e))?
        .unwrap_or(false);
        if !eligible {
            tx.rollback()
                .await
                .map_err(|e| db_err("token.issue_verification", e))?;
            return Ok(false);
        }
        sqlx::query(
            r#"
            INSERT INTO email_verification_tokens (token_hash, user_id, email, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(token_hash)
        .bind(user_id.0)
        .bind(email)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("token.issue_verification", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("token.issue_verification", e))?;
        Ok(true)
    }

    async fn consume_verification(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<(UserId, String)>, AuthError> {
        let token_hash = sha256_hex(raw.as_bytes());
        #[derive(sqlx::FromRow)]
        struct Row {
            user_id: i64,
            email: String,
        }
        let row = sqlx::query_as::<_, Row>(
            r#"
            UPDATE email_verification_tokens
            SET used_at = $2
            WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2
            RETURNING user_id, email
            "#,
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("token.consume_verification", e))?,
        )
        .await
        .map_err(|e| db_err("token.consume_verification", e))?;
        Ok(row.map(|r| (UserId(r.user_id), r.email)))
    }

    async fn find_verification(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError> {
        let token_hash = sha256_hex(raw.as_bytes());
        sqlx::query_scalar(
            "SELECT user_id FROM email_verification_tokens
             WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2",
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("token.find_verification", e))?,
        )
        .await
        .map(|id| id.map(UserId))
        .map_err(|e| db_err("token.find_verification", e))
    }

    async fn issue_reset(
        &self,
        user_id: UserId,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<bool, AuthError> {
        let token_hash = sha256_hex(raw.as_bytes());
        let expires_at = now + RESET_TTL;
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("token.issue_reset", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("token.issue_reset", e))?;
        let eligible = sqlx::query_scalar::<_, bool>(
            "SELECT account_state IN ('PENDING_EMAIL_VERIFICATION', 'ACTIVE')
             FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(user_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("token.issue_reset", e))?
        .unwrap_or(false);
        if !eligible {
            tx.rollback()
                .await
                .map_err(|e| db_err("token.issue_reset", e))?;
            return Ok(false);
        }
        sqlx::query(
            r#"
            INSERT INTO password_reset_tokens (token_hash, user_id, expires_at)
            VALUES ($1, $2, $3)
            "#,
        )
        .bind(token_hash)
        .bind(user_id.0)
        .bind(expires_at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("token.issue_reset", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("token.issue_reset", e))?;
        Ok(true)
    }

    async fn consume_reset(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError> {
        let token_hash = sha256_hex(raw.as_bytes());
        #[derive(sqlx::FromRow)]
        struct Row {
            user_id: i64,
        }
        let row = sqlx::query_as::<_, Row>(
            r#"
            UPDATE password_reset_tokens
            SET used_at = $2
            WHERE token_hash = $1 AND used_at IS NULL AND expires_at > $2
            RETURNING user_id
            "#,
        )
        .bind(token_hash)
        .bind(now)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("token.consume_reset", e))?,
        )
        .await
        .map_err(|e| db_err("token.consume_reset", e))?;
        Ok(row.map(|r| UserId(r.user_id)))
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// [`AuthError`]. `context` names the operation, e.g. `"token.issue"`.
fn db_err(context: &'static str, e: sqlx::Error) -> AuthError {
    crate::db_error::classify_and_log(context, e).into()
}
