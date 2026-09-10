//! `EmailQueue` implementations: durable (via the job queue) and inline.
//!
//! [`JobEmailQueue`] is the durable implementation. It validates the current
//! account/token and inserts `background_job` in its own transaction, after the
//! caller's auth transition has committed. Provider delivery, retries and
//! dead-lettering then happen outside the request path. Authentication uses
//! its narrower transactional outbox port; this generic queue remains for
//! non-aggregate callers and direct queue tests.
//!
//! [`InlineEmailQueue`] sends on the spot. It exists for two reasons: tests
//! that want the message without running a worker, and deployments with
//! `JOBS_ENABLED=false` — where nothing would ever claim an `email.send` row,
//! so queuing the mail would be the same as dropping it.

use crate::Db;
use crate::auth::hash::sha256_hex;
use crate::email::token_hash_from_link;
use crate::job::{
    email::SendEmailHandler,
    repo::{MailEnqueue, SqlxJobRepository},
};
use async_trait::async_trait;
use bikesnest_application::{
    AdmittedAuthMail, AuthError, AuthMailDispatcher, EmailError, EmailMessage, EmailProvider,
    EmailQueue, JobHandler,
};
use std::sync::Arc;

/// Enqueue-time idempotency key for one message: `email:{kind}:{sha256(link)}`.
///
/// The link embeds the single-use token, so the key identifies exactly "this
/// message about this token". A retried enqueue of the same token — a
/// double-submitted form, a retried request — collapses onto the existing row.
/// This deduplicates queue admission, not provider delivery: lease expiry or an
/// ambiguous provider outcome can still cause an at-least-once duplicate. A
/// *fresh* token (a real re-send) has a different link and therefore a new job.
pub fn idempotency_key(msg: &EmailMessage) -> String {
    format!(
        "email:{}:{}",
        msg.kind.code(),
        sha256_hex(msg.kind.link().as_bytes())
    )
}

/// Durable delivery: one `email.send` job per message.
#[derive(Clone)]
pub struct JobEmailQueue {
    jobs: SqlxJobRepository,
    max_attempts: i32,
}

#[derive(Clone, Default)]
pub struct DurableAuthMailDispatcher;

#[async_trait]
impl AuthMailDispatcher for DurableAuthMailDispatcher {
    async fn dispatch(&self, _mail: AdmittedAuthMail) -> Result<(), AuthError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct InlineAuthMailDispatcher {
    jobs: SqlxJobRepository,
    handler: Arc<SendEmailHandler>,
}

impl InlineAuthMailDispatcher {
    pub fn new(db: Db, provider: Arc<dyn EmailProvider>) -> Self {
        Self {
            jobs: SqlxJobRepository::new(db.clone()),
            handler: Arc::new(SendEmailHandler::new(db, provider)),
        }
    }
}

#[async_trait]
impl AuthMailDispatcher for InlineAuthMailDispatcher {
    async fn dispatch(&self, mail: AdmittedAuthMail) -> Result<(), AuthError> {
        static INLINE_CLAIM_SEQUENCE: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        let sequence = INLINE_CLAIM_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let owner = format!(
            "inline-{}-{}-{nonce}-{sequence}",
            std::process::id(),
            mail.job_id
        );
        let Some(job) = self
            .jobs
            .claim_id(mail.job_id, &owner, std::time::Duration::from_secs(30))
            .await
            .map_err(|_| AuthError::Unavailable)?
        else {
            return match self.jobs.mail_dispatch_state(mail.job_id).await {
                Ok(Some((state, _, _))) if state == "succeeded" => Ok(()),
                Ok(Some((state, _, active))) if state == "running" && active => Ok(()),
                Ok(Some((state, due, _))) if state == "pending" && !due => {
                    Err(AuthError::Unavailable)
                }
                Ok(Some(_)) => Err(AuthError::Internal),
                Ok(None) => Err(AuthError::Internal),
                Err(_) => Err(AuthError::Unavailable),
            };
        };
        match self.handler.run(&job.payload).await {
            Ok(()) => self
                .jobs
                .finish_success(job.id, &owner, None, chrono::Utc::now())
                .await
                .map_err(|_| AuthError::Unavailable)?,
            Err(bikesnest_application::JobError::Permanent(error)) => {
                self.handler.on_dead_letter(&job.payload, &error).await;
                self.jobs
                    .fail(job.id, &owner, &error)
                    .await
                    .map_err(|_| AuthError::Unavailable)?;
                return Err(AuthError::Internal);
            }
            Err(error) if job.attempts < job.max_attempts => {
                self.jobs
                    .retry(
                        job.id,
                        &owner,
                        &error.to_string(),
                        chrono::Utc::now() + chrono::Duration::seconds(2),
                    )
                    .await
                    .map_err(|_| AuthError::Unavailable)?;
                return Err(AuthError::Unavailable);
            }
            Err(error) => {
                self.handler
                    .on_dead_letter(&job.payload, &error.to_string())
                    .await;
                self.jobs
                    .fail(job.id, &owner, &error.to_string())
                    .await
                    .map_err(|_| AuthError::Unavailable)?;
                return Err(AuthError::Internal);
            }
        }
        Ok(())
    }
}

impl JobEmailQueue {
    pub fn new(jobs: SqlxJobRepository, max_attempts: i32) -> Self {
        Self { jobs, max_attempts }
    }
}

#[async_trait]
impl EmailQueue for JobEmailQueue {
    async fn enqueue(&self, msg: EmailMessage) -> Result<(), EmailError> {
        let payload = serde_json::to_value(&msg)
            .map_err(|_| EmailError::Unexpected("email payload encoding failed".into()))?;
        let key = idempotency_key(&msg);
        let token_hash = token_hash_from_link(msg.kind.link())
            .ok_or_else(|| EmailError::Unexpected("email token reference missing".into()))?;
        let queued = self
            .jobs
            .enqueue_mail(MailEnqueue {
                payload: &payload,
                account_id: msg.account_id,
                token_hash: &token_hash,
                purpose: msg.kind.code(),
                recipient: &msg.to,
                run_at: chrono::Utc::now(),
                max_attempts: self.max_attempts,
                idempotency_key: &key,
            })
            .await
            .map_err(|e| {
                // The caller turns this into a failed request: better than
                // telling someone to check an inbox nothing will arrive in.
                let reason = match e {
                    crate::job::JobRepoError::InvalidMailCredential => "invalid_credential",
                    crate::job::JobRepoError::Db(_) => "database_unavailable",
                    _ => "queue_error",
                };
                tracing::error!(
                    kind = msg.kind.code(),
                    reason,
                    "could not queue transactional email"
                );
                EmailError::Unavailable
            })?;
        match queued {
            Some(id) => tracing::debug!(job = id, kind = msg.kind.code(), "email queued"),
            // The idempotency key already existed: the same message for the
            // same token is pending or was delivered. Not an error.
            None => tracing::debug!(
                kind = msg.kind.code(),
                "email for this token is already queued; not enqueued twice"
            ),
        }
        Ok(())
    }
}

/// Immediate delivery on the calling task. Used by tests and by deployments
/// that run without the background worker.
#[derive(Clone)]
pub struct InlineEmailQueue {
    handler: Arc<SendEmailHandler>,
}

impl InlineEmailQueue {
    pub fn new(db: Db, provider: Arc<dyn EmailProvider>) -> Self {
        Self {
            handler: Arc::new(SendEmailHandler::new(db, provider)),
        }
    }
}

#[async_trait]
impl EmailQueue for InlineEmailQueue {
    async fn enqueue(&self, msg: EmailMessage) -> Result<(), EmailError> {
        let payload = serde_json::to_value(msg)
            .map_err(|_| EmailError::Unexpected("email payload encoding failed".into()))?;
        self.handler.run(&payload).await.map_err(|e| match e {
            bikesnest_application::JobError::Failed(_) => EmailError::Unavailable,
            bikesnest_application::JobError::Permanent(_) => {
                EmailError::Unexpected("mail credential is no longer deliverable".into())
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bikesnest_application::EmailKind;
    use bikesnest_domain::LocaleCode;

    fn msg(kind: EmailKind) -> EmailMessage {
        EmailMessage::linked(
            bikesnest_domain::UserId(7),
            "ada@example.com",
            LocaleCode::PtBr,
            kind,
        )
    }

    #[test]
    fn the_key_is_per_kind_and_per_token() {
        let verify = msg(EmailKind::VerifyEmail {
            link: "https://x/verify-email?token=aaa".into(),
        });
        let same_again = msg(EmailKind::VerifyEmail {
            link: "https://x/verify-email?token=aaa".into(),
        });
        let other_token = msg(EmailKind::VerifyEmail {
            link: "https://x/verify-email?token=bbb".into(),
        });
        let other_kind = msg(EmailKind::ResetPassword {
            link: "https://x/verify-email?token=aaa".into(),
        });

        // Same message twice → one key → the second enqueue is a no-op.
        assert_eq!(idempotency_key(&verify), idempotency_key(&same_again));
        // A re-send issues a new token, and that must be a new job.
        assert_ne!(idempotency_key(&verify), idempotency_key(&other_token));
        // Two different messages about one token stay independent.
        assert_ne!(idempotency_key(&verify), idempotency_key(&other_kind));

        // Shape: no raw token or address in the key (it is stored in a column
        // that is not treated as secret).
        let key = idempotency_key(&verify);
        assert!(key.starts_with("email:verify:"), "{key}");
        assert!(!key.contains("aaa"), "the token must be hashed, not copied");
        assert_eq!(key.len(), "email:verify:".len() + 64);
    }
}
