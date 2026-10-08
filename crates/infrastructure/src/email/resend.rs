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

fn retryable_response(status: reqwest::StatusCode, name: Option<&str>) -> bool {
    status.is_server_error()
        || matches!(status.as_u16(), 408 | 425 | 429)
        || (status.as_u16() == 409 && name != Some("invalid_idempotent_request"))
}

async fn bounded_error_name(response: &mut reqwest::Response) -> Option<String> {
    const MAX_ERROR_BODY: usize = 1024;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ERROR_BODY as u64)
    {
        return None;
    }
    let mut body = Vec::with_capacity(MAX_ERROR_BODY);
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len() + chunk.len() > MAX_ERROR_BODY {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice::<serde_json::Value>(&body)
        .ok()?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

async fn classify_error_response(mut response: reqwest::Response) -> EmailError {
    let status = response.status();
    let name = if status.as_u16() == 409 {
        bounded_error_name(&mut response).await
    } else {
        None
    };
    if retryable_response(status, name.as_deref()) {
        EmailError::Unavailable
    } else {
        EmailError::Permanent
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
            "html": rendered.html,
        })
    }

    fn request(&self, msg: &EmailMessage, idempotency_key: &str) -> reqwest::RequestBuilder {
        self.client
            .post("https://api.resend.com/emails")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .header("Idempotency-Key", idempotency_key)
            .json(&self.payload(msg))
    }
}

#[async_trait]
impl EmailProvider for ResendEmailProvider {
    async fn send(&self, msg: &EmailMessage) -> Result<(), EmailError> {
        self.send_idempotent(msg, &crate::email::idempotency_key(msg))
            .await
    }

    async fn send_idempotent(
        &self,
        msg: &EmailMessage,
        idempotency_key: &str,
    ) -> Result<(), EmailError> {
        let res = self
            .request(msg, idempotency_key)
            .send()
            .await
            .map_err(|_| EmailError::Unavailable)?;

        if res.status().is_success() {
            Ok(())
        } else {
            Err(classify_error_response(res).await)
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
                expires_at: None,
            },
        );
        let p = provider().payload(&msg);
        assert_eq!(p["from"], "no-reply@bikesnest.local");
        assert_eq!(p["to"][0], "a@example.com");
        assert_eq!(p["subject"], "Confirm your BikesNest email");
        assert!(p["text"].as_str().unwrap().contains("verify-email?token=t"));
        assert!(p["html"].as_str().unwrap().contains("verify-email?token=t"));
    }

    #[test]
    fn payload_is_rendered_in_the_recipients_locale() {
        let msg = EmailMessage::new(
            "a@example.com",
            LocaleCode::PtBr,
            EmailKind::VerifyEmail {
                link: "https://bikesnest.test/verify-email?token=t".into(),
                expires_at: None,
            },
        );
        let p = provider().payload(&msg);
        assert_eq!(p["subject"], "Confirme seu e-mail no BikesNest");
    }

    #[test]
    fn retryability_distinguishes_idempotency_conflicts() {
        use reqwest::StatusCode;
        assert!(retryable_response(StatusCode::TOO_MANY_REQUESTS, None));
        assert!(retryable_response(StatusCode::INTERNAL_SERVER_ERROR, None));
        assert!(retryable_response(
            StatusCode::CONFLICT,
            Some("concurrent_idempotent_requests")
        ));
        assert!(!retryable_response(
            StatusCode::CONFLICT,
            Some("invalid_idempotent_request")
        ));
        assert!(!retryable_response(
            StatusCode::BAD_REQUEST,
            Some("validation_error")
        ));
    }

    async fn conflict_response(body: Vec<u8>, content_length: Option<usize>) -> reqwest::Response {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let length = content_length
                .map(|value| format!("Content-Length: {value}\r\n"))
                .unwrap_or_default();
            write!(
                stream,
                "HTTP/1.0 409 Conflict\r\n{length}Connection: close\r\n\r\n"
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        let response = reqwest::Client::new()
            .get(format!("http://{address}"))
            .send()
            .await
            .unwrap();
        server.join().unwrap();
        response
    }

    #[tokio::test]
    async fn response_matrix_bounds_and_classifies_idempotency_conflicts() {
        let concurrent = br#"{"name":"concurrent_idempotent_requests"}"#.to_vec();
        let invalid = br#"{"name":"invalid_idempotent_request"}"#.to_vec();
        for declared in [Some(concurrent.len()), None] {
            assert!(matches!(
                classify_error_response(conflict_response(concurrent.clone(), declared).await)
                    .await,
                EmailError::Unavailable
            ));
        }
        for declared in [Some(invalid.len()), None] {
            assert!(matches!(
                classify_error_response(conflict_response(invalid.clone(), declared).await).await,
                EmailError::Permanent
            ));
        }

        // Unknown/unreadable conflicts are conservatively retryable. The body
        // parser neither returns nor persists provider text, and stops once the
        // 1 KiB diagnostic budget is exceeded.
        for (body, declared) in [
            (vec![b'x'; 1025], Some(1025)),
            (vec![b'x'; 1025], None),
            (b"not-json-hostile-secret".to_vec(), Some(23)),
            (vec![0xff, 0xfe], None),
        ] {
            assert!(matches!(
                classify_error_response(conflict_response(body, declared).await).await,
                EmailError::Unavailable
            ));
        }
    }

    #[test]
    fn requests_use_a_stable_per_message_idempotency_key_and_payload() {
        let provider = provider();
        let msg = EmailMessage::new(
            "a@example.com",
            LocaleCode::En,
            EmailKind::VerifyEmail {
                link: "https://bikesnest.test/verify-email?token=stable".into(),
                expires_at: None,
            },
        );
        let key = crate::email::idempotency_key(&msg);
        let first = provider.request(&msg, &key).build().unwrap();
        let second = provider.request(&msg, &key).build().unwrap();
        assert_eq!(first.headers()["idempotency-key"], key);
        assert_eq!(
            first.body().unwrap().as_bytes(),
            second.body().unwrap().as_bytes()
        );
        let other = EmailMessage::new(
            "b@example.com",
            LocaleCode::En,
            EmailKind::VerifyEmail {
                link: "https://bikesnest.test/verify-email?token=other".into(),
                expires_at: None,
            },
        );
        assert_ne!(key, crate::email::idempotency_key(&other));
    }
}
