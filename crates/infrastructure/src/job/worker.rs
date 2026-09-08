//! In-process background job worker loop.

use crate::config::JobConfig;
use crate::job::registry::JobRegistry;
use crate::job::repo::{ClaimedJob, JobRepoError, SqlxJobRepository};
use crate::job::schedule::{backoff_ms, next_run_at};
use bikesnest_application::JobError;
use chrono::Utc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

/// Sleep for `poll`, or return early the moment shutdown is signalled.
async fn sleep_or_cancel(poll: std::time::Duration, shutdown: &CancellationToken) {
    tokio::select! {
        _ = tokio::time::sleep(poll) => {}
        _ = shutdown.cancelled() => {}
    }
}

/// Polls the job queue, claims due jobs, runs their handler, and records the
/// outcome (success / retry / dead-letter). Spawned on the tokio runtime at
/// startup when `JOBS_ENABLED`. One loop per instance; multiple instances are
/// safe because claims use `FOR UPDATE SKIP LOCKED`.
pub struct Worker {
    repo: SqlxJobRepository,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    id: String,
    bootstrap_attempts: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct WorkerDiagnostics {
    bootstrap_attempts: Arc<AtomicU64>,
}

impl WorkerDiagnostics {
    pub fn bootstrap_attempts(&self) -> u64 {
        self.bootstrap_attempts.load(Ordering::Relaxed)
    }
}

impl Worker {
    pub fn new(repo: SqlxJobRepository, registry: Arc<JobRegistry>, config: JobConfig) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = format!("worker-{}-{}-{}", std::process::id(), nanos, seq);
        Self {
            repo,
            registry,
            config,
            id,
            bootstrap_attempts: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Read-only worker diagnostics that remain observable while `run` owns the worker.
    pub fn diagnostics(&self) -> WorkerDiagnostics {
        WorkerDiagnostics {
            bootstrap_attempts: self.bootstrap_attempts.clone(),
        }
    }

    /// This worker's claim identity, as written to `background_job.claimed_by`.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Bootstrap recurring jobs, then poll→claim→process until `shutdown` is
    /// cancelled. Consumes `self` so the loop can be moved into a spawned
    /// tokio task.
    ///
    /// Cancellation is checked between polls *and* interrupts the idle sleep,
    /// so an idle worker returns within one poll interval at worst. A job
    /// already in flight is always run to completion and its outcome recorded —
    /// abandoning it would leave the row `running` until its lease expired.
    pub async fn run(self, shutdown: CancellationToken) {
        self.run_inner(shutdown, None).await;
    }

    /// Run a worker restricted to an explicit set of registered kinds.
    /// This shares the production loop and is useful for independently scoped
    /// worker deployments and isolated integration tests.
    pub async fn run_kinds(self, shutdown: CancellationToken, kinds: Vec<String>) {
        self.run_inner(shutdown, Some(kinds)).await;
    }

    async fn run_inner(self, shutdown: CancellationToken, claim_kinds: Option<Vec<String>>) {
        let poll = std::time::Duration::from_millis(self.config.poll_interval.as_millis() as u64);
        let bootstrap_retry = poll.max(std::time::Duration::from_millis(250));
        let mut bootstrapped = false;
        let mut next_bootstrap_attempt = tokio::time::Instant::now();
        while !shutdown.is_cancelled() {
            if !bootstrapped && tokio::time::Instant::now() >= next_bootstrap_attempt {
                self.bootstrap_attempts.fetch_add(1, Ordering::Relaxed);
                match self.bootstrap().await {
                    Ok(()) => bootstrapped = true,
                    Err(e) => {
                        tracing::error!(error = %e, "recurring job bootstrap failed; will retry");
                        next_bootstrap_attempt = tokio::time::Instant::now() + bootstrap_retry;
                    }
                }
            }
            let claim = if let Some(kinds) = claim_kinds.as_ref() {
                let refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
                self.repo
                    .claim_kinds(
                        self.config.batch_size,
                        &self.id,
                        self.config.lease_ttl,
                        &refs,
                    )
                    .await
            } else {
                self.repo
                    .claim(self.config.batch_size, &self.id, self.config.lease_ttl)
                    .await
            };
            match claim {
                Ok(jobs) if jobs.is_empty() => sleep_or_cancel(poll, &shutdown).await,
                Ok(jobs) => {
                    for job in jobs {
                        // Finish the batch we already claimed: these rows are
                        // marked `running` and would otherwise wait out a lease.
                        self.process_claimed(job).await;
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "job claim failed; backing off");
                    sleep_or_cancel(poll, &shutdown).await;
                }
            }
        }
        tracing::info!(worker = %self.id, "background worker stopped");
    }

    /// Reconcile the always-on recurring rows through the production startup path.
    /// A caller may continue claiming independent work after an error; `run`
    /// retries on later poll iterations without starving one-shot jobs.
    pub async fn bootstrap(&self) -> Result<(), JobRepoError> {
        let now = Utc::now();
        let mut incomplete = Vec::new();
        for rk in self.registry.recurring() {
            match self
                .repo
                .register_recurring(
                    rk.job_kind,
                    &rk.payload,
                    &rk.schedule,
                    now,
                    rk.max_attempts,
                    rk.idempotency_key,
                )
                .await
            {
                Ok(outcome) => tracing::info!(
                    kind = rk.job_kind,
                    key = rk.idempotency_key,
                    ?outcome,
                    "recurring job reconciled"
                ),
                Err(error) => {
                    tracing::error!(
                        kind = rk.job_kind,
                        key = rk.idempotency_key,
                        %error,
                        "recurring job reconciliation incomplete"
                    );
                    incomplete.push(rk.idempotency_key.to_string());
                }
            }
        }
        if !incomplete.is_empty() {
            return Err(JobRepoError::BootstrapIncomplete(incomplete));
        }
        Ok(())
    }

    /// Claim → run → finish, wrapped in a `background_job` tracing span.
    /// Execute and persist the outcome of one repository claim.
    pub async fn process_claimed(&self, job: ClaimedJob) {
        let span = tracing::info_span!(
            "background_job",
            kind = %job.kind,
            id = job.id,
            attempt = job.attempts
        );
        self.run_job(job).instrument(span).await;
    }

    async fn run_job(&self, job: ClaimedJob) {
        let Some(handler) = self.registry.get(&job.kind) else {
            tracing::warn!(kind = job.kind, "no handler for job kind; dead-lettering");
            let _ = self
                .repo
                .fail(
                    job.id,
                    &self.id,
                    &format!("no handler registered for kind '{}'", job.kind),
                )
                .await;
            return;
        };

        let heartbeat = self.spawn_heartbeat(job.id);
        let result = handler.run(&job.payload).await;
        heartbeat.abort();

        let now = Utc::now();
        match result {
            Ok(()) => {
                match next_run_at(job.schedule.as_ref(), now) {
                    Ok(next) => {
                        let _ = self.repo.finish_success(job.id, &self.id, next, now).await;
                        tracing::info!("job succeeded (recurring={})", next.is_some());
                    }
                    // Invalid schedule → permanent (dead-letter) rather than retry.
                    Err(e) => {
                        let _ = self.repo.fail(job.id, &self.id, &e.to_string()).await;
                        tracing::warn!(error = %e, "invalid schedule; dead-lettering");
                    }
                }
            }
            Err(JobError::Failed(e)) => {
                if job.attempts < job.max_attempts {
                    let delay_ms = backoff_ms(job.attempts, self.config.backoff_base_ms);
                    let run_at = now + chrono::Duration::milliseconds(delay_ms as i64);
                    let _ = self.repo.retry(job.id, &self.id, &e, run_at).await;
                    tracing::info!(
                        attempt = job.attempts,
                        backoff_ms = delay_ms,
                        error = %e,
                        "job failed; will retry"
                    );
                } else {
                    // Give the handler its say before the row goes terminal:
                    // only it can decode the payload (e.g. which email, to
                    // which domain) into something operators can act on.
                    handler.on_dead_letter(&job.payload, &e).await;
                    let _ = self.repo.fail(job.id, &self.id, &e).await;
                    tracing::warn!(error = %e, "job exhausted attempts; dead-lettered");
                }
            }
            Err(JobError::Permanent(e)) => {
                handler.on_dead_letter(&job.payload, &e).await;
                let _ = self.repo.fail(job.id, &self.id, &e).await;
                tracing::warn!(error = %e, "job failed permanently; dead-lettered");
            }
        }
    }

    /// Extend a long-running job's lease every `lease_ttl / 3` so it is not
    /// re-claimed mid-run. The heartbeat updates only `state = 'running'` rows,
    /// so it becomes a no-op after the job finishes (and it is aborted anyway).
    fn spawn_heartbeat(&self, id: i64) -> tokio::task::JoinHandle<()> {
        let repo = self.repo.clone();
        let worker_id = self.id.clone();
        let ttl = self.config.lease_ttl;
        tokio::spawn(async move {
            let interval = std::time::Duration::from_millis((ttl.as_millis() as u64 / 3).max(500));
            loop {
                tokio::time::sleep(interval).await;
                if repo.heartbeat(id, &worker_id, ttl).await.is_err() {
                    break;
                }
            }
        })
    }
}
