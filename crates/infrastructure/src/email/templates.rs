//! Escaped, image-free transactional email rendering.

use bikesnest_application::{EmailKind, EmailMessage};
use bikesnest_i18n::{Locale, Translator};

pub const APP_NAME: &str = "BikesNest";

#[derive(Clone, PartialEq, Eq)]
pub struct RenderedEmail {
    pub subject: String,
    pub text: String,
    pub html: String,
}

impl std::fmt::Debug for RenderedEmail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RenderedEmail")
            .field("subject", &"[REDACTED]")
            .field("text", &"[REDACTED]")
            .field("html", &"[REDACTED]")
            .finish()
    }
}

pub fn render(msg: &EmailMessage) -> RenderedEmail {
    let locale = Locale::from_code(msg.locale.as_str()).unwrap_or(Locale::PtBr);
    let tr = Translator::new(locale);
    let (subject_key, body_key, cta_key) = keys(&msg.kind);
    let subject = fill(tr.t(subject_key));
    let body = fill(tr.t(body_key));
    let link = msg.kind.action_link();
    let expiry = msg
        .kind
        .expires_at()
        .map(|at| at.format("%Y-%m-%d %H:%M:%S").to_string());
    let expiry_text = expiry
        .as_deref()
        .map(|value| fill_value(tr.t("email.expires"), value));
    let mut text = format!("{body}\n\n{}:\n{link}", tr.t(cta_key));
    if let Some(value) = &expiry_text {
        text.push_str(&format!("\n\n{value}"));
    }
    text.push_str(&format!("\n\n{}\n{link}", tr.t("email.fallback")));
    let escaped_link = escape_html(link);
    let expiry_html = expiry_text
        .as_deref()
        .map(|value| format!("<p style=\"color:#475569\">{}</p>", escape_html(value)))
        .unwrap_or_default();
    let html = format!(
        "<!doctype html><html lang=\"{}\"><body style=\"margin:0;background:#f1f5f9;color:#172033;font-family:Arial,sans-serif\"><main style=\"max-width:600px;margin:24px auto;padding:24px;background:#ffffff;border-radius:12px\"><h1 style=\"font-size:24px\">{}</h1><h2 style=\"font-size:20px\">{}</h2><p>{}</p><p><a href=\"{}\" style=\"display:inline-block;box-sizing:border-box;min-height:44px;line-height:20px;padding:12px 18px;background:#0f766e;color:#ffffff;border-radius:8px;text-decoration:none\">{}</a></p>{}<hr><p style=\"word-break:break-all\">{}</p><p style=\"word-break:break-all\"><a href=\"{}\">{}</a></p></main></body></html>",
        locale.html_lang(),
        escape_html(APP_NAME),
        escape_html(tr.t(cta_key)),
        escape_html(&body),
        escaped_link,
        escape_html(tr.t(cta_key)),
        expiry_html,
        escape_html(tr.t("email.fallback")),
        escaped_link,
        escaped_link,
    );
    RenderedEmail {
        subject,
        text,
        html,
    }
}

fn keys(kind: &EmailKind) -> (&'static str, &'static str, &'static str) {
    match kind {
        EmailKind::VerifyEmail { .. } => (
            "email.verify.subject",
            "email.verify.body",
            "email.cta.verify",
        ),
        EmailKind::ResetPassword { .. } => {
            ("email.reset.subject", "email.reset.body", "email.cta.reset")
        }
        EmailKind::ConfirmEmailChange { .. } => (
            "email.change.subject",
            "email.change.body",
            "email.cta.change",
        ),
        EmailKind::PasswordChanged { .. } => (
            "email.password_changed.subject",
            "email.password_changed.body",
            "email.cta.account",
        ),
        EmailKind::EmailAddressChanged { .. } => (
            "email.email_changed.subject",
            "email.email_changed.body",
            "email.cta.account",
        ),
    }
}

fn fill(template: &str) -> String {
    template.replace("{app}", APP_NAME)
}
fn fill_value(template: &str, value: &str) -> String {
    fill(template).replace("{expires}", value)
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use bikesnest_domain::LocaleCode;
    use chrono::{TimeZone, Utc};

    fn message(locale: LocaleCode, kind: EmailKind) -> EmailMessage {
        EmailMessage::new("a@example.com", locale, kind)
    }

    #[test]
    fn every_kind_and_locale_has_escaped_complete_alternatives() {
        for locale in [LocaleCode::En, LocaleCode::PtBr] {
            for (kind, hostile_link) in [
                (
                    EmailKind::VerifyEmail {
                        link: "https://example.test/a?x=<x>&quote=\"marker\"".into(),
                        expires_at: None,
                    },
                    true,
                ),
                (
                    EmailKind::ResetPassword {
                        link: "https://example.test/a".into(),
                        expires_at: None,
                    },
                    false,
                ),
                (
                    EmailKind::ConfirmEmailChange {
                        link: "https://example.test/a".into(),
                        expires_at: None,
                    },
                    false,
                ),
                (
                    EmailKind::PasswordChanged {
                        account_link: "https://example.test/account".into(),
                        notification_id: "secret-notice".into(),
                    },
                    false,
                ),
                (
                    EmailKind::EmailAddressChanged {
                        account_link: "https://example.test/account".into(),
                        notification_id: "secret-notice".into(),
                    },
                    false,
                ),
            ] {
                let out = render(&message(locale, kind));
                assert!(!out.subject.is_empty() && !out.text.is_empty() && !out.html.is_empty());
                if hostile_link {
                    assert!(!out.html.contains("<x>") && out.html.contains("&lt;x&gt;"));
                    assert!(out.html.contains("&amp;") && out.html.contains("&quot;"));
                }
                assert!(!out.html.contains("secret-notice") && !out.text.contains("secret-notice"));
                assert!(!out.html.contains("⟨i18n?⟩") && !out.text.contains('{'));
                assert!(!format!("{out:?}").contains("secret-notice"));
            }
        }
    }

    #[test]
    fn expiry_is_exact_and_legacy_credential_mail_claims_no_duration() {
        let at = Utc.with_ymd_and_hms(2030, 1, 2, 3, 4, 5).unwrap();
        let with_expiry = render(&message(
            LocaleCode::En,
            EmailKind::VerifyEmail {
                link: "https://e.test/x".into(),
                expires_at: Some(at),
            },
        ));
        assert!(with_expiry.text.contains("2030-01-02 03:04:05 UTC"));
        let legacy = render(&message(
            LocaleCode::En,
            EmailKind::VerifyEmail {
                link: "https://e.test/x".into(),
                expires_at: None,
            },
        ));
        assert!(!legacy.text.contains("expires at") && !legacy.text.contains("24 hours"));
        let marker = "CREDENTIAL-DEBUG-MARKER";
        let debug = render(&message(
            LocaleCode::En,
            EmailKind::ResetPassword {
                link: format!("https://e.test/{marker}"),
                expires_at: None,
            },
        ));
        assert!(!format!("{debug:?}").contains(marker));
    }
}
