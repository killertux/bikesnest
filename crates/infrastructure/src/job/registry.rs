//! Job handler registry and the built-in handlers.

use crate::Db;
use crate::auth::{SqlxAuditLog, SystemClock};
use crate::config::Config;
use crate::job::email::SendEmailHandler;
use crate::job::repo::SqlxJobRepository;
use crate::privacy::SqlxRetentionRepository;
use async_trait::async_trait;
use bikesnest_application::{
    EmailProvider, JOB_JOBS_GC, JOB_RETENTION, JobError, JobHandler, JobPayload,
};
use std::collections::HashMap;
use std::sync::Arc;

pub const RETENTION_RECURRING_KEY: &str = "recurring:retention";
pub const JOBS_GC_RECURRING_KEY: &str = "recurring:jobs.gc";

/// An always-on recurring job the worker reconciles at startup.
pub struct RecurringKind {
    pub job_kind: &'static str,
    pub payload: JobPayload,
    pub schedule: JobPayload,
    pub idempotency_key: &'static str,
    pub max_attempts: i32,
}

/// Maps a job `kind` → its handler, plus the always-on recurring kinds.
pub struct JobRegistry {
    handlers: HashMap<String, Box<dyn JobHandler>>,
    recurring: Vec<RecurringKind>,
}

impl JobRegistry {
    pub fn new(handlers: Vec<Box<dyn JobHandler>>, recurring: Vec<RecurringKind>) -> Self {
        let map = handlers
            .into_iter()
            .map(|h| (h.kind().to_string(), h))
            .collect();
        Self {
            handlers: map,
            recurring,
        }
    }

    pub fn get(&self, kind: &str) -> Option<&dyn JobHandler> {
        self.handlers.get(kind).map(|b| b.as_ref())
    }

    pub fn recurring(&self) -> &[RecurringKind] {
        &self.recurring
    }
}

/// Everything the worker needs, plus the repo it shares with the GC handler.
pub struct JobServices {
    pub repo: SqlxJobRepository,
    pub registry: Arc<JobRegistry>,
}

/// Build the built-in job handlers from the real infrastructure. Wires
/// retention (backed by `RetentionJob`), `jobs.gc` and `email.send`.
///
/// `email` is the same provider instance the router holds, so a deployment
/// running with `JOBS_ENABLED=false` (where the inline `EmailQueue` sends on
/// the request path) and one running with the worker talk to one configured
/// relay/ESP, not two.
pub fn job_services(
    db: Db,
    config: &Config,
    storage: Arc<dyn bikesnest_application::ObjectStorage>,
    email: Arc<dyn EmailProvider>,
) -> JobServices {
    let repo = SqlxJobRepository::new(db.clone());
    let handlers: Vec<Box<dyn JobHandler>> = vec![
        Box::new(RetentionJobHandler::new(db.clone(), config, storage)),
        Box::new(JobsGcHandler::new(
            repo.clone(),
            config.jobs.history_retention_days,
        )),
        Box::new(SendEmailHandler::new(db.clone(), email)),
    ];
    let recurring = vec![
        RecurringKind {
            job_kind: JOB_RETENTION,
            payload: serde_json::json!({}),
            schedule: serde_json::json!({ "every_seconds": 86_400 }),
            idempotency_key: RETENTION_RECURRING_KEY,
            max_attempts: config.jobs.max_attempts,
        },
        RecurringKind {
            job_kind: JOB_JOBS_GC,
            payload: serde_json::json!({}),
            schedule: serde_json::json!({ "every_seconds": 86_400 }),
            idempotency_key: JOBS_GC_RECURRING_KEY,
            max_attempts: config.jobs.max_attempts,
        },
    ];
    JobServices {
        repo,
        registry: Arc::new(JobRegistry::new(handlers, recurring)),
    }
}

/// Runs the existing `RetentionJob` use case as a background job. Idempotent
/// (every purge is `DELETE WHERE expires_at < now()`), so at-least-once is safe.
pub struct RetentionJobHandler {
    job: bikesnest_application::RetentionJob,
}

impl RetentionJobHandler {
    pub fn new(
        db: Db,
        config: &Config,
        storage: Arc<dyn bikesnest_application::ObjectStorage>,
    ) -> Self {
        // `storage` is the same object store the request path writes uploads to:
        // the retention orphan sweep lists it under `uploads/` and deletes aged,
        // unreferenced keys, so the job and the uploads it collects cannot
        // disagree about where the objects live.
        let retention = SqlxRetentionRepository::new(db.clone(), config.retention, storage.clone());
        let job = bikesnest_application::RetentionJob::new(
            Box::new(retention),
            Box::new(SqlxAuditLog::new(db.clone())),
            Box::new(SystemClock),
            bikesnest_application::RetentionConfig {
                inactive_account_anonymize_after_days: config.inactive_account_anonymize_after_days,
                deleted_account_purge_after_days: config.deleted_account_purge_after_days,
            },
        );
        Self { job }
    }
}

#[async_trait]
impl JobHandler for RetentionJobHandler {
    fn kind(&self) -> &'static str {
        JOB_RETENTION
    }

    async fn run(&self, _payload: &JobPayload) -> Result<(), JobError> {
        self.job
            .run()
            .await
            .map(|_| ())
            .map_err(|e| JobError::Failed(format!("retention failed: {e}")))
    }
}

/// Deletes terminal job rows older than the configured retention window.
pub struct JobsGcHandler {
    repo: SqlxJobRepository,
    retention_days: u32,
}

impl JobsGcHandler {
    pub fn new(repo: SqlxJobRepository, retention_days: u32) -> Self {
        Self {
            repo,
            retention_days,
        }
    }
}

#[async_trait]
impl JobHandler for JobsGcHandler {
    fn kind(&self) -> &'static str {
        JOB_JOBS_GC
    }

    async fn run(&self, _payload: &JobPayload) -> Result<(), JobError> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(self.retention_days as i64);
        let deleted = self
            .repo
            .gc(cutoff)
            .await
            .map_err(|e| JobError::Failed(format!("jobs.gc failed: {e}")))?;
        tracing::debug!(deleted, "jobs.gc purged terminal rows");
        Ok(())
    }
}
