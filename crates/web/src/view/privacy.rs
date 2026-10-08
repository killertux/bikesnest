//! Privacy and account-lifecycle view models: exports, rights requests,
//! policy versions.

use super::format::*;
use crate::i18n::Translator;

/// One row of the export-status page (status + optional single-use link).
#[derive(Debug, Clone)]
pub struct ExportVm {
    pub id: i64,
    pub state_code: &'static str,
    pub state_label: &'static str,
    pub created_label: String,
    pub expires_label: String,
    /// Whether the request holds this export's download token, and so whether
    /// to render the link. The token itself is never put in the page: it lives
    /// in the path-scoped `export_{id}` cookie and the browser attaches it.
    pub downloadable: bool,
    pub is_ready: bool,
}

pub fn export_state_label(t: Translator, s: bikesnest_domain::ExportState) -> &'static str {
    match s {
        bikesnest_domain::ExportState::Ready => t.t("export.state.ready"),
        bikesnest_domain::ExportState::Downloaded => t.t("export.state.downloaded"),
        bikesnest_domain::ExportState::Expired => t.t("export.state.expired"),
    }
}

/// Build an export row. `token_held` says whether this request carries the export's
/// single-use download token (the `export_{id}` cookie) — true only for the
/// export the owner just requested, and only while it is still `READY`.
pub fn export_vm(t: Translator, e: &bikesnest_application::Export, token_held: bool) -> ExportVm {
    let is_ready = e.state == bikesnest_domain::ExportState::Ready;
    ExportVm {
        id: e.id,
        state_code: e.state.as_code(),
        state_label: export_state_label(t, e.state),
        created_label: iso_datetime_label(t, e.created_at),
        expires_label: iso_datetime_label(t, e.expires_at),
        downloadable: is_ready && token_held,
        is_ready,
    }
}

/// One row of the admin privacy-request queue (or the privacy rights list).
///
/// A rights request is a legal clock, so the row carries the three facts an
/// operator needs to act: who asked, what they wrote, and how long is left
/// (LGPD art. 19 — [`bikesnest_domain::PRIVACY_REQUEST_SLA_DAYS`]).
#[derive(Debug, Clone)]
pub struct PrivacyRequestVm {
    pub id: i64,
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub state_code: &'static str,
    pub state_label: &'static str,
    pub created_label: String,
    /// Display name or email of the subject; the anonymized-account fallback
    /// when the user row is gone.
    pub subject_label: String,
    /// Their row in the admin user list, when the account still exists.
    pub subject_url: Option<String>,
    /// The free text the user typed with the request.
    pub details: Option<String>,
    /// "3 days left" / "2 days overdue"; empty once the request is closed.
    pub deadline_label: String,
    pub is_overdue: bool,
}

pub fn privacy_request_kind_label(
    t: Translator,
    kind: bikesnest_domain::PrivacyRequestKind,
) -> &'static str {
    match kind {
        bikesnest_domain::PrivacyRequestKind::Access => t.t("privacy.kind.access"),
        bikesnest_domain::PrivacyRequestKind::Rectification => t.t("privacy.kind.rectification"),
        bikesnest_domain::PrivacyRequestKind::Deletion => t.t("privacy.kind.deletion"),
        bikesnest_domain::PrivacyRequestKind::Export => t.t("privacy.kind.export"),
        bikesnest_domain::PrivacyRequestKind::Restriction => t.t("privacy.kind.restriction"),
        bikesnest_domain::PrivacyRequestKind::Objection => t.t("privacy.kind.objection"),
        bikesnest_domain::PrivacyRequestKind::ConsentWithdrawal => t.t("privacy.kind.consent"),
    }
}

pub fn privacy_request_state_label(
    t: Translator,
    s: bikesnest_domain::PrivacyRequestState,
) -> &'static str {
    match s {
        bikesnest_domain::PrivacyRequestState::Open => t.t("privacy.state.open"),
        bikesnest_domain::PrivacyRequestState::InProgress => t.t("privacy.state.in_progress"),
        bikesnest_domain::PrivacyRequestState::Completed => t.t("privacy.state.completed"),
        bikesnest_domain::PrivacyRequestState::Declined => t.t("privacy.state.declined"),
    }
}

/// `labels` is the batched subject lookup for every request on the page.
pub fn privacy_request_vm(
    t: Translator,
    r: &bikesnest_application::PrivacyRequest,
    labels: &std::collections::HashMap<i64, String>,
) -> PrivacyRequestVm {
    let subject = r.user_id.map(|u| u.0);
    let is_open = matches!(
        r.state,
        bikesnest_domain::PrivacyRequestState::Open
            | bikesnest_domain::PrivacyRequestState::InProgress
    );
    let days_left = bikesnest_domain::privacy_request_days_left(r.created_at, chrono::Utc::now());
    let deadline_label = if !is_open {
        String::new()
    } else if days_left < 0 {
        t.t("admin.privacy_requests.overdue")
            .replace("{n}", &(-days_left).to_string())
    } else {
        t.t("admin.privacy_requests.days_left")
            .replace("{n}", &days_left.to_string())
    };
    PrivacyRequestVm {
        id: r.id,
        kind_code: r.kind.as_code(),
        kind_label: privacy_request_kind_label(t, r.kind),
        state_code: r.state.as_code(),
        state_label: privacy_request_state_label(t, r.state),
        created_label: iso_datetime_label(t, r.created_at),
        subject_label: subject
            .and_then(|id| labels.get(&id).cloned())
            .or_else(|| subject.map(|id| format!("#{id}")))
            .unwrap_or_else(|| t.t("privacy.subject.anonymized").to_string()),
        subject_url: subject
            .filter(|id| labels.contains_key(id))
            .map(|id| format!("/admin/users?q={id}")),
        details: r
            .details
            .get("note")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        deadline_label,
        is_overdue: is_open && days_left < 0,
    }
}

/// One row of the policy version-history list.
#[derive(Debug, Clone)]
pub struct PolicyVersionVm {
    pub version: String,
    pub effective_label: String,
    pub is_current: bool,
}

pub fn policy_version_vm(
    t: Translator,
    doc: &bikesnest_application::PolicyDocument,
) -> PolicyVersionVm {
    PolicyVersionVm {
        version: doc.version.clone(),
        effective_label: iso_datetime_label(t, doc.effective_at),
        is_current: doc.superseded_at.is_none(),
    }
}

/// One selectable manual rights kind on the privacy hub.
#[derive(Debug, Clone)]
pub struct PrivacyRequestKindVm {
    pub code: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

/// The manual (operator-fulfilled) rights kinds, with descriptions, for the privacy
/// request list. Access/export and deletion are automatic and have their
/// own cards.
pub fn privacy_request_kind_options(t: Translator) -> Vec<PrivacyRequestKindVm> {
    vec![
        PrivacyRequestKindVm {
            code: "rectification",
            label: privacy_request_kind_label(
                t,
                bikesnest_domain::PrivacyRequestKind::Rectification,
            ),
            description: t.t("privacy.rights.rectification_desc"),
        },
        PrivacyRequestKindVm {
            code: "restriction",
            label: privacy_request_kind_label(t, bikesnest_domain::PrivacyRequestKind::Restriction),
            description: t.t("privacy.rights.restriction_desc"),
        },
        PrivacyRequestKindVm {
            code: "objection",
            label: privacy_request_kind_label(t, bikesnest_domain::PrivacyRequestKind::Objection),
            description: t.t("privacy.rights.objection_desc"),
        },
        PrivacyRequestKindVm {
            code: "consent_withdrawal",
            label: privacy_request_kind_label(
                t,
                bikesnest_domain::PrivacyRequestKind::ConsentWithdrawal,
            ),
            description: t.t("privacy.rights.consent_desc"),
        },
    ]
}
