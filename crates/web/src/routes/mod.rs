//! Request handlers, one module per slice of the product, plus the table that
//! maps URLs onto them.
//!
//! The table lives here rather than in [`crate::wiring`] so that a new
//! endpoint is one directory's worth of change: the handler and its route sit
//! side by side. `wiring` keeps the other half of the job — building the
//! providers and the middleware stack — and calls [`routes`] with the finished
//! state.
//!
//! No module in here may reach for a repository or a connection pool: every
//! handler goes through an application port held in
//! [`AppState`](crate::state::AppState).

pub mod admin;
pub mod api;
pub mod auth;
pub mod common;
pub mod community;
pub mod contribution_form;
pub mod details;
pub mod errors;
pub mod legal;
pub mod moderation;
pub mod photo;
pub mod privacy;
pub mod public;
pub mod reviews;
pub mod search;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, header};
use axum::routing::{get, post};

use crate::state::AppState;

use admin::{
    admin_audit, admin_privacy_request_fulfill, admin_privacy_requests, admin_role_post,
    admin_user_contributions, admin_user_restore, admin_user_suspend, admin_users,
};
use api::geocode_api;
use auth::{
    account, account_email, account_email_post, account_password, account_password_post,
    account_public_name_post, auth_google, auth_google_callback, auth_google_fake_consent,
    login_page, login_post, logout, password_reset_new, password_reset_new_post,
    password_reset_page, password_reset_post, register_page, register_post, verify_email,
    verify_resend,
};
use community::{
    parking_edit_page, parking_edit_post, parking_new_page, parking_new_post,
    parking_proposal_post, parking_proposal_vote_post,
};
use details::parking_details;
use errors::not_found;
use legal::{
    cookies_page, cookies_versions, privacy_page, privacy_versions, terms_page, terms_versions,
};
use moderation::{
    moderation_dashboard, moderation_parking_invalidate, moderation_parking_restore,
    moderation_proposal_approve, moderation_proposal_reject, moderation_proposals,
    moderation_report_claim, moderation_report_dismiss, moderation_report_resolve,
    moderation_reports, moderation_review_hide, moderation_review_restore, report_submit,
};
use photo::{
    moderation_photo_approve, moderation_photo_hide, moderation_photo_reject,
    moderation_photo_restore, moderation_photos, upload_photo,
};
use privacy::{
    account_delete, account_delete_post, account_export, account_export_download,
    account_export_post, account_privacy, account_privacy_request_post,
};
use public::{about, healthz, home, readyz, robots_txt, set_lang, sitemap_xml};
use reviews::{
    account_contributions, account_favorites, parking_favorite_post, parking_parked_here_post,
    parking_verify_post, review_page, review_post,
};
use search::search;

/// The URL → handler table. Returns the router still awaiting its state, so
/// [`crate::wiring`] can wrap it in middleware before calling `with_state`.
pub(crate) fn routes(state: &AppState) -> Router<AppState> {
    let mut router = Router::new()
        .route("/", get(home))
        .route("/search", get(search))
        // Address → coordinates for the add/edit map picker. Signed-in and
        // verified only, and metered on the same per-IP geocode budget as
        // `/search`, because it reaches the same billable provider.
        .route("/api/geocode", get(geocode_api))
        .route("/parking/{id}", get(parking_details))
        .route("/about", get(about))
        .route("/robots.txt", get(robots_txt))
        .route("/sitemap.xml", get(sitemap_xml))
        .route("/lang/{code}", get(set_lang))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        // --- Accounts & authentication (M2) ---
        .route("/register", get(register_page).post(register_post))
        .route("/login", get(login_page).post(login_post))
        .route("/logout", post(logout))
        .route("/verify-email", get(verify_email))
        .route("/verify-email/resend", post(verify_resend))
        .route(
            "/password-reset",
            get(password_reset_page).post(password_reset_post),
        )
        .route(
            "/password-reset/new",
            get(password_reset_new).post(password_reset_new_post),
        );
    // Google sign-in (product decision: disabled until a real OAuth provider
    // exists). Unregistered routes fall through to the styled 404 handler.
    if state.google_oauth_enabled {
        router = router
            .route("/auth/google", get(auth_google))
            .route("/auth/google/fake-consent", get(auth_google_fake_consent))
            .route("/auth/google/callback", get(auth_google_callback));
    }
    router
        .route("/account", get(account))
        .route("/account/public-name", post(account_public_name_post))
        .route(
            "/account/password",
            get(account_password).post(account_password_post),
        )
        .route(
            "/account/email",
            get(account_email).post(account_email_post),
        )
        // --- M6 privacy & account lifecycle ---
        .route("/privacy", get(privacy_page))
        .route("/terms", get(terms_page))
        .route("/cookies", get(cookies_page))
        .route("/privacy/versions", get(privacy_versions))
        .route("/terms/versions", get(terms_versions))
        .route("/cookies/versions", get(cookies_versions))
        .route("/account/privacy", get(account_privacy))
        .route("/account/privacy/export", post(account_export_post))
        .route(
            "/account/privacy/request",
            post(account_privacy_request_post),
        )
        .route("/account/export/{id}", get(account_export))
        .route(
            "/account/export/{id}/download",
            get(account_export_download),
        )
        .route(
            "/account/delete",
            get(account_delete).post(account_delete_post),
        )
        .route("/admin/privacy-requests", get(admin_privacy_requests))
        .route(
            "/admin/privacy-requests/{id}/fulfill",
            post(admin_privacy_request_fulfill),
        )
        // --- M3 community contributions ---
        .route("/parking/new", get(parking_new_page).post(parking_new_post))
        .route(
            "/parking/{id}/edit",
            get(parking_edit_page).post(parking_edit_post),
        )
        .route("/parking/{id}/proposal", post(parking_proposal_post))
        .route(
            "/parking/proposals/{id}/vote",
            post(parking_proposal_vote_post),
        )
        .route("/parking/{id}/review", get(review_page).post(review_post))
        .route("/parking/{id}/verify", post(parking_verify_post))
        .route("/parking/{id}/parked-here", post(parking_parked_here_post))
        .route("/parking/{id}/favorite", post(parking_favorite_post))
        .route("/account/favorites", get(account_favorites))
        .route("/account/contributions", get(account_contributions))
        .route("/admin/users", get(admin_users))
        .route("/admin/users/{id}/role", post(admin_role_post))
        // --- M4 photos (upload → moderate → publish) ---
        .route(
            "/parking/{id}/photo",
            post(upload_photo).layer(DefaultBodyLimit::max(
                state.config.photo.max_bytes + 64 * 1024,
            )),
        )
        .route("/moderation/photos", get(moderation_photos))
        .route(
            "/moderation/photos/{kind}/{id}/approve",
            post(moderation_photo_approve),
        )
        .route(
            "/moderation/photos/{kind}/{id}/reject",
            post(moderation_photo_reject),
        )
        .route(
            "/moderation/photos/{kind}/{id}/hide",
            post(moderation_photo_hide),
        )
        .route(
            "/moderation/photos/{kind}/{id}/restore",
            post(moderation_photo_restore),
        )
        // --- M5 reports + moderation actions + audit viewer ---
        .route("/reports", post(report_submit))
        .route("/moderation", get(moderation_dashboard))
        .route("/moderation/reports", get(moderation_reports))
        .route(
            "/moderation/reports/{id}/claim",
            post(moderation_report_claim),
        )
        .route(
            "/moderation/reports/{id}/resolve",
            post(moderation_report_resolve),
        )
        .route(
            "/moderation/reports/{id}/dismiss",
            post(moderation_report_dismiss),
        )
        .route("/moderation/proposals", get(moderation_proposals))
        .route(
            "/moderation/proposals/{id}/approve",
            post(moderation_proposal_approve),
        )
        .route(
            "/moderation/proposals/{id}/reject",
            post(moderation_proposal_reject),
        )
        .route(
            "/moderation/reviews/{id}/hide",
            post(moderation_review_hide),
        )
        .route(
            "/moderation/reviews/{id}/restore",
            post(moderation_review_restore),
        )
        .route(
            "/moderation/parking/{id}/invalidate",
            post(moderation_parking_invalidate),
        )
        .route(
            "/moderation/parking/{id}/restore",
            post(moderation_parking_restore),
        )
        .route("/admin/users/{id}/suspend", post(admin_user_suspend))
        .route("/admin/users/{id}/restore", post(admin_user_restore))
        .route(
            "/admin/users/{id}/contributions",
            get(admin_user_contributions),
        )
        .route("/admin/audit", get(admin_audit))
        // Content-hashed assets (WP14): a more specific static segment
        // ("h") than the `/static/{*rest}` the `nest_service` below expands
        // to, so this route wins the match for any hashed URL. Validates the
        // hash against `state.assets` and answers with a long, immutable
        // `Cache-Control` — see `crate::assets::hashed_static`.
        .route(
            "/static/h/{hash}/{*path}",
            get(crate::assets::hashed_static),
        )
        // Served from `STATIC_ROOT` (resolved at startup), so the binary is
        // relocatable instead of pinned to its compile-time manifest path.
        // Everything under the plain `/static/...` path is unhashed content,
        // so it only gets a short cache lifetime — a hashed URL above is what
        // a page actually links to for anything long-cacheable.
        .nest_service(
            "/static",
            tower::ServiceBuilder::new()
                .layer(
                    tower_http::set_header::SetResponseHeaderLayer::if_not_present(
                        header::CACHE_CONTROL,
                        HeaderValue::from_static("public, max-age=3600"),
                    ),
                )
                .service(tower_http::services::ServeDir::new(
                    &state.config.static_root,
                )),
        )
        .fallback(not_found)
}
