//! Authentication, account, privacy, legal and admin-user pages.

use crate::i18n::Translator;
use crate::{PageLayout, view};
use askama::Template;

/// Register.
#[derive(Template)]
#[template(path = "pages/register.html")]
pub struct RegisterPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub display_name: String,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to.
    pub field_errors: view::FieldErrors,
    pub terms_required: bool,
    pub terms_policy_id: i64,
    pub terms_version: String,
    pub terms_url: String,
}

/// Login.
#[derive(Template)]
#[template(path = "pages/login.html")]
pub struct LoginPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub notice: Option<String>,
    pub error: Option<String>,
    /// Which input(s) a rejected submission belongs to. A
    /// bad login never says which of email/password was wrong, so a failure
    /// flags both rather than disclosing one over the other.
    pub field_errors: view::FieldErrors,
    /// Where to send the user after a successful login — already reduced to a
    /// safe local path (`htmx::safe_local_path`), empty when there is none.
    /// Rendered as a hidden field so the no-JS round trip keeps it.
    pub next: String,
    /// Google sign-in feature flag: when false the link is replaced by a
    /// disabled "coming soon" button (product decision: disabled until a real
    /// OAuth provider exists).
    pub google_enabled: bool,
}

/// Email verification: activation form (`mode` "activate"), email-change
/// confirmation ("confirm"), or invalid link + resend ("invalid").
#[derive(Template)]
#[template(path = "pages/verify_email.html")]
pub struct VerifyEmailPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub mode: &'static str,
    pub token: String,
    pub error: Option<String>,
}

/// Request a password reset.
#[derive(Template)]
#[template(path = "pages/password_reset.html")]
pub struct PasswordResetPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// Set a new password.
#[derive(Template)]
#[template(path = "pages/password_reset_new.html")]
pub struct PasswordResetNewPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub token: String,
    pub error: Option<String>,
}

/// Account overview.
#[derive(Template)]
#[template(path = "pages/account.html")]
pub struct AccountPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub display_name: Option<String>,
    pub public_contribution_name: bool,
    pub is_verified: bool,
    pub roles_label: String,
    pub notice: Option<String>,
    pub terms_notices: Vec<TermsNoticeVm>,
}

#[derive(Debug, Clone)]
pub struct TermsNoticeVm {
    pub url: String,
    pub version: String,
    pub effective_label: String,
    pub effective_at: String,
    pub future: bool,
}

#[derive(Template)]
#[template(path = "pages/terms_notice.html")]
pub struct TermsNoticePage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub policy_id: i64,
    pub version: String,
    pub effective_label: String,
    pub effective_at: String,
    pub content: String,
    pub future: bool,
    pub error: Option<String>,
}

/// Change password.
#[derive(Template)]
#[template(path = "pages/account_password.html")]
pub struct AccountPasswordPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub error: Option<String>,
    pub notice: Option<String>,
}

/// Change email.
#[derive(Template)]
#[template(path = "pages/account_email.html")]
pub struct AccountEmailPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub email: String,
    pub error: Option<String>,
    pub notice: Option<String>,
}

/// User management (role assignment).
#[derive(Template)]
#[template(path = "pages/admin_users.html")]
pub struct AdminUsersPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub users: Vec<view::AdminUserVm>,
    /// The current search term, echoed into the search box.
    pub query: String,
    /// Keyset "load more" link, present only when the page was full.
    pub next_url: Option<String>,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// A versioned legal page: current version and effective date.
#[derive(Template)]
#[template(path = "pages/policy.html")]
pub struct PolicyPage {
    pub layout: PageLayout,
    pub tr: Translator,
    /// Stable kind code ("privacy" | "terms" | "cookies").
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub version: String,
    pub effective_label: String,
    /// HTML produced by [`markdown::render_policy_markdown`] from the stored
    /// markdown (raw HTML in the source is escaped there). This is the only
    /// template field marked `|safe`.
    pub content: String,
}

/// The version history for a legal page.
#[derive(Template)]
#[template(path = "pages/policy_versions.html")]
pub struct PolicyVersionsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub kind_code: &'static str,
    pub kind_label: &'static str,
    pub items: Vec<view::PolicyVersionVm>,
}

/// Privacy and data hub.
#[derive(Template)]
#[template(path = "pages/account_privacy.html")]
pub struct AccountPrivacyPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub request_types: Vec<view::PrivacyRequestKindVm>,
    pub consent_records: bool,
    pub notice: Option<String>,
    pub error: Option<String>,
}

/// Data export status.
#[derive(Template)]
#[template(path = "pages/account_export.html")]
pub struct AccountExportPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::ExportVm>,
    pub notice: Option<String>,
}

/// Account deletion confirmation.
#[derive(Template)]
#[template(path = "pages/account_delete.html")]
pub struct AccountDeletePage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub error: Option<String>,
}

/// Admin privacy-request queue.
#[derive(Template)]
#[template(path = "pages/admin_privacy_requests.html")]
pub struct AdminPrivacyRequestsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::PrivacyRequestVm>,
    pub notice: Option<String>,
}
