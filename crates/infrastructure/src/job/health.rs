//! SQL-backed [`JobHealthReader`]: the job table's bookkeeping columns, read
//! for the admin health view. Never selects `payload` (an `email.send` row
//! carries an address there).

use async_trait::async_trait;
use bikesnest_application::{
    JOB_LATE_AFTER_SECS, JobHealthReader, JobQueueSummary, ReaderError, RecurringJobStatus,
};
use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::Db;
use crate::parking::search::reader_err;

/// Recurring rows are a handful (one per registered recurring kind); the cap
/// only guards against a runaway registration.
const RECURRING_LIMIT: i64 = 100;

pub struct SqlxJobHealthReader {
    db: Db,
}

impl SqlxJobHealthReader {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

type RecurringRow = (
    i64,
    String,
    String,
    Value,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
    i32,
    i32,
    Option<String>,
    Option<DateTime<Utc>>,
);

#[async_trait]
impl JobHealthReader for SqlxJobHealthReader {
    async fn recurring(&self) -> Result<Vec<RecurringJobStatus>, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("job_health.recurring", e))?;
        let rows: Vec<RecurringRow> = sqlx::query_as(
            r#"SELECT id, kind, state, schedule, finished_at, run_at, attempts, max_attempts,
                      last_error, lease_expires_at
               FROM background_job
               WHERE schedule IS NOT NULL
               ORDER BY kind, id
               LIMIT $1"#,
        )
        .bind(RECURRING_LIMIT)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| reader_err("job_health.recurring", e))?;
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    kind,
                    state,
                    schedule,
                    last_success_at,
                    next_run_at,
                    attempts,
                    max_attempts,
                    last_error,
                    lease_expires_at,
                )| RecurringJobStatus {
                    id,
                    kind,
                    state,
                    schedule,
                    last_success_at,
                    next_run_at,
                    attempts,
                    max_attempts,
                    last_error,
                    lease_expires_at,
                },
            )
            .collect())
    }

    async fn queue_summary(&self, now: DateTime<Utc>) -> Result<JobQueueSummary, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("job_health.queue_summary", e))?;
        // Each count rides one of the queue's partial indexes: pending rows
        // by `run_at` (background_job_due), terminal rows by `finished_at`
        // (background_job_terminal).
        let (overdue_pending, failed_last_day): (i64, i64) = sqlx::query_as(
            r#"SELECT
                 (SELECT count(*) FROM background_job
                   WHERE state = 'pending' AND schedule IS NULL
                     AND run_at < $1 - ($2 * interval '1 second')),
                 (SELECT count(*) FROM background_job
                   WHERE state = 'failed'
                     AND schedule IS NULL AND finished_at > $1 - interval '24 hours')"#,
        )
        .bind(now)
        .bind(JOB_LATE_AFTER_SECS as f64)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| reader_err("job_health.queue_summary", e))?;
        Ok(JobQueueSummary {
            overdue_pending,
            failed_last_day,
        })
    }
}
