//! Transactional email ports.
//!
//! The application layer describes *what* to send — recipient, locale, kind,
//! and either a credential link or credential-free notice identity — and never
//! *how it reads*: subject and body come from
//! the message catalog at render time, in the recipient's own language. That is
//! the i18n rule (no user-facing strings outside the catalog) applied to mail.
//!
//! Two ports, deliberately separate:
//!
//! - [`EmailQueue`] supports generic callers. Authentication uses `AuthOutbox`
//!   so its transition, audit, and durable `email.send` admission commit in one
//!   transaction. Inline dispatch may attempt the exact admitted job after
//!   commit, but never makes provider I/O part of that transaction.
//! - [`EmailProvider`] is what actually talks to a relay/ESP. Only the job
//!   handler (and the inline queue used when the worker is disabled) calls it.

use async_trait::async_trait;
use bikesnest_domain::{LocaleCode, UserId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum EmailError {
    #[error("mail provider unavailable")]
    Unavailable,
    #[error("mail provider error: {0}")]
    Unexpected(String),
    #[error("mail provider permanently rejected the request")]
    Permanent,
}

/// Which transactional message this is. The variant chooses the catalog keys;
/// its payload carries either a single-use credential link or an immutable,
/// credential-free security-notice identity.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmailKind {
    /// Registration (and re-send): confirm the address on a pending account.
    VerifyEmail {
        link: String,
        #[serde(default)]
        expires_at: Option<DateTime<Utc>>,
    },
    /// Password reset: choose a new password.
    ResetPassword {
        link: String,
        #[serde(default)]
        expires_at: Option<DateTime<Utc>>,
    },
    /// Email change: confirm the *new* address before it becomes canonical.
    ConfirmEmailChange {
        link: String,
        #[serde(default)]
        expires_at: Option<DateTime<Utc>>,
    },
    /// A password was successfully replaced. Carries no credential.
    PasswordChanged {
        account_link: String,
        notification_id: String,
    },
    /// The canonical email was successfully changed; sent to the old address.
    EmailAddressChanged {
        account_link: String,
        notification_id: String,
    },
}

impl std::fmt::Debug for EmailKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailKind")
            .field("code", &self.code())
            .field("link", &"[REDACTED]")
            .finish()
    }
}

impl EmailKind {
    /// Stable short code for logs and the enqueue idempotency key. Never
    /// user-facing, so it is not a catalog string.
    pub fn code(&self) -> &'static str {
        match self {
            EmailKind::VerifyEmail { .. } => "verify",
            EmailKind::ResetPassword { .. } => "reset",
            EmailKind::ConfirmEmailChange { .. } => "change",
            EmailKind::PasswordChanged { .. } => "password_changed",
            EmailKind::EmailAddressChanged { .. } => "email_changed",
        }
    }

    /// The CTA/fallback URL. Security-notice URLs carry no credential.
    pub fn action_link(&self) -> &str {
        match self {
            EmailKind::VerifyEmail { link, .. }
            | EmailKind::ResetPassword { link, .. }
            | EmailKind::ConfirmEmailChange { link, .. } => link,
            EmailKind::PasswordChanged { account_link, .. }
            | EmailKind::EmailAddressChanged { account_link, .. } => account_link,
        }
    }
    /// Return a single-use credential only for action mails. Security notices
    /// deliberately have no credential-bearing destination.
    pub fn credential_link(&self) -> Option<&str> {
        match self {
            EmailKind::VerifyEmail { link, .. }
            | EmailKind::ResetPassword { link, .. }
            | EmailKind::ConfirmEmailChange { link, .. } => Some(link),
            EmailKind::PasswordChanged { .. } | EmailKind::EmailAddressChanged { .. } => None,
        }
    }

    pub fn expires_at(&self) -> Option<DateTime<Utc>> {
        match self {
            EmailKind::VerifyEmail { expires_at, .. }
            | EmailKind::ResetPassword { expires_at, .. }
            | EmailKind::ConfirmEmailChange { expires_at, .. } => *expires_at,
            EmailKind::PasswordChanged { .. } | EmailKind::EmailAddressChanged { .. } => None,
        }
    }

    /// Stable material for queue/provider replay; never rendered or logged.
    pub fn idempotency_material(&self) -> &str {
        match self {
            EmailKind::VerifyEmail { link, .. }
            | EmailKind::ResetPassword { link, .. }
            | EmailKind::ConfirmEmailChange { link, .. } => link,
            EmailKind::PasswordChanged {
                notification_id, ..
            }
            | EmailKind::EmailAddressChanged {
                notification_id, ..
            } => notification_id,
        }
    }
}

/// One transactional email, still unrendered: the provider (or the job handler
/// behind it) turns `kind` + `locale` into a subject and body.
///
/// This is also the `email.send` job payload, hence the serde derives. The
/// recipient is a plain `String` because a queued payload is round-tripped
/// through JSON, and the address was already validated when it was accepted.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmailMessage {
    pub account_id: i64,
    pub to: String,
    #[serde(with = "locale_code")]
    pub locale: LocaleCode,
    #[serde(flatten)]
    pub kind: EmailKind,
}

impl EmailMessage {
    pub fn new(to: impl Into<String>, locale: LocaleCode, kind: EmailKind) -> Self {
        Self::linked(UserId(0), to, locale, kind)
    }

    pub fn linked(
        account_id: UserId,
        to: impl Into<String>,
        locale: LocaleCode,
        kind: EmailKind,
    ) -> Self {
        Self {
            account_id: account_id.0,
            to: to.into(),
            locale,
            kind,
        }
    }
}

impl std::fmt::Debug for EmailMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailMessage")
            .field("account_id", &self.account_id)
            .field("locale", &self.locale.as_str())
            .field("kind", &self.kind.code())
            .finish()
    }
}

/// `LocaleCode` as its canonical code in JSON, so the domain type needs no
/// serde dependency and a stored payload stays readable (`"locale":"pt-BR"`).
mod locale_code {
    use bikesnest_domain::LocaleCode;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(code: &LocaleCode, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(code.as_str())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<LocaleCode, D::Error> {
        let raw = String::deserialize(d)?;
        LocaleCode::parse(&raw).ok_or_else(|| serde::de::Error::custom("unknown locale code"))
    }
}

/// Port: hand a message off for delivery.
///
/// Called by use cases *inside* the request. The durable implementation
/// enqueues an `email.send` job (one INSERT in the application's own database)
/// and lets the worker's retry/backoff/dead-letter budget handle the provider;
/// an inline implementation exists for tests and for deployments that run
/// without the worker.
#[async_trait]
pub trait EmailQueue: Send + Sync {
    async fn enqueue(&self, msg: EmailMessage) -> Result<(), EmailError>;
}

/// Port: send one transactional email through a relay/ESP. The implementation
/// renders `msg` from the catalog before handing it over.
#[async_trait]
pub trait EmailProvider: Send + Sync {
    async fn send(&self, msg: &EmailMessage) -> Result<(), EmailError>;
    /// Provider-supported replay key. SMTP implementations deliberately fall
    /// back to `send`: SMTP has no portable idempotency contract.
    async fn send_idempotent(
        &self,
        msg: &EmailMessage,
        _idempotency_key: &str,
    ) -> Result<(), EmailError> {
        self.send(msg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_round_trips_through_the_job_payload() {
        let msg = EmailMessage::linked(
            UserId(7),
            "ada@example.com",
            LocaleCode::PtBr,
            EmailKind::VerifyEmail {
                link: "http://localhost:8080/verify-email?token=abc".into(),
                expires_at: None,
            },
        );
        let json = serde_json::to_value(&msg).unwrap();
        // Readable, stable payload shape: a queued row stays inspectable.
        assert_eq!(json["locale"], "pt-BR");
        assert_eq!(json["kind"], "verify_email");
        assert_eq!(serde_json::from_value::<EmailMessage>(json).unwrap(), msg);
    }

    #[test]
    fn purpose_code_is_a_bounded_diagnostic() {
        let msg = EmailMessage::linked(
            UserId(7),
            "ada@example.com",
            LocaleCode::En,
            EmailKind::ResetPassword {
                link: "x".into(),
                expires_at: None,
            },
        );
        assert_eq!(msg.kind.code(), "reset");
    }

    #[test]
    fn an_unknown_locale_code_is_rejected_rather_than_defaulted() {
        let json = serde_json::json!({
            "to": "a@example.com",
            "locale": "fr",
            "kind": "verify_email",
            "link": "x",
        });
        let error = serde_json::from_value::<EmailMessage>(json)
            .unwrap_err()
            .to_string();
        assert!(!error.contains("fr"));
    }

    #[test]
    fn debug_output_redacts_recipient_and_link() {
        let marker = "SECRET-MARKER.invalid";
        let msg = EmailMessage::linked(
            UserId(9),
            format!("ada@{marker}"),
            LocaleCode::En,
            EmailKind::ResetPassword {
                link: format!("https://x/?token={marker}"),
                expires_at: None,
            },
        );
        let debug = format!("{msg:?}");
        assert!(!debug.contains(marker));
        assert!(debug.contains("reset"));
    }
}
