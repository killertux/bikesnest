//! In-memory capturing fake (`EMAIL_PROVIDER=fake`). Renders each message
//! exactly as the real providers do and records the result in memory
//! (inspectable in tests via [`FakeEmailProvider::emails`]) — so a test can
//! assert the subject a pt-BR recipient would actually have read. When given
//! an outbox root it also appends a capture to `<root>/outbox/` and
//! `tracing::info!`s the message, so the flow can be followed without a real
//! provider.

use crate::email::templates::render;
use async_trait::async_trait;
use bikesnest_application::{EmailError, EmailMessage, EmailProvider};
use std::path::PathBuf;
use std::sync::Mutex;

/// An email this fake has sent, as rendered.
#[derive(Debug, Clone)]
pub struct CapturedEmail {
    pub to: String,
    pub subject: String,
    pub text: String,
    /// Locale it was rendered in (`pt-BR` / `en`).
    pub locale: String,
    /// Message kind (`verify` / `reset` / `change`).
    pub kind: String,
}

#[derive(Clone)]
pub struct FakeEmailProvider {
    root: Option<PathBuf>,
    capture: std::sync::Arc<Mutex<Vec<CapturedEmail>>>,
}

impl FakeEmailProvider {
    /// A fake that only captures in memory. The dev outbox on disk is opt-in
    /// via [`FakeEmailProvider::with_root`], which the wiring passes the
    /// configured media root.
    pub fn new() -> Self {
        Self::with_root(None)
    }

    /// A fake with an explicit outbox root (or `None` for in-memory capture
    /// only — used by tests).
    pub fn with_root(root: Option<PathBuf>) -> Self {
        Self {
            root,
            capture: std::sync::Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Everything sent so far, in order.
    pub fn emails(&self) -> Vec<CapturedEmail> {
        self.capture.lock().map(|c| c.clone()).unwrap_or_default()
    }

    /// Extract the `token` query param from the first captured email whose text
    /// contains `path` (e.g. `/verify-email` or `/password-reset/new`).
    pub fn token_for(&self, path: &str) -> Option<String> {
        self.emails().iter().find_map(|e| token_from(&e.text, path))
    }

    /// The subject of the first captured message of `kind`, if any.
    pub fn subject_for_kind(&self, kind: &str) -> Option<String> {
        self.emails()
            .into_iter()
            .find(|e| e.kind == kind)
            .map(|e| e.subject)
    }

    async fn record(&self, msg: &EmailMessage) {
        let rendered = render(msg);
        let captured = CapturedEmail {
            to: msg.to.clone(),
            subject: rendered.subject,
            text: rendered.text,
            locale: msg.locale.as_str().to_string(),
            kind: msg.kind.code().to_string(),
        };
        if let Ok(mut cap) = self.capture.lock() {
            cap.push(captured.clone());
        }
        if let Some(root) = &self.root {
            static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = root.join("outbox");
            let file = dir.join(format!(
                "{}-{n}.txt",
                chrono::Utc::now().format("%Y%m%d%H%M%S")
            ));
            let body = format!(
                "To: {}\nSubject: {}\n\n{}\n",
                captured.to, captured.subject, captured.text
            );
            if tokio::fs::create_dir_all(&dir).await.is_ok()
                && tokio::fs::write(&file, body).await.is_ok()
            {
                tracing::info!(to = %captured.to, subject = %captured.subject, "fake email captured");
            }
        } else {
            tracing::info!(to = %captured.to, subject = %captured.subject, "fake email captured");
        }
    }
}

impl Default for FakeEmailProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EmailProvider for FakeEmailProvider {
    async fn send(&self, msg: &EmailMessage) -> Result<(), EmailError> {
        self.record(msg).await;
        Ok(())
    }
}

/// Pull the `token=` value out of a URL that sits within `text`, stopping at the
/// first non-base64url character (the token is URL-safe base64).
fn token_from(text: &str, path: &str) -> Option<String> {
    let url_start = text.find(path)?;
    let rest = &text[url_start..];
    let token_at = rest.find("token=")? + "token=".len();
    let mut end = token_at;
    for (i, b) in rest[token_at..].bytes().enumerate() {
        let is_token_char =
            matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.');
        if !is_token_char {
            end = token_at + i;
            break;
        }
        end = token_at + i + 1;
    }
    Some(rest[token_at..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bikesnest_application::EmailKind;
    use bikesnest_domain::LocaleCode;

    #[tokio::test]
    async fn captures_the_rendered_message_and_recovers_the_token() {
        let fake = FakeEmailProvider::with_root(None);
        fake.send(&EmailMessage::new(
            "a@example.com",
            LocaleCode::PtBr,
            EmailKind::VerifyEmail {
                link: "http://localhost:8080/verify-email?token=abc-123_XYZ".into(),
                expires_at: None,
            },
        ))
        .await
        .unwrap();

        assert_eq!(fake.emails().len(), 1);
        // What the fake stores is the *rendered* mail, in the message's locale.
        let captured = &fake.emails()[0];
        assert_eq!(captured.to, "a@example.com");
        assert_eq!(captured.locale, "pt-BR");
        assert_eq!(captured.kind, "verify");
        assert_eq!(captured.subject, "Confirme seu e-mail no BikesNest");
        assert_eq!(
            fake.token_for("/verify-email").as_deref(),
            Some("abc-123_XYZ")
        );
        assert_eq!(fake.token_for("/password-reset/new"), None);
    }
}
