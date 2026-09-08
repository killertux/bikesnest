//! SQL-backed server-side session store. The cookie carries the raw id;
//! the DB stores its SHA-256 hash. `resolve` applies idle (30-day) + absolute
//! (90-day) expiry and slides `last_seen_at` forward.
//!
//! `resolve` runs on every authenticated request, so it is a single statement
//! and its `last_seen_at` write is throttled to at most once per
//! [`LAST_SEEN_REFRESH`]: the stored value may lag reality by that much, which
//! against a 30-day idle window is immaterial.

use crate::Db;
use crate::auth::hash::sha256_hex;
use async_trait::async_trait;
use bikesnest_application::{AuthError, Session, SessionStore};
use bikesnest_domain::{CsrfToken, SessionId, UserId};
use chrono::{DateTime, Duration, Utc};

const INACTIVE_IDLE: Duration = Duration::days(30);
const ABSOLUTE_CAP: Duration = Duration::days(90);
/// How stale `last_seen_at` may get before `resolve` writes it again. Idle
/// expiry is 30 days, so a five-minute staleness moves the practical idle
/// deadline by at most five minutes while removing one `UPDATE` per
/// authenticated request (the auth middleware resolves the session on every
/// request, GETs included).
const LAST_SEEN_REFRESH: &str = "5 minutes";

pub struct SqlxSessionStore {
    db: Db,
}

impl SqlxSessionStore {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct SessionRow {
    user_id: i64,
    csrf_token: String,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

#[async_trait]
impl SessionStore for SqlxSessionStore {
    async fn create(
        &self,
        user_id: UserId,
        raw: &SessionId,
        csrf: &CsrfToken,
        now: DateTime<Utc>,
    ) -> Result<(), AuthError> {
        let token_hash = sha256_hex(raw.as_bytes());
        let expires_at = now + ABSOLUTE_CAP;
        sqlx::query(
            r#"
            INSERT INTO sessions (token_hash, user_id, csrf_token, expires_at)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(token_hash)
        .bind(user_id.0)
        .bind(csrf.to_base64url())
        .bind(expires_at)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("session.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("session.create", e))?;
        Ok(())
    }

    async fn resolve(
        &self,
        raw: &SessionId,
        now: DateTime<Utc>,
    ) -> Result<Option<Session>, AuthError> {
        let token_hash = sha256_hex(raw.as_bytes());
        // One round trip: read the session and, in the same statement, slide
        // `last_seen_at` forward — but only when it is already stale by more
        // than LAST_SEEN_REFRESH. The auth middleware resolves the session on
        // every authenticated request, so an unconditional UPDATE turned every
        // page view (and every asset request behind auth) into a row write.
        //
        // The CTEs share one snapshot, so `s` still returns the *pre-update*
        // `last_seen_at`; the returned session is exactly the row as read. A
        // data-modifying CTE always runs to completion, whether or not the
        // outer query reads it.
        let sql = format!(
            r#"
            WITH s AS (
                SELECT user_id, csrf_token, created_at,
                       last_seen_at, expires_at, revoked_at
                FROM sessions
                WHERE token_hash = $1
            ),
            u AS (
                UPDATE sessions
                   SET last_seen_at = $2
                 WHERE token_hash = $1
                   AND revoked_at IS NULL
                   AND expires_at >= $2
                   AND last_seen_at >  $2 - interval '{idle} days'
                   AND last_seen_at <  $2 - interval '{refresh}'
                RETURNING 1
            )
            SELECT user_id, csrf_token, created_at,
                   last_seen_at, expires_at, revoked_at
            FROM s
            "#,
            idle = INACTIVE_IDLE.num_days(),
            refresh = LAST_SEEN_REFRESH,
        );
        let row = sqlx::query_as::<_, SessionRow>(&sql)
            .bind(token_hash)
            .bind(now)
            .fetch_optional(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("session.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("session.resolve", e))?;
        let Some(row) = row else {
            return Ok(None);
        };
        if row.revoked_at.is_some() {
            return Ok(None);
        }
        if now > row.expires_at {
            return Ok(None);
        }
        if now - row.last_seen_at > INACTIVE_IDLE {
            return Ok(None);
        }
        let csrf_token = CsrfToken::from_base64url(&row.csrf_token).ok_or(AuthError::Internal)?;
        Ok(Some(Session {
            user_id: UserId(row.user_id),
            csrf_token,
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            expires_at: row.expires_at,
            revoked_at: row.revoked_at,
        }))
    }

    async fn revoke(&self, raw: &SessionId) -> Result<(), AuthError> {
        let token_hash = sha256_hex(raw.as_bytes());
        sqlx::query(
            "UPDATE sessions SET revoked_at = now() WHERE token_hash = $1 AND revoked_at IS NULL",
        )
        .bind(token_hash)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("session.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("session.revoke", e))?;
        Ok(())
    }

    async fn revoke_all_for_user_except(
        &self,
        user_id: UserId,
        keep: &SessionId,
    ) -> Result<(), AuthError> {
        let keep_hash = sha256_hex(keep.as_bytes());
        sqlx::query(
            "UPDATE sessions SET revoked_at = now()
             WHERE user_id = $1 AND token_hash != $2 AND revoked_at IS NULL",
        )
        .bind(user_id.0)
        .bind(keep_hash)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("session.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("session.revoke_all_for_user_except", e))?;
        Ok(())
    }

    async fn revoke_all_for_user(&self, user_id: UserId) -> Result<(), AuthError> {
        sqlx::query(
            "UPDATE sessions SET revoked_at = now()
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id.0)
        .execute(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("session.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("session.revoke_all_for_user", e))?;
        Ok(())
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// [`AuthError`]. `context` names the operation, e.g. `"session.create"`.
fn db_err(context: &'static str, e: sqlx::Error) -> AuthError {
    crate::db_error::classify_and_log(context, e).into()
}
