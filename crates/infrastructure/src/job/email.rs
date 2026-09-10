//! The `email.send` job handler: one queued message → one provider call.
//!
//! Retries, backoff and dead-lettering come from the queue, so this handler
//! only has to classify: a provider error is transient (`Failed`, retry within
//! the budget), an undecodable payload is not (`Permanent` — no number of
//! retries will make it parse).
//!
//! At-least-once execution is real here, and unlike a purge or an upsert a send
//! cannot be undone. What keeps a user from getting the same mail twice is the
//! enqueue-time idempotency key (`email:{kind}:{sha256(link)}`): the row for a
//! given token exists once, so the message is *queued* once. A lease that
//! expires mid-send can still deliver twice — the alternative (marking sent
//! before sending) drops mail instead, and a duplicate verification link is
//! the better failure.

use crate::Db;
use crate::email::token_hash_from_link;
use async_trait::async_trait;
use bikesnest_application::{
    EmailMessage, EmailProvider, JOB_EMAIL_SEND, JobError, JobHandler, JobPayload,
};
use std::sync::Arc;

pub struct SendEmailHandler {
    db: Db,
    provider: Arc<dyn EmailProvider>,
}

impl SendEmailHandler {
    pub fn new(db: Db, provider: Arc<dyn EmailProvider>) -> Self {
        Self { db, provider }
    }
}

#[async_trait]
impl JobHandler for SendEmailHandler {
    fn kind(&self) -> &'static str {
        JOB_EMAIL_SEND
    }

    async fn run(&self, payload: &JobPayload) -> Result<(), JobError> {
        let msg = decode(payload)?;
        let token_hash = token_hash_from_link(msg.kind.link())
            .ok_or_else(|| JobError::Permanent("mail link is invalid".into()))?;
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
        // Longer than the bounded provider call, so PostgreSQL cannot release
        // the account lock while a healthy delivery is in flight.
        sqlx::query("SET LOCAL idle_in_transaction_session_timeout = '15s'")
            .execute(&mut *tx)
            .await
            .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
        let state: Option<String> =
            sqlx::query_scalar("SELECT account_state::text FROM users WHERE id = $1 FOR UPDATE")
                .bind(msg.account_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
        let eligible = match (msg.kind.code(), state.as_deref()) {
            ("verify", Some("PENDING_EMAIL_VERIFICATION" | "ACTIVE")) => {
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM email_verification_tokens WHERE token_hash=$1 AND user_id=$2 AND email=$3 AND used_at IS NULL AND expires_at > clock_timestamp())")
                    .bind(&token_hash).bind(msg.account_id).bind(&msg.to).fetch_one(&mut *tx).await
            }
            ("change", Some("ACTIVE")) => {
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM email_verification_tokens WHERE token_hash=$1 AND user_id=$2 AND email=$3 AND used_at IS NULL AND expires_at > clock_timestamp())")
                    .bind(&token_hash).bind(msg.account_id).bind(&msg.to).fetch_one(&mut *tx).await
            }
            ("reset", Some("PENDING_EMAIL_VERIFICATION" | "ACTIVE")) => {
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM password_reset_tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=$1 AND t.user_id=$2 AND u.email=$3 AND t.used_at IS NULL AND t.expires_at > clock_timestamp())")
                    .bind(&token_hash).bind(msg.account_id).bind(&msg.to).fetch_one(&mut *tx).await
            }
            _ => Ok(false),
        }.map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
        if !eligible {
            tx.rollback()
                .await
                .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
            return Err(JobError::Permanent(
                "mail credential is no longer deliverable".into(),
            ));
        }
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(10), self.provider.send(&msg))
                .await;
        match result {
            Ok(Ok(())) => tx.commit().await.map_err(|_| {
                JobError::Failed("mail delivery outcome could not be recorded".into())
            }),
            Ok(Err(_)) => {
                tx.rollback()
                    .await
                    .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
                Err(JobError::Failed(format!(
                    "{} mail provider failed",
                    msg.kind.code()
                )))
            }
            Err(_) => {
                tx.rollback()
                    .await
                    .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
                Err(JobError::Failed(
                    "mail provider timed out; delivery outcome unknown".into(),
                ))
            }
        }
    }

    /// A dead-lettered email is a user who is stuck: no verification link, no
    /// password reset. Log it at `error!` so it is alertable — with the message
    /// allowlisted message kind only. The address, domain, provider response
    /// and link are untrusted or personal; none belongs in this log line.
    async fn on_dead_letter(&self, payload: &JobPayload, _error: &str) {
        match decode(payload) {
            Ok(msg) => tracing::error!(
                kind = msg.kind.code(),
                "transactional email dead-lettered; the recipient never got it"
            ),
            Err(_) => tracing::error!("email.send dead-lettered with an unreadable payload"),
        }
    }
}

/// Decode the queued payload. A shape mismatch means the row was written by
/// another version of the app (or by hand): permanent, not retryable.
fn decode(payload: &JobPayload) -> Result<EmailMessage, JobError> {
    serde_json::from_value(payload.clone())
        .map_err(|_| JobError::Permanent("unreadable email.send payload".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unreadable_payload_errors_are_bounded_and_secret_free() {
        let err = decode(&serde_json::json!({"locale": "SECRET-MARKER"})).unwrap_err();
        assert!(matches!(err, JobError::Permanent(_)), "{err:?}");
        assert_eq!(err.to_string(), "unreadable email.send payload");
    }
}
