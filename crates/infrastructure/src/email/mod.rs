//! Transactional email: the provider implementations, the two `EmailQueue`
//! implementations, and the renderer they share.
//!
//! The provider is selected at wiring time from the parsed `EMAIL_PROVIDER`
//! setting (`fake` | `smtp` | `resend`), so swapping backends is a
//! configuration change rather than a domain/app change. Whichever one is
//! chosen, it renders the message from the catalog in the recipient's locale
//! (`templates::render`) — no caller ever supplies a subject or a body.

pub mod fake;
pub mod queue;
pub mod resend;
pub mod smtp;
pub mod templates;

pub use fake::{CapturedEmail, FakeEmailProvider};
pub use queue::{
    DurableAuthMailDispatcher, InlineAuthMailDispatcher, InlineEmailQueue, JobEmailQueue,
    idempotency_key,
};
pub use resend::ResendEmailProvider;
pub use smtp::SmtpEmailProvider;
pub use templates::{APP_NAME, RenderedEmail, render};

pub(crate) fn token_hash_from_link(link: &str) -> Option<String> {
    let parsed = url::Url::parse(link).ok()?;
    let encoded = parsed
        .query_pairs()
        .find_map(|(key, value)| (key == "token").then_some(value))?;
    let token = bikesnest_domain::VerificationToken::from_base64url(&encoded)?;
    Some(crate::auth::hash::sha256_hex(token.as_bytes()))
}

use crate::config::{ConfigError, EmailConfig};

/// Build the email provider the parsed configuration selected.
///
/// There is no fallback: an SMTP relay that cannot be reached is a startup
/// error, because a silent downgrade to the in-memory fake makes every
/// verification and password-reset message disappear. The fake is chosen only
/// when the configuration asked for it (development default).
pub fn from_config(
    config: &EmailConfig,
) -> Result<Box<dyn bikesnest_application::EmailProvider>, ConfigError> {
    match config {
        EmailConfig::Fake { outbox_root } => {
            Ok(Box::new(FakeEmailProvider::with_root(outbox_root.clone())))
        }
        EmailConfig::Smtp { .. } => Ok(Box::new(SmtpEmailProvider::from_config(config)?)),
        EmailConfig::Resend { .. } => Ok(Box::new(ResendEmailProvider::from_config(config)?)),
    }
}
