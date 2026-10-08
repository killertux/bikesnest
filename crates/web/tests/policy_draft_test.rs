//! Draft rendering smoke only: this neither seeds policy versions nor certifies
//! the legal meaning or publication readiness of their text.

use bikesnest_infrastructure::privacy::{POLICY_LOCALES, fill_policy_placeholders};
use bikesnest_web::markdown::render_policy_markdown;

#[test]
fn all_bilingual_policy_drafts_fill_known_placeholders_and_render() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../policies");
    for kind in ["privacy", "terms", "cookies"] {
        for locale in POLICY_LOCALES {
            let file = root.join(format!("{kind}.{locale}.md"));
            let source = std::fs::read_to_string(&file).unwrap();
            let filled = fill_policy_placeholders(&source, |token| match token {
                "OPERATOR_NAME" => Some("Draft Test Operator".into()),
                "OPERATOR_CNPJ" => Some("TEST-ONLY".into()),
                "OPERATOR_ADDRESS" => Some("Test address, not for publication".into()),
                "CONTACT_EMAIL" => Some("draft@example.invalid".into()),
                _ => None,
            })
            .unwrap_or_else(|missing| {
                panic!("{}: unknown placeholders {missing:?}", file.display())
            });
            assert!(!filled.contains("{{"), "{}", file.display());
            let html = render_policy_markdown(&filled);
            assert!(html.contains("<h2>"), "{}", file.display());
            assert!(html.contains("draft@example.invalid"), "{}", file.display());
            assert!(!html.contains("href=\"#\""), "{}", file.display());
            assert!(!html.contains("<script"), "{}", file.display());
            if kind != "terms" {
                assert!(html.contains("<table>"), "{}", file.display());
            }
            if kind == "cookies" {
                assert!(html.contains("<code>bn.search.mapOpen</code>"));
                assert!(html.contains("Cloudflare Web Analytics"));
            }
        }
    }
}
