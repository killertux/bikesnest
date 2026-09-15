//! Policy seeding for the versioned legal pages.
//!
//! `seed-policies` reads `policies/{kind}.{locale}.md` and installs all three
//! kinds in both locales in one transaction. An exact six-document replay is
//! idempotent; partial, changed, or ambiguously scheduled releases fail closed.
//!
//! The markdown carries `{{TOKEN}}` placeholders for the operator's identity
//! and contact channel (see [`POLICY_PLACEHOLDERS`]) so the company details
//! live in the deployment environment, not in the repository. Seeding refuses
//! to publish a document with an unresolved placeholder.

use crate::Db;
use bikesnest_application::PrivacyError;
use bikesnest_domain::PolicyKind;
use chrono::{DateTime, Utc};

#[derive(Debug, Clone)]
pub struct SeedPolicyDocument<'a> {
    pub kind: PolicyKind,
    pub locale: &'a str,
    pub version: &'a str,
    pub effective_at: DateTime<Utc>,
    pub content: &'a str,
    pub requires_acknowledgement: bool,
}

/// Locales a policy document is published in (`policy_version.locale`).
/// pt-BR is the fallback the web layer uses when a locale has no document.
pub const POLICY_LOCALES: &[&str] = &["pt-BR", "en"];

/// `{{TOKEN}}` → environment variable that supplies it at seed time (:
/// controller identity + contact information).
pub const POLICY_PLACEHOLDERS: &[(&str, &str)] = &[
    ("OPERATOR_NAME", "POLICY_OPERATOR_NAME"),
    ("OPERATOR_CNPJ", "POLICY_OPERATOR_CNPJ"),
    ("OPERATOR_ADDRESS", "POLICY_OPERATOR_ADDRESS"),
    ("CONTACT_EMAIL", "POLICY_CONTACT_EMAIL"),
];

/// Replace every `{{TOKEN}}` in `content` using `lookup`. Tokens are
/// `[A-Z0-9_]+`; anything else between double braces is left untouched.
/// Returns the distinct unresolved token names when any lookup fails, so the
/// caller can refuse to seed legal text with holes in it.
pub fn fill_policy_placeholders(
    content: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String, Vec<String>> {
    let mut out = String::with_capacity(content.len());
    let mut missing: Vec<String> = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let inner = &after[..end];
        let token = inner.trim();
        if is_token(token) {
            match lookup(token) {
                Some(value) => out.push_str(&value),
                None => {
                    if !missing.iter().any(|m| m == token) {
                        missing.push(token.to_string());
                    }
                }
            }
        } else {
            out.push_str("{{");
            out.push_str(inner);
            out.push_str("}}");
        }
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    if missing.is_empty() {
        Ok(out)
    } else {
        Err(missing)
    }
}

fn is_token(t: &str) -> bool {
    !t.is_empty()
        && t.chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Atomically install one coherent policy release. Production callers pass all
/// three kinds in both locales. Replays must match every immutable byte.
pub async fn seed_policy_release(
    db: &Db,
    documents: &[SeedPolicyDocument<'_>],
) -> Result<(), PrivacyError> {
    if documents.len() != 6 {
        return Err(PrivacyError::InvalidField(
            "a policy release must contain all six kind/locale documents".into(),
        ));
    }
    let version = documents[0].version;
    let effective_at = documents[0].effective_at;
    if documents.iter().any(|d| {
        d.version != version
            || d.effective_at != effective_at
            || (d.requires_acknowledgement && d.kind != PolicyKind::Terms)
    }) {
        return Err(PrivacyError::InvalidField(
            "incoherent policy release".into(),
        ));
    }
    for kind in [PolicyKind::Privacy, PolicyKind::Terms, PolicyKind::Cookies] {
        for locale in POLICY_LOCALES {
            if documents
                .iter()
                .filter(|d| d.kind == kind && d.locale == *locale)
                .count()
                != 1
            {
                return Err(PrivacyError::InvalidField(
                    "duplicate or missing policy kind/locale".into(),
                ));
            }
        }
    }
    let material: Vec<bool> = documents
        .iter()
        .filter(|d| d.kind == PolicyKind::Terms)
        .map(|d| d.requires_acknowledgement)
        .collect();
    if material.len() != 2 || material[0] != material[1] {
        return Err(PrivacyError::InvalidField(
            "terms material flag must match across locales".into(),
        ));
    }
    let mut conn = db
        .acquire()
        .await
        .map_err(|e| db_err("policy_seed.release", e))?;
    let mut tx = conn
        .begin()
        .await
        .map_err(|e| db_err("policy_seed.release", e))?;
    sqlx::query("SELECT pg_advisory_xact_lock(726_159_001)")
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("policy_seed.release", e))?;

    let mut replayed = 0usize;
    for doc in documents {
        let existing: Option<(DateTime<Utc>, String, bool)> = sqlx::query_as(
            "SELECT effective_at,content,requires_acknowledgement FROM policy_version WHERE kind=$1 AND locale=$2 AND version=$3",
        ).bind(doc.kind.as_code()).bind(doc.locale).bind(doc.version)
            .fetch_optional(&mut *tx).await.map_err(|e| db_err("policy_seed.release", e))?;
        if let Some((at, content, required)) = existing {
            if at != doc.effective_at
                || content != doc.content
                || required != doc.requires_acknowledgement
            {
                return Err(PrivacyError::Conflict);
            }
            replayed += 1;
        }
    }
    if replayed == documents.len() {
        tx.commit()
            .await
            .map_err(|e| db_err("policy_seed.release", e))?;
        return Ok(());
    }
    if replayed != 0 {
        return Err(PrivacyError::Conflict);
    }

    for doc in documents {
        let ambiguous: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM policy_version WHERE kind=$1 AND locale=$2 AND superseded_at IS NULL",
        ).bind(doc.kind.as_code()).bind(doc.locale).fetch_one(&mut *tx).await
            .map_err(|e| db_err("policy_seed.release", e))?;
        let latest: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT max(effective_at) FROM policy_version WHERE kind=$1 AND locale=$2",
        )
        .bind(doc.kind.as_code())
        .bind(doc.locale)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_err("policy_seed.release", e))?;
        if ambiguous > 1 || latest.is_some_and(|at| at >= doc.effective_at) {
            return Err(PrivacyError::Conflict);
        }
        sqlx::query("UPDATE policy_version SET superseded_at=$3 WHERE kind=$1 AND locale=$2 AND superseded_at IS NULL")
            .bind(doc.kind.as_code()).bind(doc.locale).bind(doc.effective_at).execute(&mut *tx).await
            .map_err(|e| db_err("policy_seed.release", e))?;
        sqlx::query("INSERT INTO policy_version(kind,locale,version,effective_at,content,requires_acknowledgement) VALUES($1,$2,$3,$4,$5,$6)")
            .bind(doc.kind.as_code()).bind(doc.locale).bind(doc.version).bind(doc.effective_at)
            .bind(doc.content).bind(doc.requires_acknowledgement).execute(&mut *tx).await
            .map_err(|e| db_err("policy_seed.release", e))?;
    }
    tx.commit()
        .await
        .map_err(|e| db_err("policy_seed.release", e))
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"policy_seed.insert"`.
fn db_err(context: &'static str, e: sqlx::Error) -> PrivacyError {
    crate::db_error::classify_and_log(context, e).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup(token: &str) -> Option<String> {
        match token {
            "OPERATOR_NAME" => Some("Acme Ltda.".to_string()),
            "CONTACT_EMAIL" => Some("privacidade@example.com".to_string()),
            _ => None,
        }
    }

    #[test]
    fn fills_known_tokens_and_leaves_other_braces_alone() {
        let out = fill_policy_placeholders(
            "Operated by {{OPERATOR_NAME}} ({{ CONTACT_EMAIL }}). Not a token: {{not one}} {{",
            lookup,
        )
        .unwrap();
        assert_eq!(
            out,
            "Operated by Acme Ltda. (privacidade@example.com). Not a token: {{not one}} {{"
        );
    }

    #[test]
    fn reports_each_missing_token_once() {
        let err = fill_policy_placeholders(
            "{{OPERATOR_CNPJ}} {{OPERATOR_NAME}} {{OPERATOR_CNPJ}} {{OPERATOR_ADDRESS}}",
            lookup,
        )
        .unwrap_err();
        assert_eq!(
            err,
            vec!["OPERATOR_CNPJ".to_string(), "OPERATOR_ADDRESS".to_string()]
        );
    }

    #[test]
    fn every_placeholder_has_an_env_var() {
        for (token, var) in POLICY_PLACEHOLDERS {
            assert!(is_token(token), "{token} must be an uppercase token");
            assert!(var.starts_with("POLICY_"), "{var} must be namespaced");
        }
    }
}
