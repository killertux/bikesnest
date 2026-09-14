//! Offline browser checks of actual renderer output, not mail-client certification.

use std::io::Write;
use std::process::{Command, Stdio};

use bikesnest_application::{EmailKind, EmailMessage};
use bikesnest_domain::LocaleCode;
use bikesnest_infrastructure::render_email;
use chrono::{TimeZone, Utc};

#[test]
#[ignore = "requires local Playwright Chromium; no database or external providers"]
fn renderer_html_is_visually_checked_locally() {
    let expiry = Utc.with_ymd_and_hms(2030, 1, 2, 3, 4, 5).unwrap();
    let link = format!(
        "https://example.test/password-reset/new?token={}&next=\"<account>\"",
        "credential-marker-".repeat(8)
    );
    let mut fixtures = Vec::new();
    for locale in [LocaleCode::En, LocaleCode::PtBr] {
        let mut kinds = Vec::new();
        for expires_at in [Some(expiry), None] {
            kinds.extend([
                EmailKind::VerifyEmail {
                    link: link.clone(),
                    expires_at,
                },
                EmailKind::ResetPassword {
                    link: link.clone(),
                    expires_at,
                },
                EmailKind::ConfirmEmailChange {
                    link: link.clone(),
                    expires_at,
                },
            ]);
        }
        kinds.extend([
            EmailKind::PasswordChanged {
                account_link: "https://example.test/login".into(),
                notification_id: "never-render".into(),
            },
            EmailKind::EmailAddressChanged {
                account_link: "https://example.test/login".into(),
                notification_id: "never-render".into(),
            },
        ]);
        for kind in kinds {
            let rendered = render_email(&EmailMessage::new("a@example.test", locale, kind.clone()));
            fixtures.push(serde_json::json!({
                "name": format!("{}-{}-{}", locale.as_str(), kind.code(), if kind.expires_at().is_some() { "expiry" } else { "no-expiry" }),
                "lang": locale.as_str(),
                "link": kind.action_link(),
                "expiry": kind.expires_at().map(|at| at.format("%Y-%m-%d %H:%M:%S UTC").to_string()),
                "html": rendered.html,
                "text": rendered.text,
            }));
        }
    }
    let root = format!("{}/../..", env!("CARGO_MANIFEST_DIR"));
    let mut child = Command::new("node")
        .arg("tests/browser/email-renderer-visual.cjs")
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&fixtures).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
