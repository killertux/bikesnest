//! Pure SMTP email provider (`EMAIL_PROVIDER=smtp`) built on `lettre`.
//!
//! Dev/compose points this at **Mailpit** (no TLS, no credentials); a real
//! relay uses `SMTP_TLS=true` (STARTTLS) plus credentials. Swapping backends is
//! a config change, not a code change.

use crate::config::{ConfigError, EmailConfig};

use crate::email::templates::render;
use async_trait::async_trait;
use bikesnest_application::{EmailError, EmailMessage, EmailProvider};
use lettre::message::{Mailbox, MultiPart};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

type Mailer = AsyncSmtpTransport<Tokio1Executor>;

#[derive(Clone)]
pub struct SmtpEmailProvider {
    mailer: Mailer,
    from: String,
}

impl SmtpEmailProvider {
    pub fn new(
        host: String,
        port: u16,
        username: String,
        password: String,
        from: String,
        tls: bool,
    ) -> Result<Self, EmailError> {
        let mailer = build_mailer(host, port, username, password, tls)?;
        Ok(Self { mailer, from })
    }

    /// Build from the parsed `SMTP_*` block.
    pub fn from_config(config: &EmailConfig) -> Result<Self, ConfigError> {
        let EmailConfig::Smtp {
            host,
            port,
            username,
            password,
            tls,
            from,
        } = config
        else {
            return Err(ConfigError::invalid(
                "EMAIL_PROVIDER",
                "expected the smtp configuration",
            ));
        };
        Self::new(
            host.clone(),
            *port,
            username.clone(),
            password.clone(),
            from.clone(),
            *tls,
        )
        .map_err(|e| ConfigError::invalid("SMTP_HOST", e.to_string()))
    }

    /// Render `msg` in the recipient's locale and build the SMTP message.
    fn message(&self, msg: &EmailMessage) -> Result<Message, EmailError> {
        let from: Mailbox = self
            .from
            .parse()
            .map_err(|_| EmailError::Unexpected("invalid EMAIL_FROM".into()))?;
        let to: Mailbox = msg
            .to
            .parse()
            .map_err(|_| EmailError::Unexpected("invalid recipient email".into()))?;
        let rendered = render(msg);
        Message::builder()
            .from(from)
            .to(to)
            .subject(rendered.subject)
            .multipart(MultiPart::alternative_plain_html(
                rendered.text,
                rendered.html,
            ))
            .map_err(|_| EmailError::Unexpected("could not build SMTP message".into()))
    }
}

fn build_mailer(
    host: String,
    port: u16,
    username: String,
    password: String,
    tls: bool,
) -> Result<Mailer, EmailError> {
    let builder = if tls {
        AsyncSmtpTransport::<Tokio1Executor>::relay(&host)
            .map_err(|_| EmailError::Unexpected("invalid SMTP relay configuration".into()))?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&host)
    };
    let builder = builder.port(port);
    let builder = if username.is_empty() {
        builder
    } else {
        builder.credentials(Credentials::new(username, password))
    };
    Ok(builder.build())
}

#[async_trait]
impl EmailProvider for SmtpEmailProvider {
    async fn send(&self, msg: &EmailMessage) -> Result<(), EmailError> {
        let message = self.message(msg)?;
        self.mailer
            .send(message)
            .await
            .map_err(|_| EmailError::Unavailable)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bikesnest_application::EmailKind;
    use bikesnest_domain::LocaleCode;

    fn provider() -> SmtpEmailProvider {
        SmtpEmailProvider::new(
            "smtp.example.com".into(),
            1025,
            String::new(),
            String::new(),
            "no-reply@bikesnest.local".into(),
            false,
        )
        .unwrap()
    }

    /// The provider renders: what goes on the wire is the catalog text for the
    /// recipient's locale, not anything a caller passed in.
    #[test]
    fn builds_a_multipart_message_rendered_in_the_recipients_locale() {
        let message = EmailMessage::new(
            "x@example.com",
            LocaleCode::PtBr,
            EmailKind::ResetPassword {
                link: "https://bikesnest.test/password-reset/new?token=t".into(),
                expires_at: None,
            },
        );
        let built = provider().message(&message).unwrap();
        let bytes = built.formatted();
        let formatted = String::from_utf8_lossy(&bytes);
        assert!(formatted.contains("To: x@example.com"));
        assert!(formatted.contains("Content-Type: multipart/alternative"));
        assert!(formatted.contains("Content-Type: text/plain"));
        assert!(formatted.contains("Content-Type: text/html"));
        assert!(
            formatted.contains("=?utf-8?") || formatted.contains("Redefina sua senha"),
            "the pt-BR subject must be on the wire (encoded or literal): {formatted}"
        );
        // The body is quoted-printable (the pt-BR text is non-ASCII), so the
        // link's `=` arrives as `=3D` — match the part that survives encoding.
        assert!(formatted.contains("password-reset/new"), "{formatted}");
    }
}
