//! The published policy documents (privacy, terms, cookies) and their
//! version history.

use axum::extract::{Form, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use bikesnest_application::POLICY_FALLBACK_LOCALE;
use bikesnest_domain::PolicyKind;

use crate::auth::Auth;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view;
use crate::{PageLayout, PolicyPage, PolicyVersionsPage, TermsNoticePage};

use super::common::render;

/// Resolve the current document for the request locale, falling back to
/// pt-BR when that locale has no published document.
pub(crate) async fn current_policy(
    state: &AppState,
    kind: PolicyKind,
    locale: Locale,
) -> Option<bikesnest_application::PolicyDocument> {
    let code = locale.html_lang();
    match state.policy.current(kind, code).await {
        Ok(Some(doc)) => Some(doc),
        Ok(None) if code != POLICY_FALLBACK_LOCALE => state
            .policy
            .current(kind, POLICY_FALLBACK_LOCALE)
            .await
            .ok()
            .flatten(),
        _ => None,
    }
}

pub(crate) async fn pending_terms_notices(
    state: &AppState,
    user_id: bikesnest_domain::UserId,
    locale: Locale,
) -> Result<Vec<bikesnest_application::PendingTermsNotice>, bikesnest_application::PrivacyError> {
    let code = locale.html_lang();
    let found = state.terms.pending_notices(user_id, code).await?;
    if !found.is_empty() || code == POLICY_FALLBACK_LOCALE {
        return Ok(found);
    }
    state
        .terms
        .pending_notices(user_id, POLICY_FALLBACK_LOCALE)
        .await
}

pub(crate) async fn terms_version(
    Path(id): Path<i64>,
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
) -> Response {
    let doc = match state.policy.by_id(id).await {
        Ok(Some(doc)) if doc.kind == PolicyKind::Terms => doc,
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let tr = Translator::new(locale);
    render(
        PolicyPage {
            layout: PageLayout::for_request(
                format!("{} — BikesNest", tr.t("nav.terms")),
                "terms",
                &auth,
                &state.map,
            ),
            tr,
            kind_code: "terms",
            kind_label: tr.t("nav.terms"),
            version: doc.version,
            effective_label: view::iso_datetime_label(tr, doc.effective_at),
            content: crate::markdown::render_policy_markdown(&doc.content),
        },
        StatusCode::OK,
    )
}

async fn render_terms_notice(
    state: &AppState,
    locale: Locale,
    auth: &Auth,
    notice: bikesnest_application::PendingTermsNotice,
    error: Option<String>,
    status: StatusCode,
) -> Response {
    let tr = Translator::new(locale);
    render(
        TermsNoticePage {
            layout: PageLayout::for_request(
                tr.t("terms.notice.title").to_string(),
                "account",
                auth,
                &state.map,
            ),
            tr,
            policy_id: notice.document.id,
            version: notice.document.version,
            effective_label: view::iso_datetime_label(tr, notice.document.effective_at),
            effective_at: notice.document.effective_at.to_rfc3339(),
            content: crate::markdown::render_policy_markdown(&notice.document.content),
            future: !notice.may_acknowledge,
            error,
        },
        status,
    )
}

pub(crate) async fn account_terms_notice(
    Path(id): Path<i64>,
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
) -> Response {
    let user = match auth.require_user() {
        Ok(user) => user,
        Err(response) => return response,
    };
    if !state.config.policy.acknowledgement_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let notice = match pending_terms_notices(&state, user.id, locale).await {
        Ok(notices) if notices.iter().any(|notice| notice.document.id == id) => notices
            .into_iter()
            .find(|notice| notice.document.id == id)
            .unwrap(),
        Ok(notices) if !notices.is_empty() => return StatusCode::CONFLICT.into_response(),
        Ok(_) => return StatusCode::NOT_FOUND.into_response(),
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let proof = bikesnest_application::TermsProof {
        policy_version_id: notice.document.id,
        terms_version: notice.document.version.clone(),
        shown_locale: notice.document.locale.clone(),
    };
    // Build the exact response before recording a presentation. The timestamp
    // proves only that the server prepared this response for return; it does
    // not prove browser display or human reading.
    let response = render_terms_notice(&state, locale, &auth, notice, None, StatusCode::OK).await;
    if state.terms.present(user.id, &proof).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    response
}

#[derive(serde::Deserialize)]
pub(crate) struct TermsAckForm {
    policy_id: i64,
    terms_version: String,
}

pub(crate) async fn account_terms_acknowledge(
    Path(id): Path<i64>,
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
    Form(form): Form<TermsAckForm>,
) -> Response {
    let user = match auth.require_user() {
        Ok(user) => user,
        Err(response) => return response,
    };
    if !state.config.policy.acknowledgement_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    let doc = match state.policy.by_id(id).await {
        Ok(doc) => doc,
        Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };
    let proof = doc.as_ref().map(|doc| bikesnest_application::TermsProof {
        policy_version_id: form.policy_id,
        terms_version: form.terms_version,
        shown_locale: doc.locale.clone(),
    });
    let result = if id != form.policy_id {
        Err(bikesnest_application::PrivacyError::Conflict)
    } else if let Some(proof) = proof {
        state.terms.acknowledge_current(user.id, &proof).await
    } else {
        Err(bikesnest_application::PrivacyError::Conflict)
    };
    match result {
        Ok(()) => axum::response::Redirect::to("/account?terms_acknowledged=1").into_response(),
        Err(bikesnest_application::PrivacyError::Unavailable) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Err(_) => match pending_terms_notices(&state, user.id, locale).await {
            Ok(notices) if !notices.is_empty() => {
                let notice = notices
                    .iter()
                    .find(|notice| notice.may_acknowledge)
                    .cloned()
                    .unwrap_or_else(|| notices[0].clone());
                render_terms_notice(
                    &state,
                    locale,
                    &auth,
                    notice,
                    Some(Translator::new(locale).t("terms.notice.stale").to_string()),
                    StatusCode::CONFLICT,
                )
                .await
            }
            Ok(_) => StatusCode::CONFLICT.into_response(),
            Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        },
    }
}

/// Version history for the request locale, falling back to pt-BR.
pub(crate) async fn policy_history(
    state: &AppState,
    kind: PolicyKind,
    locale: Locale,
) -> Vec<bikesnest_application::PolicyDocument> {
    let code = locale.html_lang();
    let docs = state.policy.history(kind, code).await.unwrap_or_default();
    if docs.is_empty() && code != POLICY_FALLBACK_LOCALE {
        return state
            .policy
            .history(kind, POLICY_FALLBACK_LOCALE)
            .await
            .unwrap_or_default();
    }
    docs
}

pub(crate) fn policy_kind_meta(tr: Translator, kind: PolicyKind) -> (&'static str, &'static str) {
    match kind {
        PolicyKind::Privacy => ("privacy", tr.t("nav.privacy")),
        PolicyKind::Terms => ("terms", tr.t("nav.terms")),
        PolicyKind::Cookies => ("cookies", tr.t("nav.cookies")),
    }
}

/// Shared builder for a public versioned legal page. The stored
/// markdown goes through [`crate::markdown::render_policy_markdown`], which
/// escapes raw HTML — that output is the only `|safe` value in the template.
/// Takes `auth` so a signed-in visitor still sees their own header here (these
/// pages are reachable from the footer on every page, logged in or not).
pub(crate) async fn policy_page_impl(
    state: &AppState,
    locale: Locale,
    auth: &Auth,
    kind: PolicyKind,
) -> Response {
    let tr = Translator::new(locale);
    let (kind_code, kind_label) = policy_kind_meta(tr, kind);
    let layout = PageLayout::for_request(
        format!("{kind_label} — BikesNest"),
        kind_code,
        auth,
        &state.map,
    );
    match current_policy(state, kind, locale).await {
        Some(doc) => render(
            PolicyPage {
                layout,
                tr,
                kind_code,
                kind_label,
                version: doc.version.clone(),
                effective_label: view::iso_datetime_label(tr, doc.effective_at),
                content: crate::markdown::render_policy_markdown(&doc.content),
            },
            StatusCode::OK,
        ),
        None => render(
            PolicyPage {
                layout,
                tr,
                kind_code,
                kind_label,
                version: "—".to_string(),
                effective_label: String::new(),
                content: format!(
                    "<p>{}</p>",
                    crate::markdown::escape_text(tr.t("policy.missing"))
                ),
            },
            StatusCode::OK,
        ),
    }
}

pub(crate) async fn privacy_page(
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
) -> Response {
    policy_page_impl(&state, locale, &auth, PolicyKind::Privacy).await
}

pub(crate) async fn terms_page(
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
) -> Response {
    policy_page_impl(&state, locale, &auth, PolicyKind::Terms).await
}

pub(crate) async fn cookies_page(
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
) -> Response {
    policy_page_impl(&state, locale, &auth, PolicyKind::Cookies).await
}

/// Shared builder for a policy version-history page.
pub(crate) async fn policy_versions_impl(
    state: &AppState,
    locale: Locale,
    auth: &Auth,
    kind: PolicyKind,
) -> Response {
    let tr = Translator::new(locale);
    let (kind_code, kind_label) = policy_kind_meta(tr, kind);
    let docs = policy_history(state, kind, locale).await;
    let items: Vec<view::PolicyVersionVm> = docs
        .iter()
        .map(|d| view::policy_version_vm(tr, d))
        .collect();
    render(
        PolicyVersionsPage {
            layout: PageLayout::for_request(
                format!("{} — BikesNest", tr.t("policy.versions_title")),
                kind_code,
                auth,
                &state.map,
            ),
            tr,
            kind_code,
            kind_label,
            items,
        },
        StatusCode::OK,
    )
}

pub(crate) async fn privacy_versions(
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
) -> Response {
    policy_versions_impl(&state, locale, &auth, PolicyKind::Privacy).await
}

pub(crate) async fn terms_versions(
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
) -> Response {
    policy_versions_impl(&state, locale, &auth, PolicyKind::Terms).await
}

pub(crate) async fn cookies_versions(
    locale: Locale,
    auth: Auth,
    State(state): State<AppState>,
) -> Response {
    policy_versions_impl(&state, locale, &auth, PolicyKind::Cookies).await
}
