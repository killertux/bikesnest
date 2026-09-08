//! PostgreSQL-backed background job queue.
//!
//! A durable one-shot + recurring job table claimed by in-process workers.
//! All timestamps are `TIMESTAMPTZ` (UTC); there is no per-job timezone.

pub mod email;
pub mod registry;
pub mod repo;
pub mod schedule;
pub mod worker;

pub use email::SendEmailHandler;
pub use registry::{
    JOBS_GC_RECURRING_KEY, JobRegistry, JobServices, JobsGcHandler, RETENTION_RECURRING_KEY,
    RecurringKind, RetentionJobHandler, job_services,
};
pub use repo::{ClaimedJob, JobRepoError, RecurringRegistrationOutcome, SqlxJobRepository};
pub use schedule::{backoff_ms, next_run_at};
pub use worker::{Worker, WorkerDiagnostics};
