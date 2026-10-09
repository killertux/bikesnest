//! Background-job health: whether the recurring jobs (retention, queue GC)
//! are keeping up, and whether one-shot work is piling up or dead-lettering.
//!
//! Readiness (`/readyz`) only proves the database answers; a worker that has
//! stopped claiming, or a recurring job that fails every run, leaves it green.
//! This read model is what an admin looks at (and what the alert queries in
//! `docs/deployment.md` encode) to see the background half of the system.

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::Value;

use crate::auth::{AuthenticatedUser, Clock};
use crate::ports::ReaderError;

/// How far past its `run_at` a pending recurring job may slip before it
/// counts as late. Covers the worker's poll interval, a deploy restart and a
/// busy queue; anything longer means no worker is claiming it.
pub const JOB_LATE_AFTER_SECS: i64 = 15 * 60;

/// One recurring `background_job` row, as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct RecurringJobStatus {
    pub id: i64,
    pub kind: String,
    /// `pending` | `running` | `succeeded` | `failed`.
    pub state: String,
    /// The stored schedule: `{"every_seconds":N}` or `{"cron":"…"}`.
    pub schedule: Value,
    /// `finished_at`: on a recurring row only a successful run writes it, so
    /// it is the last success (a failed occurrence keeps the previous value).
    pub last_success_at: Option<DateTime<Utc>>,
    /// `run_at`: the next scheduled occurrence (or the retry, mid-backoff).
    pub next_run_at: DateTime<Utc>,
    /// Attempts spent on the current occurrence.
    pub attempts: i32,
    pub max_attempts: i32,
    /// The latest failure; cleared by the next success.
    pub last_error: Option<String>,
    /// Set while `running`: when the current claim lapses unless heartbeated.
    pub lease_expires_at: Option<DateTime<Utc>>,
}

/// One-shot queue pressure, alongside the recurring rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct JobQueueSummary {
    /// One-shot jobs due more than [`JOB_LATE_AFTER_SECS`] ago and still pending.
    pub overdue_pending: i64,
    /// One-shot jobs dead-lettered in the last 24 hours.
    pub failed_last_day: i64,
}

/// What an admin sees for one recurring job, worst first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JobHealth {
    /// Dead-lettered: a recurring row only ends up `failed` when its schedule
    /// can no longer be computed, so it will never run again.
    Dead,
    /// `running`, but the lease lapsed without a heartbeat: the worker that
    /// claimed it is gone and the row waits for the exhausted-lease sweep.
    Stuck,
    /// `pending` and more than [`JOB_LATE_AFTER_SECS`] past due: nothing is
    /// claiming it.
    Late,
    /// On schedule, but the last attempt failed (`last_error` is set).
    Failing,
    /// Claimed and heartbeating.
    Running,
    Healthy,
}

impl JobHealth {
    /// Stable code for templates and alerts.
    pub fn as_code(self) -> &'static str {
        match self {
            JobHealth::Dead => "dead",
            JobHealth::Stuck => "stuck",
            JobHealth::Late => "late",
            JobHealth::Failing => "failing",
            JobHealth::Running => "running",
            JobHealth::Healthy => "healthy",
        }
    }
}

impl RecurringJobStatus {
    /// How long past due a pending row is; `None` when it is not due yet or
    /// is not waiting to be claimed.
    pub fn lateness(&self, now: DateTime<Utc>) -> Option<Duration> {
        (self.state == "pending" && self.next_run_at < now).then(|| now - self.next_run_at)
    }

    pub fn health(&self, now: DateTime<Utc>) -> JobHealth {
        match self.state.as_str() {
            "failed" | "succeeded" => return JobHealth::Dead,
            "running" if self.lease_expires_at.is_none_or(|t| t <= now) => {
                return JobHealth::Stuck;
            }
            _ => {}
        }
        if self
            .lateness(now)
            .is_some_and(|late| late > Duration::seconds(JOB_LATE_AFTER_SECS))
        {
            return JobHealth::Late;
        }
        if self.last_error.is_some() {
            return JobHealth::Failing;
        }
        if self.state == "running" {
            JobHealth::Running
        } else {
            JobHealth::Healthy
        }
    }
}

/// Port: the job table, read for health. Implementations read only the
/// bookkeeping columns, never payloads (an `email.send` payload carries an
/// address).
#[async_trait]
pub trait JobHealthReader: Send + Sync {
    /// Every recurring row (`schedule IS NOT NULL`), ordered by kind.
    async fn recurring(&self) -> Result<Vec<RecurringJobStatus>, ReaderError>;
    /// One-shot queue pressure, measured against `now`.
    async fn queue_summary(&self, now: DateTime<Utc>) -> Result<JobQueueSummary, ReaderError>;
}

#[derive(Debug, thiserror::Error)]
pub enum JobHealthError {
    #[error("admin role required")]
    Forbidden,
    #[error(transparent)]
    Read(#[from] ReaderError),
}

/// A point-in-time health report.
#[derive(Debug, Clone, PartialEq)]
pub struct JobHealthReport {
    pub checked_at: DateTime<Utc>,
    pub recurring: Vec<RecurringJobStatus>,
    pub queue: JobQueueSummary,
}

/// Use case: the admin job-health view.
pub struct JobHealthService {
    reader: Box<dyn JobHealthReader>,
    clock: Box<dyn Clock>,
}

impl JobHealthService {
    pub fn new(reader: Box<dyn JobHealthReader>, clock: Box<dyn Clock>) -> Self {
        Self { reader, clock }
    }

    /// ADMIN-only: the recurring rows and the one-shot queue summary.
    pub async fn report(
        &self,
        admin: &AuthenticatedUser,
    ) -> Result<JobHealthReport, JobHealthError> {
        if !admin.has_role(bikesnest_domain::Role::Admin) {
            return Err(JobHealthError::Forbidden);
        }
        let now = self.clock.now();
        Ok(JobHealthReport {
            checked_at: now,
            recurring: self.reader.recurring().await?,
            queue: self.reader.queue_summary(now).await?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: &str) -> RecurringJobStatus {
        RecurringJobStatus {
            id: 1,
            kind: "retention".into(),
            state: state.into(),
            schedule: serde_json::json!({"every_seconds": 3600}),
            last_success_at: None,
            next_run_at: now(),
            attempts: 0,
            max_attempts: 5,
            last_error: None,
            lease_expires_at: None,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-08T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn a_pending_row_within_the_grace_is_healthy() {
        let mut r = row("pending");
        r.next_run_at = now() - Duration::seconds(JOB_LATE_AFTER_SECS - 1);
        assert_eq!(r.health(now()), JobHealth::Healthy);
        assert_eq!(
            r.lateness(now()),
            Some(Duration::seconds(JOB_LATE_AFTER_SECS - 1))
        );
        r.next_run_at = now() + Duration::seconds(60);
        assert_eq!(r.lateness(now()), None, "not due yet");
    }

    #[test]
    fn a_pending_row_past_the_grace_is_late_even_when_failing() {
        let mut r = row("pending");
        r.next_run_at = now() - Duration::seconds(JOB_LATE_AFTER_SECS + 1);
        r.last_error = Some("database error: other".into());
        assert_eq!(r.health(now()), JobHealth::Late);
    }

    #[test]
    fn a_failed_occurrence_on_schedule_is_failing() {
        let mut r = row("pending");
        r.next_run_at = now() + Duration::seconds(60);
        r.last_error = Some("job failed: x".into());
        assert_eq!(r.health(now()), JobHealth::Failing);
    }

    #[test]
    fn running_rows_are_judged_by_their_lease() {
        let mut r = row("running");
        r.lease_expires_at = Some(now() + Duration::seconds(30));
        assert_eq!(r.health(now()), JobHealth::Running);
        assert_eq!(r.lateness(now()), None, "a claimed row is not late");
        r.lease_expires_at = Some(now() - Duration::seconds(1));
        assert_eq!(r.health(now()), JobHealth::Stuck);
    }

    #[test]
    fn a_terminal_recurring_row_is_dead() {
        assert_eq!(row("failed").health(now()), JobHealth::Dead);
    }

    #[test]
    fn health_orders_worst_first() {
        assert!(JobHealth::Dead < JobHealth::Stuck);
        assert!(JobHealth::Late < JobHealth::Failing);
        assert!(JobHealth::Running < JobHealth::Healthy);
    }
}
