//! Admin-only pages: the user directory and its role/state actions, the
//! audit-log viewer and the manual privacy-request queue.

use axum::extract::{Form, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use bikesnest_application::{AuditFilter, AuthError};
use bikesnest_domain::{Role, UserId};

use crate::auth::Auth;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view;
use crate::{
    AdminAuditPage, AdminPrivacyRequestsPage, AdminUserContributionsPage, AdminUsersPage,
    PageLayout,
};

use super::common::{
    DEFAULT_PAGE_LIMIT, parse_after_id, parse_datetime, render, urlencoding_query,
};

/// POST /admin/users/{id}/suspend — ADMIN-only; revokes sessions + audits.
pub(crate) async fn admin_user_suspend(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Response {
    let actor = match auth.require_role(Role::Admin) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    match state.auth.suspend_user(actor, UserId(id)).await {
        Ok(()) => axum::response::Redirect::to("/admin/users?suspended=1").into_response(),
        Err(err) => axum::response::Redirect::to(&format!(
            "/admin/users?error={}",
            account_action_error_code(&err)
        ))
        .into_response(),
    }
}

/// POST /admin/users/{id}/restore — ADMIN-only; restores to Active + audits.
pub(crate) async fn admin_user_restore(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
) -> Response {
    let actor = match auth.require_role(Role::Admin) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    match state.auth.restore_user(actor, UserId(id)).await {
        Ok(()) => axum::response::Redirect::to("/admin/users?restored=1").into_response(),
        Err(err) => axum::response::Redirect::to(&format!(
            "/admin/users?error={}",
            account_action_error_code(&err)
        ))
        .into_response(),
    }
}

/// The `?error=` code a refused suspend/restore redirects with, so the user
/// list can say *why* instead of a generic failure.
fn account_action_error_code(err: &AuthError) -> &'static str {
    match err {
        AuthError::SelfSuspension => "self_suspension",
        AuthError::LastActiveAdmin => "last_active_admin",
        AuthError::StateUnchanged => "state_unchanged",
        _ => "1",
    }
}

/// The message for an `?error=` code on the admin user list.
fn admin_users_error(tr: Translator, code: &str) -> String {
    let key = match code {
        "self_suspension" => "auth.error.self_suspension",
        "last_active_admin" => "auth.error.last_active_admin",
        "state_unchanged" => "auth.error.state_unchanged",
        _ => "admin.role_error",
    };
    tr.t(key).to_string()
}

/// GET /admin/users/{id}/contributions — a target user's contribution feed
/// (MODERATOR/ADMIN).
pub(crate) async fn admin_user_contributions(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Path(id): Path<i64>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_moderator() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let target = UserId(id);
    // One row, not the whole users table: this page used to load every account
    // in the database to find a single label.
    let email = state
        .auth
        .user_labels(&[id])
        .await
        .ok()
        .and_then(|labels| labels.get(&id).cloned())
        .unwrap_or_else(|| format!("#{id}"));
    // Bounded to the newest DEFAULT_PAGE_LIMIT entries; this admin inspection
    // view has no "load more" control.
    let items = state
        .moderation
        .user_contribution_history(user, target, None, DEFAULT_PAGE_LIMIT)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|i| view::contribution_vm(tr, &i))
        .collect();
    render(
        AdminUserContributionsPage {
            layout: PageLayout::for_request(
                tr.t("admin.contrib.title").to_string(),
                "admin",
                &auth,
                &state.map,
            ),
            tr,
            user_id: id,
            email,
            items,
        },
        StatusCode::OK,
    )
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct AuditFilterQuery {
    #[serde(default)]
    action: String,
    #[serde(default)]
    target_type: String,
    #[serde(default)]
    actor: i64,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    cursor: i64,
}

/// GET /admin/audit — the ADMIN-only audit-log viewer.
pub(crate) async fn admin_audit(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Query(q): Query<AuditFilterQuery>,
) -> Response {
    let tr = Translator::new(locale);
    let admin = match auth.require_role(Role::Admin) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let filter = AuditFilter {
        actor_id: (q.actor > 0).then_some(UserId(q.actor)),
        action: (!q.action.is_empty()).then(|| q.action.clone()),
        target_type: (!q.target_type.is_empty()).then(|| q.target_type.clone()),
        from: parse_filter_datetime(&q.from),
        to: parse_filter_datetime(&q.to),
        cursor: (q.cursor > 0).then_some(q.cursor),
        limit: 50,
    };
    let page = state
        .moderation
        .list_audit_events(admin, filter)
        .await
        .map(|p| (p.items, p.next_cursor))
        .unwrap_or_default();
    // Resolve every actor on the page in ONE query, so the trail names people
    // instead of ids without turning 50 rows into 50 lookups.
    let mut actor_ids: Vec<i64> = page
        .0
        .iter()
        .filter_map(|e| e.event.actor_user_id.map(|a| a.0))
        .collect();
    actor_ids.sort_unstable();
    actor_ids.dedup();
    let labels = state.auth.user_labels(&actor_ids).await.unwrap_or_default();
    let items = page
        .0
        .into_iter()
        .map(|e| view::audit_row_vm(tr, &e, &labels))
        .collect();
    render(
        AdminAuditPage {
            layout: PageLayout::for_request(
                tr.t("admin.audit.title").to_string(),
                "admin",
                &auth,
                &state.map,
            ),
            tr,
            items,
            next_cursor: page.1,
            action: q.action.clone(),
            target_type: q.target_type.clone(),
            actor: q.actor.to_string(),
            // Echo back what `<input type="datetime-local">` can render: the
            // parsed instant if the value was understood, the raw string
            // otherwise (so a hand-written ISO filter is not silently dropped).
            from: normalize_filter_datetime(&q.from),
            to: normalize_filter_datetime(&q.to),
            notice: None,
        },
        StatusCode::OK,
    )
}

/// A date filter typed into an `<input type="datetime-local">`
/// (`2026-09-04T14:02`, optionally with seconds) *or* a full RFC3339 string.
///
/// The local form has no zone, so it is read as UTC: an audit filter is
/// operator tooling over UTC-stored rows, and guessing a zone here would move
/// the boundary by hours without saying so. Bookmarked ISO URLs from before
/// this change keep working.
pub(crate) fn parse_filter_datetime(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(dt) = parse_datetime(s) {
        return Some(dt);
    }
    for format in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M", "%Y-%m-%d"] {
        if let Ok(naive) = chrono::NaiveDateTime::parse_from_str(s, format) {
            return Some(naive.and_utc());
        }
    }
    // A bare date parses as midnight UTC.
    if let Ok(date) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc());
    }
    None
}

/// The value to put back in the form field: the canonical `datetime-local`
/// rendering when the filter parsed, the raw input when it did not.
pub(crate) fn normalize_filter_datetime(raw: &str) -> String {
    match parse_filter_datetime(raw) {
        Some(dt) => view::datetime_local_value(dt),
        None => raw.trim().to_string(),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct AdminNotices {
    #[serde(default)]
    granted: Option<String>,
    #[serde(default)]
    revoked: Option<String>,
    #[serde(default)]
    suspended: Option<String>,
    #[serde(default)]
    restored: Option<String>,
    #[serde(default)]
    error: Option<String>,
    /// Search term: matches email or display name.
    #[serde(default)]
    q: String,
    /// Keyset cursor: the last (smallest) id already shown. `0` = first page.
    #[serde(default)]
    after_id: i64,
}

pub(crate) async fn admin_users(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Query(q): Query<AdminNotices>,
) -> Response {
    let tr = Translator::new(locale);
    match auth.require_role(Role::Admin) {
        Ok(_) => {}
        Err(resp) => return resp,
    }
    let query = q.q.trim();
    let users = state
        .auth
        .search_users(
            (!query.is_empty()).then_some(query),
            parse_after_id(q.after_id),
            DEFAULT_PAGE_LIMIT,
        )
        .await
        .unwrap_or_default();
    // The list used to load every account in the database; it is now a bounded
    // page, and the counters for that page come from one extra query.
    let ids: Vec<i64> = users.iter().map(|u| u.id.0).collect();
    let activity = state.auth.user_activity(&ids).await.unwrap_or_default();
    let next_url = (users.len() as i64 == DEFAULT_PAGE_LIMIT)
        .then(|| users.last())
        .flatten()
        .map(|last| {
            format!(
                "/admin/users?q={}&after_id={}",
                urlencoding_query(query),
                last.id.0
            )
        });
    let users = view::admin_users(tr, &users, &activity);
    render(
        AdminUsersPage {
            layout: PageLayout::for_request(
                tr.t("admin.users_title").to_string(),
                "admin",
                &auth,
                &state.map,
            ),
            tr,
            users,
            query: query.to_string(),
            next_url,
            notice: if q.granted.is_some() {
                Some(tr.t("admin.granted").to_string())
            } else if q.revoked.is_some() {
                Some(tr.t("admin.revoked").to_string())
            } else if q.suspended.is_some() {
                Some(tr.t("admin.suspended").to_string())
            } else if q.restored.is_some() {
                Some(tr.t("admin.restored").to_string())
            } else {
                None
            },
            error: q.error.as_deref().map(|code| admin_users_error(tr, code)),
        },
        StatusCode::OK,
    )
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct RoleForm {
    #[serde(default)]
    action: String,
    #[serde(default)]
    role: String,
}

pub(crate) async fn admin_role_post(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<i64>,
    Form(form): Form<RoleForm>,
) -> Response {
    let actor = match auth.require_role(Role::Admin) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let target = UserId(id);
    let Some(role) = Role::from_code(&form.role) else {
        return axum::response::Redirect::to("/admin/users?error=1").into_response();
    };
    let result = match form.action.as_str() {
        "grant" => state.auth.grant_role(actor, target, role).await,
        "revoke" => state.auth.revoke_role(actor, target, role).await,
        _ => return axum::response::Redirect::to("/admin/users?error=1").into_response(),
    };
    match result {
        Ok(()) => {
            let path = if form.action == "grant" {
                "/admin/users?granted=1"
            } else {
                "/admin/users?revoked=1"
            };
            axum::response::Redirect::to(path).into_response()
        }
        Err(_) => axum::response::Redirect::to("/admin/users?error=1").into_response(),
    }
}

/// GET /admin/privacy-requests — the manual rights queue (ADMIN-only).
pub(crate) async fn admin_privacy_requests(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
) -> Response {
    let tr = Translator::new(locale);
    let admin = match auth.require_role(Role::Admin) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    let requests = state
        .privacy
        .list_requests(admin, None)
        .await
        .unwrap_or_default();
    // One batched lookup for every subject on the page — a rights queue that
    // does not say whose rights they are cannot be worked.
    let mut subject_ids: Vec<i64> = requests
        .iter()
        .filter_map(|r| r.user_id.map(|u| u.0))
        .collect();
    subject_ids.sort_unstable();
    subject_ids.dedup();
    let labels = state
        .auth
        .user_labels(&subject_ids)
        .await
        .unwrap_or_default();
    let items: Vec<view::PrivacyRequestVm> = requests
        .iter()
        .map(|r| view::privacy_request_vm(tr, r, &labels))
        .collect();
    render(
        AdminPrivacyRequestsPage {
            layout: PageLayout::for_request(
                tr.t("admin.privacy_requests.title").to_string(),
                "moderation",
                &auth,
                &state.map,
            ),
            tr,
            items,
            notice: None,
        },
        StatusCode::OK,
    )
}

/// POST /admin/privacy-requests/{id}/fulfill — mark a manual request COMPLETED.
pub(crate) async fn admin_privacy_request_fulfill(
    State(state): State<AppState>,
    _locale: Locale,
    auth: Auth,
    Path(id): Path<i64>,
) -> Response {
    let admin = match auth.require_role(Role::Admin) {
        Ok(a) => a,
        Err(resp) => return resp,
    };
    match state.privacy.fulfill_request(admin, id).await {
        Ok(()) => Redirect::to("/admin/privacy-requests?fulfilled=1").into_response(),
        Err(_) => Redirect::to("/admin/privacy-requests?error=1").into_response(),
    }
}
