//! The `email.send` job handler: one queued message → one provider call.
//!
//! Retries, backoff and dead-lettering come from the queue, so this handler
//! only has to classify: a provider error is transient (`Failed`, retry within
//! the budget), an undecodable payload is not (`Permanent` — no number of
//! retries will make it parse).
//!
//! At-least-once execution is real here, and unlike a purge or an upsert a send
//! cannot be undone. What keeps a user from getting the same mail twice is the
//! enqueue-time idempotency key: a credential or immutable notification id is
//! admitted once. A lease that expires mid-send can still deliver twice; SMTP
//! offers no exactly-once guarantee and provider idempotency is time-bounded.

use crate::Db;
use crate::auth::hash::sha256_hex;
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
        let account: Option<(String, String)> =
            sqlx::query_as("SELECT account_state::text,email FROM users WHERE id = $1 FOR UPDATE")
                .bind(msg.account_id)
                .fetch_optional(&mut *tx)
                .await
                .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
        let delivery_key = crate::email::idempotency_key(&msg);
        let metadata = sqlx::query_as::<_, (Option<String>, Option<String>, Option<i64>)>(
            "SELECT mail_token_hash, mail_recipient_hash, mail_transition_audit_id
             FROM background_job
             WHERE kind = 'email.send' AND idempotency_key = $1
               AND mail_account_id = $2 AND mail_purpose = $3",
        )
        .bind(&delivery_key)
        .bind(msg.account_id)
        .bind(msg.kind.code())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
        // InlineEmailQueue deliberately has no durable row. Preserve that
        // worker-disabled compatibility only for credential mail, whose token
        // is still checked authoritatively below. Security notices always
        // require their linked audit/outbox metadata.
        let metadata = metadata.or_else(|| {
            msg.kind
                .credential_link()
                .and_then(token_hash_from_link)
                .map(|token_hash| (Some(token_hash), None, None))
        });
        let recipient_hash = sha256_hex(msg.to.trim().to_ascii_lowercase().as_bytes());
        let state = account.as_ref().map(|(state, _)| state.as_str());
        let canonical_email = account.as_ref().map(|(_, email)| email.as_str());
        let eligible = match (msg.kind.code(), state, metadata) {
            ("verify", Some("PENDING_EMAIL_VERIFICATION" | "ACTIVE"), Some((Some(token_hash), stored_hash, None))) if stored_hash.as_deref().is_none_or(|value| value == recipient_hash) => {
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM email_verification_tokens WHERE token_hash=$1 AND user_id=$2 AND email=$3 AND used_at IS NULL AND expires_at > clock_timestamp())")
                    .bind(&token_hash).bind(msg.account_id).bind(&msg.to).fetch_one(&mut *tx).await
            }
            ("change", Some("ACTIVE"), Some((Some(token_hash), stored_hash, None))) if stored_hash.as_deref().is_none_or(|value| value == recipient_hash) => {
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM email_verification_tokens WHERE token_hash=$1 AND user_id=$2 AND email=$3 AND used_at IS NULL AND expires_at > clock_timestamp())")
                    .bind(&token_hash).bind(msg.account_id).bind(&msg.to).fetch_one(&mut *tx).await
            }
            ("reset", Some("PENDING_EMAIL_VERIFICATION" | "ACTIVE"), Some((Some(token_hash), stored_hash, None))) if stored_hash.as_deref().is_none_or(|value| value == recipient_hash) => {
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM password_reset_tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=$1 AND t.user_id=$2 AND u.email=$3 AND t.used_at IS NULL AND t.expires_at > clock_timestamp())")
                    .bind(&token_hash).bind(msg.account_id).bind(&msg.to).fetch_one(&mut *tx).await
            }
            (purpose @ ("password_changed" | "email_changed"), account_state, Some((None, Some(stored_hash), Some(audit_id)))) if stored_hash == recipient_hash => {
                let allowed_state = match purpose {
                    "password_changed" => matches!(account_state, Some("PENDING_EMAIL_VERIFICATION" | "ACTIVE"))
                        && canonical_email.is_some_and(|email| email.eq_ignore_ascii_case(&msg.to)),
                    "email_changed" => account_state == Some("ACTIVE"),
                    _ => false,
                };
                if !allowed_state {
                    Ok(false)
                } else {
                    let action = if purpose == "password_changed" { "auth.password_changed" } else { "auth.email_changed" };
                    sqlx::query_scalar::<_, bool>(
                        "SELECT EXISTS(SELECT 1 FROM audit_events
                         WHERE id=$1 AND actor_user_id=$2 AND action=$3
                           AND target_type='user' AND target_id=$4 AND result='success')",
                    )
                    .bind(audit_id)
                    .bind(msg.account_id)
                    .bind(action)
                    .bind(msg.account_id.to_string())
                    .fetch_one(&mut *tx)
                    .await
                }
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
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            self.provider.send_idempotent(&msg, &delivery_key),
        )
        .await;
        match result {
            Ok(Ok(())) => tx.commit().await.map_err(|_| {
                JobError::Failed("mail delivery outcome could not be recorded".into())
            }),
            Ok(Err(bikesnest_application::EmailError::Permanent)) => {
                tx.rollback()
                    .await
                    .map_err(|_| JobError::Failed("mail lifecycle database unavailable".into()))?;
                Err(JobError::Permanent(format!(
                    "{} mail provider permanently rejected request",
                    msg.kind.code()
                )))
            }
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
                "transactional email exhausted its delivery retry budget"
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
        let JobError::Permanent(classification) = err else {
            panic!("decode must be permanent")
        };
        assert_eq!(classification, "unreadable email.send payload");
    }
}
