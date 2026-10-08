//! Privacy and account lifecycle: the data hub, personal-data exports,
//! rights requests and account deletion.

use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use bikesnest_application::PrivacyError;
use bikesnest_domain::PrivacyRequestKind;
use serde_json::json;

use crate::auth::{
    Auth, clear_session_cookie, cookie_value, export_cookie_name, set_export_cookie,
};
use crate::client_ip::ClientIp;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view;
use crate::{AccountDeletePage, AccountExportPage, AccountPrivacyPage, PageLayout};

use super::auth::redirect_with_cookie;
use super::common::render;

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct PrivacyHubNotices {
    /// Why the last export request failed: `rate_limited`, or anything else
    /// for a generic failure.
    #[serde(default)]
    export_error: Option<String>,
}

/// The privacy and data hub.
pub(crate) async fn account_privacy(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Query(q): Query<PrivacyHubNotices>,
) -> Response {
    let tr = Translator::new(locale);
    if let Err(resp) = auth.require_user() {
        return resp;
    }
    let notice = None;
    let error = q.export_error.as_deref().map(|code| match code {
        "rate_limited" => tr.t("auth.error.rate_limited").to_string(),
        _ => tr.t("export.error").to_string(),
    });
    render(
        AccountPrivacyPage {
            layout: PageLayout::for_request(
                tr.t("privacy.hub_title").to_string(),
                "account",
                &auth,
                &state.map,
            ),
            tr,
            request_types: view::privacy_request_kind_options(tr),
            consent_records: false,
            notice,
            error,
        },
        StatusCode::OK,
    )
}

/// POST /account/privacy/export — request a personal-data export.
///
/// The single-use download token is handed back in a path-scoped `HttpOnly`
/// cookie, not in the redirect URL: a token in the query string is recorded by
/// the browser's history, by any `Referer` the page emits, and by every proxy
/// and access log between here and the user.
pub(crate) async fn account_export_post(
    State(state): State<AppState>,
    _locale: Locale,
    ClientIp(ip): ClientIp,
    auth: Auth,
) -> Response {
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    match state.privacy.request_export(&ip, user).await {
        Ok(req) => redirect_with_cookie(
            &format!("/account/export/{}", req.id),
            &set_export_cookie(req.id, &req.token),
        ),
        Err(PrivacyError::RateLimited) => {
            Redirect::to("/account/privacy?export_error=rate_limited").into_response()
        }
        Err(_) => Redirect::to("/account/privacy?export_error=1").into_response(),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct RightsRequestForm {
    kind: String,
    #[serde(default)]
    details: String,
}

/// POST /account/privacy/request — submit a manual rights request.
pub(crate) async fn account_privacy_request_post(
    State(state): State<AppState>,
    _locale: Locale,
    auth: Auth,
    Form(form): Form<RightsRequestForm>,
) -> Response {
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let kind = match PrivacyRequestKind::from_code(&form.kind) {
        Ok(k) => k,
        Err(_) => return Redirect::to("/account/privacy?request_error=1").into_response(),
    };
    let details = if form.details.trim().is_empty() {
        json!({})
    } else {
        json!({ "note": form.details.trim() })
    };
    match state.privacy.submit_request(user, kind, details).await {
        Ok(_) => {
            tracing::info!("privacy request submitted"); // no PII in the log field
            Redirect::to("/account/privacy?requested=1").into_response()
        }
        Err(_) => Redirect::to("/account/privacy?request_error=1").into_response(),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ExportQuery {
    /// **Deprecated** — the token now travels in the `export_{id}` cookie.
    /// Still accepted for one release so a link a user bookmarked or a page
    /// left open across the deploy keeps working; remove it after that.
    token: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// The download token for export `id`: the path-scoped cookie first, falling
/// back to the deprecated `?token=` query parameter.
pub(crate) fn export_token(headers: &HeaderMap, id: i64, q: &ExportQuery) -> Option<String> {
    cookie_value(headers, &export_cookie_name(id)).or_else(|| q.token.clone())
}

/// Export status + single-use download link.
pub(crate) async fn account_export(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<ExportQuery>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    // Only whether a token is held decides whether the link is offered; the
    // token itself never reaches the page — the browser attaches the cookie.
    let held = export_token(&headers, id, &q).is_some();
    let exports = state.privacy.list_exports(user).await.unwrap_or_default();
    let items: Vec<view::ExportVm> = exports
        .iter()
        .map(|e| view::export_vm(tr, e, e.id == id && held))
        .collect();
    let notice = if q.error.is_some() {
        Some(tr.t("export.error").to_string())
    } else {
        None
    };
    render(
        AccountExportPage {
            layout: PageLayout::for_request(
                tr.t("export.title").to_string(),
                "account",
                &auth,
                &state.map,
            ),
            tr,
            items,
            notice,
        },
        StatusCode::OK,
    )
}

/// GET /account/export/{id}/download — owner-only, single-use, expiring. The
/// token comes from the `export_{id}` cookie (see [`set_export_cookie`]); the
/// deprecated `?token=` query parameter is still honoured for one release.
pub(crate) async fn account_export_download(
    State(state): State<AppState>,
    _locale: Locale,
    auth: Auth,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Query(q): Query<ExportQuery>,
) -> Response {
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let token = export_token(&headers, id, &q).unwrap_or_default();
    match state.privacy.download_export(user, id, &token).await {
        Ok(download) => {
            let body =
                serde_json::to_vec_pretty(&download.payload).unwrap_or_else(|_| b"{}".to_vec());
            let mut resp = body.into_response();
            resp.headers_mut().insert(
                header::CONTENT_TYPE,
                "application/json; charset=utf-8".parse().unwrap(),
            );
            resp.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"bikesnest-export.json\""
                    .parse()
                    .unwrap(),
            );
            resp.headers_mut().insert(
                header::HeaderName::from_static("x-robots-tag"),
                "noindex".parse().unwrap(),
            );
            resp
        }
        // Uniform response for a non-existent id and an id that belongs to
        // another user, so the endpoint does not leak whether an export id
        // exists (no id-probe oracle).
        Err(PrivacyError::NotFound | PrivacyError::NotAuthorized) => {
            (StatusCode::FORBIDDEN, "Forbidden").into_response()
        }
        // The legitimate "link no longer works" cases (expired / already used /
        // bad token) land back on the owner's export page with a notice.
        Err(
            PrivacyError::Expired | PrivacyError::AlreadyDownloaded | PrivacyError::InvalidToken,
        ) => Redirect::to(&format!("/account/export/{id}?error=1")).into_response(),
        Err(_) => Redirect::to(&format!("/account/export/{id}?error=1")).into_response(),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct DeleteForm {
    email: String,
    #[serde(default)]
    password: String,
}

/// GET /account/delete — deletion confirmation form.
pub(crate) async fn account_delete(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
) -> Response {
    let tr = Translator::new(locale);
    if let Err(resp) = auth.require_user() {
        return resp;
    }
    render(
        AccountDeletePage {
            layout: PageLayout::for_request(
                tr.t("delete.title").to_string(),
                "account",
                &auth,
                &state.map,
            ),
            tr,
            error: None,
        },
        StatusCode::OK,
    )
}

/// POST /account/delete — re-auth + confirm, then anonymize-in-place.
pub(crate) async fn account_delete_post(
    State(state): State<AppState>,
    locale: Locale,
    ClientIp(ip): ClientIp,
    auth: Auth,
    Form(form): Form<DeleteForm>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let password = if form.password.trim().is_empty() {
        None
    } else {
        Some(form.password.as_str())
    };
    let delete_err_status = |tr: Translator, key: &str, status: StatusCode| {
        render(
            AccountDeletePage {
                layout: PageLayout::for_request(
                    tr.t("delete.title").to_string(),
                    "account",
                    &auth,
                    &state.map,
                ),
                tr,
                error: Some(tr.t(key).to_string()),
            },
            status,
        )
    };
    let delete_err = |tr: Translator, key: &str| delete_err_status(tr, key, StatusCode::OK);
    match state
        .privacy
        .request_deletion(&ip, user, password, &form.email)
        .await
    {
        Ok(()) => {
            // Session was revoked in the service; clear the cookie too.
            let mut resp = Redirect::to("/login?deleted=1").into_response();
            resp.headers_mut()
                .insert(header::SET_COOKIE, clear_session_cookie().parse().unwrap());
            resp
        }
        Err(PrivacyError::LastAdmin) => delete_err(tr, "delete.last_admin_error"),
        Err(PrivacyError::ReauthRequired) => delete_err(tr, "delete.reauth_error"),
        Err(PrivacyError::RateLimited) => {
            delete_err_status(tr, "auth.error.rate_limited", StatusCode::TOO_MANY_REQUESTS)
        }
        Err(PrivacyError::Conflict) => {
            delete_err_status(tr, "error.conflict", StatusCode::CONFLICT)
        }
        Err(PrivacyError::Unavailable) => {
            delete_err_status(tr, "error.unavailable", StatusCode::SERVICE_UNAVAILABLE)
        }
        Err(_) => delete_err(tr, "delete.reauth_error"),
    }
}
