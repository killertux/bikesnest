//! Admin view models: user management, the audit-log viewer and the
//! background-job health view.

use super::format::*;
use crate::i18n::Translator;
use bikesnest_application::{AuthenticatedUser, JobHealth, JobHealthReport};
use bikesnest_domain::{AccountState, Role};

/// Localized role label.
pub fn role_label(t: Translator, role: Role) -> &'static str {
    match role {
        Role::User => t.t("role.user"),
        Role::Moderator => t.t("role.moderator"),
        Role::Admin => t.t("role.admin"),
    }
}

/// Localized account-state label.
pub fn account_state_label(t: Translator, s: AccountState) -> &'static str {
    match s {
        AccountState::PendingEmailVerification => t.t("account.state.pending"),
        AccountState::Active => t.t("account.state.active"),
        AccountState::Suspended => t.t("account.state.suspended"),
        AccountState::Deleted => t.t("account.state.deleted"),
    }
}

/// One row of the admin user-management table.
///
/// The email is **masked** by default: an admin managing roles does not need
/// every address on screen (and neither does anyone glancing at the screen).
/// The full value is one click away in the same row.
#[derive(Debug, Clone)]
pub struct AdminUserVm {
    pub id: i64,
    pub email: String,
    /// `c***@brick.so` — what the row shows until the admin reveals it.
    pub email_masked: String,
    pub display_name: String,
    pub roles_label: String,
    pub state_label: &'static str,
    /// Account-state code (ACTIVE/SUSPENDED/…) for conditional rendering.
    pub state: &'static str,
    pub is_verified: bool,
    pub has_moderator: bool,
    pub has_admin: bool,
    /// Last session activity, absolute; empty for an account that never
    /// signed in.
    pub last_active_label: String,
    pub last_active_title: String,
    pub contributions: i64,
    /// `hx-confirm` copy naming this user, per destructive action.
    pub confirm_suspend: String,
    pub confirm_restore: String,
    pub confirm_grant_moderator: String,
    pub confirm_revoke_moderator: String,
    pub confirm_grant_admin: String,
    pub confirm_revoke_admin: String,
}

/// Mask an email to its first character plus its domain: `c***@brick.so`.
/// A one-character local part still masks (`c***@…`), so the length of the
/// hidden part is never inferable from the mask.
pub fn mask_email(email: &str) -> String {
    let Some((local, domain)) = email.split_once('@') else {
        return "***".to_string();
    };
    match local.chars().next() {
        Some(first) => format!("{first}***@{domain}"),
        None => format!("***@{domain}"),
    }
}

/// Build the admin user list as presentation-ready rows. `activity` is the
/// batched last-seen/contribution lookup for exactly these ids.
pub fn admin_users(
    t: Translator,
    users: &[AuthenticatedUser],
    activity: &std::collections::HashMap<i64, bikesnest_application::UserActivity>,
) -> Vec<AdminUserVm> {
    users
        .iter()
        .map(|u| {
            let mut roles = u.roles.clone();
            roles.sort();
            roles.dedup();
            let email = u.email.to_string();
            let name = u.display_name.clone().unwrap_or_default();
            // Whichever of the two the admin will recognize, for the confirm copy.
            let who = if name.trim().is_empty() {
                mask_email(&email)
            } else {
                name.clone()
            };
            let confirm = |key: &str| t.t(key).replace("{name}", &who);
            let act = activity.get(&u.id.0).copied().unwrap_or_default();
            AdminUserVm {
                id: u.id.0,
                email_masked: mask_email(&email),
                email,
                display_name: name,
                roles_label: roles
                    .iter()
                    .map(|r| role_label(t, *r))
                    .collect::<Vec<_>>()
                    .join(", "),
                state_label: account_state_label(t, u.account_state),
                state: u.account_state.as_code(),
                is_verified: u.is_verified,
                has_moderator: u.has_role(Role::Moderator),
                has_admin: u.has_role(Role::Admin),
                last_active_label: act
                    .last_active_at
                    .map(|at| iso_datetime_label(t, at))
                    .unwrap_or_else(|| t.t("admin.never").to_string()),
                last_active_title: act
                    .last_active_at
                    .map(|at| time_ago_label(t, at))
                    .unwrap_or_default(),
                contributions: act.contributions,
                confirm_suspend: confirm("admin.confirm.suspend"),
                confirm_restore: confirm("admin.confirm.restore"),
                confirm_grant_moderator: confirm("admin.confirm.grant_moderator"),
                confirm_revoke_moderator: confirm("admin.confirm.revoke_moderator"),
                confirm_grant_admin: confirm("admin.confirm.grant_admin"),
                confirm_revoke_admin: confirm("admin.confirm.revoke_admin"),
            }
        })
        .collect()
}

/// One row of the admin audit-log viewer. Metadata rendered as an escaped
/// JSON blob — by construction it carries no secrets/PII.
///
/// An audit log is read to answer "who did what, exactly when" — so the
/// timestamp is absolute (a relative "yesterday" cannot be correlated with
/// anything) and the actor is a name, with the raw id kept for reference.
#[derive(Debug, Clone)]
pub struct AuditRowVm {
    pub id: i64,
    pub actor_label: String,
    /// Link to the actor's account row, when there is an actor.
    pub actor_url: Option<String>,
    pub action: String,
    pub target_label: String,
    /// Link to the target the event names, for the types that have a page.
    pub target_url: Option<String>,
    pub result_label: &'static str,
    pub metadata: String,
    /// Exact UTC instant, unambiguous and sortable.
    pub created_utc: String,
    /// The same instant in the reading operator's locale format.
    pub created_local: String,
    /// The relative phrase, kept as a `title` for a quick sense of recency.
    pub created_title: String,
}

/// Where an audit target can be inspected. Types with no page (a session, a
/// token) get no link rather than a dead one.
pub(super) fn audit_target_url(target_type: &str, target_id: &str) -> Option<String> {
    if target_id.trim().is_empty() {
        return None;
    }
    // Only ever numeric ids reach a URL — anything else is a token/opaque
    // handle and must not be pasted into a path.
    let numeric = target_id.parse::<i64>().ok();
    match target_type {
        "parking_location" => numeric.map(|id| format!("/parking/{id}")),
        "user" => numeric.map(|id| format!("/admin/users?q={id}")),
        "report" => Some("/moderation/reports".to_string()),
        "parking_proposal" => Some("/moderation/proposals".to_string()),
        "parking_photo" | "review_photo" => Some("/moderation/photos".to_string()),
        "privacy_request" => Some("/admin/privacy-requests".to_string()),
        _ => None,
    }
}

/// `labels` is the batched `AccountRepository::labels_for` result for every
/// actor on the page; an id absent from it (a deleted account) keeps the
/// "#id" form so the trail stays readable.
pub fn audit_row_vm(
    t: Translator,
    e: &bikesnest_application::AuditStoredEvent,
    labels: &std::collections::HashMap<i64, String>,
) -> AuditRowVm {
    let actor = e.event.actor_user_id.map(|a| a.0);
    let actor_label = match actor {
        Some(id) => match labels.get(&id) {
            Some(label) => format!("{label} (#{id})"),
            None => format!("{} #{}", t.t("moderation.actor"), id),
        },
        None => t.t("audit.system").to_string(),
    };
    let result_label = if e.event.result == "success" {
        t.t("audit.result.success")
    } else {
        t.t("audit.result.failure")
    };
    AuditRowVm {
        id: e.id,
        actor_label,
        actor_url: actor.map(|id| format!("/admin/users?q={id}")),
        action: e.event.action.clone(),
        target_label: format!("{}:{}", e.event.target_type, e.event.target_id),
        target_url: audit_target_url(&e.event.target_type, &e.event.target_id),
        result_label,
        metadata: e.event.metadata.to_string(),
        created_utc: utc_datetime_label(e.created_at),
        created_local: iso_datetime_label(t, e.created_at),
        created_title: time_ago_label(t, e.created_at),
    }
}

// ---------------------------------------------------------------------------
// Background-job health
// ---------------------------------------------------------------------------

/// The admin job-health view: one row per recurring job plus the one-shot
/// queue's pressure.
#[derive(Debug, Clone)]
pub struct JobHealthVm {
    /// When the report was read (UTC), so a stale tab is recognisable.
    pub checked_label: String,
    pub rows: Vec<JobStatusRowVm>,
    pub overdue_pending: i64,
    pub failed_last_day: i64,
    /// Every recurring row healthy or running, and no one-shot pressure.
    pub all_healthy: bool,
}

/// One recurring job's row.
#[derive(Debug, Clone)]
pub struct JobStatusRowVm {
    pub kind: String,
    pub schedule_label: String,
    /// Stable code ([`JobHealth::as_code`]) for tests and styling hooks.
    pub health_code: &'static str,
    pub health_label: &'static str,
    /// Complete badge class list (see `report_state_badge_class` for why).
    pub health_badge_class: &'static str,
    /// Last success, exact UTC; "never" when the job has not succeeded yet.
    pub last_success_label: String,
    /// The relative phrase, as a `title`.
    pub last_success_title: String,
    pub next_run_label: String,
    /// "12 min overdue", only for a pending row past its run time.
    pub lateness_label: Option<String>,
    pub attempts_label: String,
    pub last_error: Option<String>,
}

pub fn job_health_vm(t: Translator, report: &JobHealthReport) -> JobHealthVm {
    let now = report.checked_at;
    let mut rows: Vec<(JobHealth, JobStatusRowVm)> = report
        .recurring
        .iter()
        .map(|r| {
            let health = r.health(now);
            let row = JobStatusRowVm {
                kind: r.kind.clone(),
                schedule_label: schedule_label(t, &r.schedule),
                health_code: health.as_code(),
                health_label: job_health_label(t, health),
                health_badge_class: job_health_badge_class(health),
                last_success_label: r
                    .last_success_at
                    .map(utc_datetime_label)
                    .unwrap_or_else(|| t.t("admin.never").to_string()),
                last_success_title: r
                    .last_success_at
                    .map(|at| time_ago_label(t, at))
                    .unwrap_or_default(),
                next_run_label: utc_datetime_label(r.next_run_at),
                lateness_label: r.lateness(now).map(|late| {
                    t.t("admin.jobs.overdue")
                        .replace("{d}", &short_duration(late.num_seconds()))
                }),
                attempts_label: format!("{} / {}", r.attempts, r.max_attempts),
                last_error: r.last_error.clone(),
            };
            (health, row)
        })
        .collect();
    // Worst first, so a problem is the first thing on the page.
    rows.sort_by_key(|(health, _)| *health);
    let all_healthy = rows
        .iter()
        .all(|(h, _)| matches!(h, JobHealth::Healthy | JobHealth::Running))
        && report.queue.overdue_pending == 0
        && report.queue.failed_last_day == 0;
    JobHealthVm {
        checked_label: utc_datetime_label(now),
        rows: rows.into_iter().map(|(_, row)| row).collect(),
        overdue_pending: report.queue.overdue_pending,
        failed_last_day: report.queue.failed_last_day,
        all_healthy,
    }
}

fn job_health_label(t: Translator, h: JobHealth) -> &'static str {
    match h {
        JobHealth::Dead => t.t("admin.jobs.health.dead"),
        JobHealth::Stuck => t.t("admin.jobs.health.stuck"),
        JobHealth::Late => t.t("admin.jobs.health.late"),
        JobHealth::Failing => t.t("admin.jobs.health.failing"),
        JobHealth::Running => t.t("admin.jobs.health.running"),
        JobHealth::Healthy => t.t("admin.jobs.health.healthy"),
    }
}

fn job_health_badge_class(h: JobHealth) -> &'static str {
    match h {
        JobHealth::Dead | JobHealth::Stuck | JobHealth::Late => {
            "rounded-full bg-danger/10 px-2 py-0.5 font-medium text-danger"
        }
        JobHealth::Failing => "rounded-full bg-aging/10 px-2 py-0.5 font-medium text-aging-strong",
        JobHealth::Running | JobHealth::Healthy => {
            "rounded-full bg-fresh/10 px-2 py-0.5 font-medium text-fresh-strong"
        }
    }
}

/// `{"every_seconds":N}` → "Every 1 h"; `{"cron":"…"}` → "cron 0 3 * * *".
fn schedule_label(t: Translator, schedule: &serde_json::Value) -> String {
    if let Some(secs) = schedule.get("every_seconds").and_then(|v| v.as_i64()) {
        return t
            .t("admin.jobs.schedule.every")
            .replace("{d}", &short_duration(secs));
    }
    if let Some(cron) = schedule.get("cron").and_then(|v| v.as_str()) {
        return format!("cron {cron}");
    }
    schedule.to_string()
}

/// A compact duration with unit symbols that read the same in en and pt-BR:
/// `45 s`, `12 min`, `3 h 5 min`, `2 d 4 h`.
pub(super) fn short_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3_600, secs % 3_600 / 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{secs} s"),
        (0, 0, m) => format!("{m} min"),
        (0, h, 0) => format!("{h} h"),
        (0, h, m) => format!("{h} h {m} min"),
        (d, 0, _) => format!("{d} d"),
        (d, h, _) => format!("{d} d {h} h"),
    }
}
