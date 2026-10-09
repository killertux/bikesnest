//! Moderation and admin (audit, job health) pages and fragments.

use crate::i18n::Translator;
use crate::{PageLayout, view};
use askama::Template;

/// Moderation dashboard (counts and links to the queues).
#[derive(Template)]
#[template(path = "pages/moderation_dashboard.html")]
pub struct ModerationDashboardPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub pending_photos: i64,
    pub open_reports: i64,
    pub under_review_reports: i64,
    pub pending_proposals: i64,
    pub is_admin: bool,
}

/// Reports queue.
#[derive(Template)]
#[template(path = "pages/moderation_reports.html")]
pub struct ModerationReportsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub state_filter: String,
    pub items: Vec<view::ReportVm>,
    /// The current moderator's id — the template hides resolve/dismiss on one's
    /// own report (the server guard still enforces it).
    pub viewer_id: i64,
    pub notice: Option<String>,
    pub next_url: Option<String>,
}

/// Proposal review queue.
#[derive(Template)]
#[template(path = "pages/moderation_proposals.html")]
pub struct ModerationProposalsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::ProposalVm>,
    pub notice: Option<String>,
    pub next_url: Option<String>,
}

/// Admin audit-log viewer.
#[derive(Template)]
#[template(path = "pages/admin_audit.html")]
pub struct AdminAuditPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub items: Vec<view::AuditRowVm>,
    pub next_cursor: Option<i64>,
    pub action: String,
    pub target_type: String,
    pub actor: String,
    pub from: String,
    pub to: String,
    pub notice: Option<String>,
}

/// Admin: one user's contribution history.
#[derive(Template)]
#[template(path = "pages/admin_user_contributions.html")]
pub struct AdminUserContributionsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub user_id: i64,
    pub email: String,
    pub items: Vec<view::ContributionVm>,
}

/// HTMX fragment: the report-submit result (success or error).
#[derive(Template)]
#[template(path = "partials/report_result.html")]
pub struct ReportResultVm {
    pub tr: Translator,
    pub state: &'static str,
    pub message: String,
}

/// HTMX fragment: a generic moderation-action toast.
#[derive(Template)]
#[template(path = "partials/moderation_action_result.html")]
pub struct ModerationActionResultVm {
    pub tr: Translator,
    pub state: &'static str,
    pub message: String,
}

/// Admin: background-job health (recurring jobs and one-shot queue pressure).
#[derive(Template)]
#[template(path = "pages/admin_jobs.html")]
pub struct AdminJobsPage {
    pub layout: PageLayout,
    pub tr: Translator,
    pub health: view::JobHealthVm,
}

/// HTMX fragment: the job-health region `pages/admin_jobs.html` polls.
#[derive(Template)]
#[template(path = "partials/admin_jobs_status.html")]
pub struct AdminJobsStatusVm {
    pub tr: Translator,
    pub health: view::JobHealthVm,
}
