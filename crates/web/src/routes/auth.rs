//! Accounts: register, sign in/out, email verification, password reset,
//! the Google sign-in stub and the account settings pages.

use axum::extract::{Form, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use bikesnest_application::AuthError;
use bikesnest_domain::{Role, UserEmail};

use crate::auth::{
    Auth, anon_csrf_token, clear_session_cookie, random_state_hex, set_session_cookie,
};
use crate::client_ip::ClientIp;
use crate::htmx;
use crate::i18n::{Locale, Translator};
use crate::state::AppState;
use crate::view;
use crate::{
    AccountEmailPage, AccountPage, AccountPasswordPage, LoginPage, PageLayout,
    PasswordResetNewPage, PasswordResetPage, RegisterPage, VerifyEmailPage,
};

use super::common::{locale_code, render, render_anon};

pub(crate) fn redirect_with_cookie(path: &str, cookie: &str) -> Response {
    (
        [(header::SET_COOKIE, cookie)],
        axum::response::Redirect::to(path),
    )
        .into_response()
}

pub(crate) fn auth_error_message(tr: Translator, err: &AuthError) -> String {
    match err {
        AuthError::WeakPassword => tr.t("auth.error.weak_password").to_string(),
        AuthError::InvalidCurrentPassword => tr.t("account.pw_current_incorrect").to_string(),
        AuthError::InvalidEmail => tr.t("auth.error.invalid_email").to_string(),
        AuthError::RateLimited => tr.t("auth.error.rate_limited").to_string(),
        AuthError::TokenExpired | AuthError::TokenUsed | AuthError::TokenInvalid => {
            tr.t("auth.error.invalid_token").to_string()
        }
        AuthError::RefuseAdminSelfRevoke => tr.t("auth.error.last_admin").to_string(),
        AuthError::Conflict => tr.t("error.conflict").to_string(),
        AuthError::Unavailable => tr.t("error.unavailable").to_string(),
        _ => tr.t("auth.error.generic").to_string(),
    }
}

/// Which register input a rejected submission belongs to:
/// `None` for the errors that are not about one particular field (rate
/// limits, conflicts, "try again") — those stay a banner-only message.
pub(crate) fn register_field_error(err: &AuthError) -> Option<&'static str> {
    match err {
        AuthError::EmailTaken | AuthError::InvalidEmail => Some("email"),
        AuthError::WeakPassword => Some("password"),
        _ => None,
    }
}

pub(crate) fn format_roles(tr: Translator, mut roles: Vec<Role>) -> String {
    roles.sort();
    roles.dedup();
    roles
        .iter()
        .map(|r| view::role_label(tr, *r))
        .collect::<Vec<_>>()
        .join(", ")
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct RegisterForm {
    #[serde(default)]
    email: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    password: String,
}

pub(crate) async fn register_page(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
) -> Response {
    if auth.authenticated() {
        return axum::response::Redirect::to("/account").into_response();
    }
    let tr = Translator::new(locale);
    let token = anon_csrf_token();
    render_anon(
        RegisterPage {
            layout: PageLayout::new(&state.map, tr.t("auth.register_title").to_string(), "auth")
                .csrf(token.clone()),
            tr,
            email: String::new(),
            display_name: String::new(),
            error: None,
            field_errors: view::FieldErrors::new(),
        },
        &token,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn register_post(
    State(state): State<AppState>,
    locale: Locale,
    ClientIp(ip): ClientIp,
    auth: Auth,
    Form(form): Form<RegisterForm>,
) -> Response {
    if auth.authenticated() {
        return axum::response::Redirect::to("/account").into_response();
    }
    let tr = Translator::new(locale);
    let display_name = if form.display_name.trim().is_empty() {
        None
    } else {
        Some(form.display_name.trim())
    };
    match state
        .auth
        .register(
            &ip,
            &form.email,
            display_name,
            &form.password,
            locale_code(locale),
        )
        .await
    {
        Ok(()) => axum::response::Redirect::to("/login?registered=1").into_response(),
        Err(err) => {
            // Re-render with a fresh double-submit CSRF token so the next POST validates.
            let token = anon_csrf_token();
            let message = auth_error_message(tr, &err);
            let field_errors = match register_field_error(&err) {
                Some(field) => view::FieldErrors::single(field, message.clone()),
                None => view::FieldErrors::new(),
            };
            render_anon(
                RegisterPage {
                    layout: PageLayout::new(
                        &state.map,
                        tr.t("auth.register_title").to_string(),
                        "auth",
                    )
                    .csrf(token.clone()),
                    tr,
                    email: form.email,
                    display_name: form.display_name,
                    error: Some(message),
                    field_errors,
                },
                &token,
            )
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct LoginNotices {
    #[serde(default)]
    registered: Option<String>,
    #[serde(default)]
    verified: Option<String>,
    #[serde(default)]
    reset: Option<String>,
    #[serde(default)]
    resend: Option<String>,
    #[serde(default)]
    oauth: Option<String>,
    /// Where the gate that bounced the user wanted them to end up.
    #[serde(default)]
    next: String,
}

/// Build the notice shown on the login page from a query-string flag.
pub(crate) fn login_notice(tr: Translator, q: &LoginNotices) -> Option<String> {
    if q.registered.is_some() {
        Some(tr.t("auth.registered").to_string())
    } else if q.verified.is_some() {
        Some(tr.t("auth.verified").to_string())
    } else if q.reset.is_some() {
        Some(tr.t("auth.reset_sent").to_string())
    } else if q.resend.is_some() {
        Some(tr.t("auth.resend_sent").to_string())
    } else if q.oauth.is_some() {
        Some(tr.t("auth.oauth_failed").to_string())
    } else {
        None
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct LoginForm {
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
    /// Carried across the round trip by the hidden field on the login form.
    #[serde(default)]
    next: String,
}

/// Where a login lands. `next` comes from the user (a query parameter, then a
/// form field), so it is reduced to a safe local path or dropped — `/account`
/// is the default and the only answer for `//evil.com` or `/\evil.com`.
pub(crate) fn login_destination(next: &str) -> String {
    htmx::safe_local_path(next)
        .unwrap_or("/account")
        .to_string()
}

pub(crate) async fn login_page(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Query(q): Query<LoginNotices>,
) -> Response {
    if auth.authenticated() {
        return axum::response::Redirect::to(&login_destination(&q.next)).into_response();
    }
    let tr = Translator::new(locale);
    let token = anon_csrf_token();
    render_anon(
        LoginPage {
            layout: PageLayout::new(&state.map, tr.t("auth.login_title").to_string(), "auth")
                .csrf(token.clone()),
            tr,
            email: String::new(),
            notice: login_notice(tr, &q),
            error: None,
            field_errors: view::FieldErrors::new(),
            next: htmx::safe_local_path(&q.next).unwrap_or("").to_string(),
            google_enabled: state.google_oauth_enabled,
        },
        &token,
    )
}

pub(crate) async fn login_post(
    State(state): State<AppState>,
    locale: Locale,
    ClientIp(ip): ClientIp,
    auth: Auth,
    Query(q): Query<LoginNotices>,
    Form(form): Form<LoginForm>,
) -> Response {
    // The form field wins (it survives a failed attempt); the query parameter
    // is the fallback for a link straight to `/login?next=…`.
    let next = if form.next.is_empty() {
        q.next.clone()
    } else {
        form.next.clone()
    };
    if auth.authenticated() {
        return axum::response::Redirect::to(&login_destination(&next)).into_response();
    }
    let tr = Translator::new(locale);
    match state.auth.login(&ip, &form.email, &form.password).await {
        Ok(outcome) => redirect_with_cookie(
            &login_destination(&next),
            &set_session_cookie(&outcome.session),
        ),
        // One generic message for bad credentials AND suspended/deleted, so the
        // reply never reveals which of the two it was.
        // The submitted email is NOT echoed back, so the failure response is
        // byte-identical whether or not the account exists — and it still
        // carries a fresh double-submit CSRF token for the next attempt.
        Err(_) => {
            tracing::warn!("login failed"); // no email/IP/PII in the log field
            let token = anon_csrf_token();
            let message = tr.t("auth.error.invalid_credentials").to_string();
            // Never disclose which of the two was wrong: both inputs are
            // flagged with the same generic message rather
            // than only one, which would leak that the other was correct.
            let mut field_errors = view::FieldErrors::new();
            field_errors.push("email", message.clone());
            field_errors.push("password", message.clone());
            render_anon(
                LoginPage {
                    layout: PageLayout::new(
                        &state.map,
                        tr.t("auth.login_title").to_string(),
                        "auth",
                    )
                    .csrf(token.clone()),
                    tr,
                    email: String::new(),
                    notice: None,
                    error: Some(message),
                    field_errors,
                    next: htmx::safe_local_path(&next).unwrap_or("").to_string(),
                    google_enabled: state.google_oauth_enabled,
                },
                &token,
            )
        }
    }
}

pub(crate) async fn logout(State(state): State<AppState>, auth: Auth) -> Response {
    if let Some(session) = &auth.session {
        let _ = state.auth.logout(session).await;
    }
    (
        [(header::SET_COOKIE, clear_session_cookie())],
        axum::response::Redirect::to("/"),
    )
        .into_response()
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct VerifyParams {
    #[serde(default)]
    token: Option<String>,
}

pub(crate) async fn verify_email(
    State(state): State<AppState>,
    locale: Locale,
    Query(q): Query<VerifyParams>,
) -> Response {
    let tr = Translator::new(locale);
    let Some(token) = q.token.filter(|t| !t.is_empty()) else {
        let t = anon_csrf_token();
        return render_anon(
            VerifyEmailPage {
                layout: PageLayout::new(&state.map, tr.t("auth.verify_title").to_string(), "auth")
                    .csrf(t.clone()),
                tr,
                success: false,
                error: Some(tr.t("auth.error.invalid_token").to_string()),
            },
            &t,
        );
    };
    match state.auth.verify_email(&token).await {
        Ok(()) => axum::response::Redirect::to("/login?verified=1").into_response(),
        Err(err) => {
            let t = anon_csrf_token();
            render_anon(
                VerifyEmailPage {
                    layout: PageLayout::new(
                        &state.map,
                        tr.t("auth.verify_title").to_string(),
                        "auth",
                    )
                    .csrf(t.clone()),
                    tr,
                    success: false,
                    error: Some(auth_error_message(tr, &err)),
                },
                &t,
            )
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ResendForm {
    #[serde(default)]
    email: String,
}

pub(crate) async fn verify_resend(
    State(state): State<AppState>,
    locale: Locale,
    ClientIp(ip): ClientIp,
    Form(form): Form<ResendForm>,
) -> Response {
    let tr = Translator::new(locale);
    let Ok(email) = UserEmail::parse(&form.email) else {
        return axum::response::Redirect::to("/login?resend=1").into_response();
    };
    match state.auth.resend_verification(&ip, &email).await {
        Ok(()) => axum::response::Redirect::to("/login?resend=1").into_response(),
        Err(err) => {
            let t = anon_csrf_token();
            render_anon(
                LoginPage {
                    layout: PageLayout::new(
                        &state.map,
                        tr.t("auth.login_title").to_string(),
                        "auth",
                    )
                    .csrf(t.clone()),
                    tr,
                    email: String::new(),
                    notice: None,
                    error: Some(auth_error_message(tr, &err)),
                    field_errors: view::FieldErrors::new(),
                    next: String::new(),
                    google_enabled: state.google_oauth_enabled,
                },
                &t,
            )
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ResetRequestForm {
    #[serde(default)]
    email: String,
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ResetSent {
    #[serde(default)]
    sent: Option<String>,
}

pub(crate) async fn password_reset_page(
    State(state): State<AppState>,
    locale: Locale,
    Query(q): Query<ResetSent>,
) -> Response {
    let tr = Translator::new(locale);
    let notice = if q.sent.is_some() {
        Some(tr.t("auth.reset_sent").to_string())
    } else {
        None
    };
    let token = anon_csrf_token();
    render_anon(
        PasswordResetPage {
            layout: PageLayout::new(&state.map, tr.t("auth.reset_title").to_string(), "auth")
                .csrf(token.clone()),
            tr,
            email: String::new(),
            notice,
            error: None,
        },
        &token,
    )
}

pub(crate) async fn password_reset_post(
    State(state): State<AppState>,
    locale: Locale,
    ClientIp(ip): ClientIp,
    Form(form): Form<ResetRequestForm>,
) -> Response {
    let tr = Translator::new(locale);
    let Ok(email) = UserEmail::parse(&form.email) else {
        return axum::response::Redirect::to("/password-reset?sent=1").into_response();
    };
    match state.auth.request_password_reset(&ip, &email).await {
        Ok(()) => axum::response::Redirect::to("/password-reset?sent=1").into_response(),
        Err(err) => {
            let t = anon_csrf_token();
            render_anon(
                PasswordResetPage {
                    layout: PageLayout::new(
                        &state.map,
                        tr.t("auth.reset_title").to_string(),
                        "auth",
                    )
                    .csrf(t.clone()),
                    tr,
                    email: form.email,
                    notice: None,
                    error: Some(auth_error_message(tr, &err)),
                },
                &t,
            )
        }
    }
}

pub(crate) async fn password_reset_new(
    State(state): State<AppState>,
    locale: Locale,
    Query(q): Query<VerifyParams>,
) -> Response {
    let tr = Translator::new(locale);
    let token = q.token.unwrap_or_default();
    let t = anon_csrf_token();
    render_anon(
        PasswordResetNewPage {
            layout: PageLayout::new(&state.map, tr.t("auth.reset_new_title").to_string(), "auth")
                .csrf(t.clone()),
            tr,
            token,
            error: None,
        },
        &t,
    )
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ResetNewForm {
    #[serde(default)]
    token: String,
    #[serde(default)]
    password: String,
}

pub(crate) async fn password_reset_new_post(
    State(state): State<AppState>,
    locale: Locale,
    Form(form): Form<ResetNewForm>,
) -> Response {
    let tr = Translator::new(locale);
    match state.auth.reset_password(&form.token, &form.password).await {
        Ok(()) => axum::response::Redirect::to("/login?reset=1").into_response(),
        Err(err) => {
            let t = anon_csrf_token();
            render_anon(
                PasswordResetNewPage {
                    layout: PageLayout::new(
                        &state.map,
                        tr.t("auth.reset_new_title").to_string(),
                        "auth",
                    )
                    .csrf(t.clone()),
                    tr,
                    token: form.token,
                    error: Some(auth_error_message(tr, &err)),
                },
                &t,
            )
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ConsentParams {
    #[serde(default)]
    state: String,
}

pub(crate) async fn auth_google(State(state): State<AppState>) -> Response {
    let state_val = random_state_hex();
    let url = state.auth.oauth_authorize_url(&state_val);
    axum::response::Redirect::to(&url).into_response()
}

/// The fake provider's "consent" page: auto-issues a code that
/// redirects to the real callback route.
pub(crate) async fn auth_google_fake_consent(Query(q): Query<ConsentParams>) -> Response {
    axum::response::Redirect::to(&format!(
        "/auth/google/callback?code=fake-oauth-code&state={}",
        q.state
    ))
    .into_response()
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct CallbackParams {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
}

pub(crate) async fn auth_google_callback(
    State(state): State<AppState>,
    Query(q): Query<CallbackParams>,
) -> Response {
    if q.state.is_empty() {
        return axum::response::Redirect::to("/login?oauth=error").into_response();
    }
    match state.auth.oauth_callback(&q.code).await {
        Ok(outcome) => redirect_with_cookie("/account", &set_session_cookie(&outcome.session)),
        Err(_) => axum::response::Redirect::to("/login?oauth=error").into_response(),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct AccountNotices {
    #[serde(default)]
    pw_changed: Option<String>,
    #[serde(default)]
    email_pending: Option<String>,
    #[serde(default)]
    attribution_saved: Option<String>,
}

pub(crate) async fn account(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Query(q): Query<AccountNotices>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let notice = if q.pw_changed.is_some() {
        Some(tr.t("account.pw_changed").to_string())
    } else if q.email_pending.is_some() {
        Some(tr.t("account.email_pending").to_string())
    } else if q.attribution_saved.is_some() {
        Some(tr.t("account.attribution.saved").to_string())
    } else {
        None
    };
    render(
        AccountPage {
            layout: PageLayout::for_request(
                tr.t("account.title").to_string(),
                "account",
                &auth,
                &state.map,
            ),
            tr,
            email: user.email.to_string(),
            display_name: user.display_name.clone(),
            public_contribution_name: match state.auth.public_contribution_name(user.id).await {
                Ok(enabled) => enabled,
                Err(_) => return StatusCode::SERVICE_UNAVAILABLE.into_response(),
            },
            is_verified: user.is_verified,
            roles_label: format_roles(tr, user.roles.clone()),
            notice,
        },
        StatusCode::OK,
    )
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct PublicNameForm {
    #[serde(default)]
    enabled: Option<String>,
}

pub(crate) async fn account_public_name_post(
    State(state): State<AppState>,
    auth: Auth,
    Form(form): Form<PublicNameForm>,
) -> Response {
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let enabled = match form.enabled.as_deref() {
        None => false,
        Some("true") => true,
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    match state
        .auth
        .set_public_contribution_name(user.id, enabled)
        .await
    {
        Ok(()) => axum::response::Redirect::to("/account?attribution_saved=1").into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ChangePasswordForm {
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_password: String,
}

pub(crate) async fn account_password(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
) -> Response {
    let tr = Translator::new(locale);
    if let Err(resp) = auth.require_user() {
        return resp;
    }
    render(
        AccountPasswordPage {
            layout: PageLayout::for_request(
                tr.t("account.pw_title").to_string(),
                "account",
                &auth,
                &state.map,
            ),
            tr,
            error: None,
            notice: None,
        },
        StatusCode::OK,
    )
}

pub(crate) async fn account_password_post(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Form(form): Form<ChangePasswordForm>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let session = auth.session.as_ref();
    let session = match session {
        Some(s) => s,
        None => return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
    };
    match state
        .auth
        .change_password(user.id, &form.current_password, &form.new_password, session)
        .await
    {
        Ok(()) => axum::response::Redirect::to("/account?pw_changed=1").into_response(),
        Err(err) => render(
            AccountPasswordPage {
                layout: PageLayout::for_request(
                    tr.t("account.pw_title").to_string(),
                    "account",
                    &auth,
                    &state.map,
                ),
                tr,
                error: Some(auth_error_message(tr, &err)),
                notice: None,
            },
            StatusCode::OK,
        ),
    }
}

#[derive(Debug, Default, serde::Deserialize)]
pub(crate) struct ChangeEmailForm {
    #[serde(default)]
    current_password: String,
    #[serde(default)]
    new_email: String,
}

pub(crate) async fn account_email(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    render(
        AccountEmailPage {
            layout: PageLayout::for_request(
                tr.t("account.email_title").to_string(),
                "account",
                &auth,
                &state.map,
            ),
            tr,
            email: user.email.to_string(),
            error: None,
            notice: None,
        },
        StatusCode::OK,
    )
}

pub(crate) async fn account_email_post(
    State(state): State<AppState>,
    locale: Locale,
    auth: Auth,
    Form(form): Form<ChangeEmailForm>,
) -> Response {
    let tr = Translator::new(locale);
    let user = match auth.require_user() {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let Ok(new_email) = UserEmail::parse(&form.new_email) else {
        return render(
            AccountEmailPage {
                layout: PageLayout::for_request(
                    tr.t("account.email_title").to_string(),
                    "account",
                    &auth,
                    &state.map,
                ),
                tr,
                email: user.email.to_string(),
                error: Some(tr.t("auth.error.invalid_email").to_string()),
                notice: None,
            },
            StatusCode::OK,
        );
    };
    match state
        .auth
        .change_email(user.id, &form.current_password, &new_email)
        .await
    {
        Ok(()) => axum::response::Redirect::to("/account?email_pending=1").into_response(),
        Err(err) => render(
            AccountEmailPage {
                layout: PageLayout::for_request(
                    tr.t("account.email_title").to_string(),
                    "account",
                    &auth,
                    &state.map,
                ),
                tr,
                email: user.email.to_string(),
                error: Some(auth_error_message(tr, &err)),
                notice: None,
            },
            StatusCode::OK,
        ),
    }
}

// ---------------------------------------------------------------------------
// Error-mapping unit tests
// ---------------------------------------------------------------------------
//
// The handlers that surface these are covered end to end in
// `tests/http_test.rs`; provoking a real database conflict through HTTP is
// inherently racy, so the mapping itself is pinned here instead.

#[cfg(test)]
mod tests {
    use super::*;

    fn en() -> Translator {
        Translator::new(Locale::En)
    }

    #[test]
    fn expected_auth_errors_get_specific_copy() {
        assert_eq!(
            auth_error_message(en(), &AuthError::InvalidCurrentPassword),
            en().t("account.pw_current_incorrect")
        );
        assert_eq!(
            auth_error_message(en(), &AuthError::Conflict),
            en().t("error.conflict")
        );
        assert_eq!(
            auth_error_message(en(), &AuthError::Unavailable),
            en().t("error.unavailable")
        );
    }
}
