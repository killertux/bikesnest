//! Resend API email provider (`EMAIL_PROVIDER=resend`). POSTs to
//! `https://api.resend.com/emails` with the account auth key.

use crate::config::{ConfigError, EmailConfig};

use crate::email::templates::render;
use async_trait::async_trait;
use bikesnest_application::{EmailError, EmailMessage, EmailProvider};
use reqwest::Client;

#[derive(Clone)]
pub struct ResendEmailProvider {
    client: Client,
    api_key: String,
    from: String,
}

impl ResendEmailProvider {
    pub fn new(api_key: impl Into<String>, from: impl Into<String>) -> Self {
        // 10s timeout: the send runs in a background job with its own retry
        // budget, so a hung request would hold a worker slot and a job lease
        // rather than a user's page — still not something to wait forever on.
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .expect("reqwest client");
        Self {
            client,
            api_key: api_key.into(),
            from: from.into(),
        }
    }

    /// Build from the parsed `RESEND_*` block.
    pub fn from_config(config: &EmailConfig) -> Result<Self, ConfigError> {
        let EmailConfig::Resend { api_key, from } = config else {
            return Err(ConfigError::invalid(
                "EMAIL_PROVIDER",
                "expected the resend configuration",
            ));
        };
        Ok(Self::new(api_key.clone(), from.clone()))
    }
}

impl ResendEmailProvider {
    /// The API body for `msg`, rendered in the recipient's locale.
    fn payload(&self, msg: &EmailMessage) -> serde_json::Value {
        let rendered = render(msg);
        serde_json::json!({
            "from": self.from,
            "to": [msg.to],
            "subject": rendered.subject,
            "text": rendered.text,
        })
    }
}

#[async_trait]
impl EmailProvider for ResendEmailProvider {
    async fn send(&self, msg: &EmailMessage) -> Result<(), EmailError> {
        let body = self.payload(msg);
        let res = self
            .client
            .post("https://api.resend.com/emails")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|_| EmailError::Unavailable)?;

        if res.status().is_success() {
            Ok(())
        } else {
            let status = res.status();
            Err(EmailError::Unexpected(format!(
                "resend API status {status}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bikesnest_application::EmailKind;
    use bikesnest_domain::LocaleCode;

    fn provider() -> ResendEmailProvider {
        ResendEmailProvider::new("test-key", "no-reply@bikesnest.local")
    }

    #[test]
    fn payload_carries_the_rendered_message() {
        let msg = EmailMessage::new(
            "a@example.com",
            LocaleCode::En,
            EmailKind::VerifyEmail {
                link: "https://bikesnest.test/verify-email?token=t".into(),
            },
        );
        let p = provider().payload(&msg);
        assert_eq!(p["from"], "no-reply@bikesnest.local");
        assert_eq!(p["to"][0], "a@example.com");
        assert_eq!(p["subject"], "Confirm your BikesNest email");
        assert!(p["text"].as_str().unwrap().contains("verify-email?token=t"));
        // Plain text only: there is no HTML template for any kind.
        assert!(p.get("html").is_none());
    }

    #[test]
    fn payload_is_rendered_in_the_recipients_locale() {
        let msg = EmailMessage::new(
            "a@example.com",
            LocaleCode::PtBr,
            EmailKind::VerifyEmail {
                link: "https://bikesnest.test/verify-email?token=t".into(),
            },
        );
        let p = provider().payload(&msg);
        assert_eq!(p["subject"], "Confirme seu e-mail no BikesNest");
    }
}
