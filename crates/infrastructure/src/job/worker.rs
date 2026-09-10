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

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Sleep for `poll`, or return early the moment shutdown is signalled.
async fn sleep_or_cancel(poll: std::time::Duration, shutdown: &CancellationToken) {
    tokio::select! {
        _ = tokio::time::sleep(poll) => {}
        _ = shutdown.cancelled() => {}
    }
}

/// Polls the job queue, claims due jobs, runs their handler, and records the
/// outcome (success / retry / dead-letter). Spawned on the tokio runtime at
/// startup when worker execution is enabled. One loop per instance; multiple instances are
/// safe because claims use `FOR UPDATE SKIP LOCKED`.
#[derive(Clone)]
pub struct Worker {
    repo: SqlxJobRepository,
    registry: Arc<JobRegistry>,
    config: JobConfig,
    id: String,
    bootstrap_attempts: Arc<AtomicU64>,
    claim_sequence: Arc<AtomicU64>,
    observable_failures: Arc<AtomicU64>,
}

#[derive(Clone)]
pub struct WorkerDiagnostics {
    bootstrap_attempts: Arc<AtomicU64>,
    observable_failures: Arc<AtomicU64>,
}

impl WorkerDiagnostics {
    pub fn bootstrap_attempts(&self) -> u64 {
        self.bootstrap_attempts.load(Ordering::Relaxed)
    }

    pub fn observable_failures(&self) -> u64 {
        self.observable_failures.load(Ordering::Relaxed)
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
            claim_sequence: Arc::new(AtomicU64::new(0)),
            observable_failures: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Read-only worker diagnostics that remain observable while `run` owns the worker.
    pub fn diagnostics(&self) -> WorkerDiagnostics {
        WorkerDiagnostics {
            bootstrap_attempts: self.bootstrap_attempts.clone(),
            observable_failures: self.observable_failures.clone(),
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
    /// so an idle worker returns promptly. Active attempts receive a bounded
    /// natural drain, then their owned handler and heartbeat tasks are cancelled
    /// and joined; the durable lease makes interrupted work recoverable.
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
        let mut active = tokio::task::JoinSet::new();
        let force_shutdown = CancellationToken::new();
        while !shutdown.is_cancelled() {
            if !bootstrapped && tokio::time::Instant::now() >= next_bootstrap_attempt {
                self.bootstrap_attempts.fetch_add(1, Ordering::Relaxed);
                let bootstrap = tokio::select! {
                    result = self.bootstrap() => result,
                    _ = shutdown.cancelled() => break,
                };
                match bootstrap {
                    Ok(()) => bootstrapped = true,
                    Err(e) => {
                        tracing::error!(error = %e, "recurring job bootstrap failed; will retry");
                        next_bootstrap_attempt = tokio::time::Instant::now() + bootstrap_retry;
                    }
                }
            }
            while active.len() < self.config.batch_size && !shutdown.is_cancelled() {
                let claim_owner = format!(
                    "{}-{}",
                    self.id,
                    self.claim_sequence.fetch_add(1, Ordering::Relaxed)
                );
                let claim_future = async {
                    if let Some(kinds) = claim_kinds.as_ref() {
                        let refs: Vec<&str> = kinds.iter().map(String::as_str).collect();
                        self.repo
                            .claim_kinds(1, &claim_owner, self.config.lease_ttl, &refs)
                            .await
                    } else {
                        self.repo
                            .claim(1, &claim_owner, self.config.lease_ttl)
                            .await
                    }
                };
                let claim = tokio::select! {
                    result = claim_future => result,
                    _ = shutdown.cancelled() => break,
                };
                match claim {
                    Ok(jobs) if jobs.is_empty() => break,
                    Ok(jobs) => {
                        for job in jobs {
                            let worker = self.clone();
                            let force = force_shutdown.child_token();
                            active.spawn(async move {
                                worker.process_claimed_with_shutdown(job, force).await
                            });
                        }
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "job claim failed; backing off");
                        break;
                    }
                }
            }
            if active.is_empty() {
                sleep_or_cancel(poll, &shutdown).await;
            } else {
                tokio::select! {
                    _ = shutdown.cancelled() => {},
                    _ = tokio::time::sleep(poll) => {},
                    completed = active.join_next() => {
                        if completed.is_some_and(|result| result.is_err()) {
                            self.note_failure("execution_task_panicked", None, None);
                        }
                    }
                }
            }
        }
        let drain = async { while active.join_next().await.is_some() {} };
        if tokio::time::timeout(self.config.shutdown_grace, drain)
            .await
            .is_err()
        {
            self.note_failure("shutdown_grace_exceeded", None, None);
            force_shutdown.cancel();
            let forced_drain = async { while active.join_next().await.is_some() {} };
            if tokio::time::timeout(self.config.shutdown_grace, forced_drain)
                .await
                .is_err()
            {
                active.abort_all();
                while active.join_next().await.is_some() {}
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
        self.process_claimed_with_shutdown(job, CancellationToken::new())
            .await;
    }

    async fn process_claimed_with_shutdown(&self, job: ClaimedJob, shutdown: CancellationToken) {
        let span = tracing::info_span!(
            "background_job",
            kind = %job.kind,
            id = job.id,
            attempt = job.attempts
        );
        self.run_job(job, shutdown).instrument(span).await;
    }

    fn note_failure(&self, classification: &'static str, job_id: Option<i64>, kind: Option<&str>) {
        self.observable_failures.fetch_add(1, Ordering::Relaxed);
        tracing::error!(classification, job_id, kind, "background worker failure");
    }

    async fn run_job(&self, job: ClaimedJob, shutdown: CancellationToken) {
        let Some(handler) = self.registry.get(&job.kind) else {
            tracing::warn!(kind = job.kind, "no handler for job kind; dead-lettering");
            if self
                .repo
                .fail(
                    job.id,
                    &job.owner,
                    &format!("no handler registered for kind '{}'", job.kind),
                )
                .await
                .is_err()
            {
                self.note_failure("outcome_write_failed", Some(job.id), Some(&job.kind));
            }
            return;
        };

        let execution_handler = handler.clone();
        let payload = job.payload.clone();
        let mut execution = tokio::spawn(async move { execution_handler.run(&payload).await });
        let _execution_guard = AbortOnDrop(execution.abort_handle());
        let (heartbeat_tx, mut heartbeat_rx) = tokio::sync::oneshot::channel();
        let heartbeat = self.spawn_heartbeat(job.id, job.owner.clone(), heartbeat_tx);
        let _heartbeat_guard = AbortOnDrop(heartbeat.abort_handle());
        let result = tokio::select! {
            joined = &mut execution => match joined {
                Ok(result) => result,
                Err(_) => Err(JobError::Failed("handler task panicked".into())),
            },
            _ = tokio::time::sleep(self.config.handler_timeout) => {
                execution.abort();
                let _ = execution.await;
                Err(JobError::Failed("handler timed out".into()))
            },
            heartbeat_error = &mut heartbeat_rx => {
                execution.abort();
                let _ = execution.await;
                self.note_failure("heartbeat_failed", Some(job.id), Some(&job.kind));
                let _ = heartbeat_error;
                return;
            },
            _ = shutdown.cancelled() => {
                execution.abort();
                let _ = execution.await;
                heartbeat.abort();
                let _ = heartbeat.await;
                return;
            }
        };
        let now = Utc::now();
        match result {
            Ok(()) => {
                match next_run_at(job.schedule.as_ref(), now) {
                    Ok(next) => {
                        if self
                            .repo
                            .finish_success(job.id, &job.owner, next, now)
                            .await
                            .is_err()
                        {
                            self.note_failure(
                                "outcome_write_failed",
                                Some(job.id),
                                Some(&job.kind),
                            );
                        } else {
                            tracing::info!("job succeeded (recurring={})", next.is_some());
                        }
                    }
                    // Invalid schedule → permanent (dead-letter) rather than retry.
                    Err(_) => {
                        let persisted = self
                            .repo
                            .fail(job.id, &job.owner, "invalid schedule")
                            .await
                            .is_ok();
                        if !persisted {
                            self.note_failure(
                                "outcome_write_failed",
                                Some(job.id),
                                Some(&job.kind),
                            );
                        } else {
                            tracing::warn!("invalid schedule dead-letter persisted");
                        }
                    }
                }
            }
            Err(JobError::Failed(e)) => {
                if job.attempts < job.max_attempts {
                    let delay_ms = backoff_ms(job.attempts, self.config.backoff_base_ms);
                    let run_at = now + chrono::Duration::milliseconds(delay_ms as i64);
                    let persisted = self
                        .repo
                        .retry(job.id, &job.owner, &e, run_at)
                        .await
                        .is_ok();
                    if !persisted {
                        self.note_failure("outcome_write_failed", Some(job.id), Some(&job.kind));
                    } else {
                        tracing::info!(
                            attempt = job.attempts,
                            backoff_ms = delay_ms,
                            "job failed; retry persisted"
                        );
                    }
                } else {
                    // Give the handler its say before the row goes terminal:
                    // only it can decode the payload (e.g. which email, to
                    // which domain) into something operators can act on.
                    self.run_dead_letter_hook(
                        handler.clone(),
                        job.payload.clone(),
                        e.clone(),
                        job.id,
                        &job.kind,
                    )
                    .await;
                    if self.repo.fail(job.id, &job.owner, &e).await.is_err() {
                        self.note_failure("outcome_write_failed", Some(job.id), Some(&job.kind));
                    } else {
                        tracing::warn!("job exhausted attempts; dead-letter persisted");
                    }
                }
            }
            Err(JobError::Permanent(e)) => {
                self.run_dead_letter_hook(
                    handler,
                    job.payload.clone(),
                    e.clone(),
                    job.id,
                    &job.kind,
                )
                .await;
                if self.repo.fail(job.id, &job.owner, &e).await.is_err() {
                    self.note_failure("outcome_write_failed", Some(job.id), Some(&job.kind));
                } else {
                    tracing::warn!("permanent failure dead-letter persisted");
                }
            }
        }
        heartbeat.abort();
        let _ = heartbeat.await;
    }

    async fn run_dead_letter_hook(
        &self,
        handler: Arc<dyn bikesnest_application::JobHandler>,
        payload: bikesnest_application::JobPayload,
        error: String,
        job_id: i64,
        kind: &str,
    ) {
        let mut task = tokio::spawn(async move { handler.on_dead_letter(&payload, &error).await });
        let _guard = AbortOnDrop(task.abort_handle());
        match tokio::time::timeout(self.config.handler_timeout, &mut task).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => self.note_failure("dead_letter_hook_panicked", Some(job_id), Some(kind)),
            Err(_) => {
                task.abort();
                let _ = task.await;
                self.note_failure("dead_letter_hook_timed_out", Some(job_id), Some(kind));
            }
        }
    }

    /// Extend a long-running job's lease every `lease_ttl / 3` so it is not
    /// re-claimed during handler and outcome finalization. Lost ownership or a
    /// database error terminates the heartbeat and cancels handler execution.
    fn spawn_heartbeat(
        &self,
        id: i64,
        worker_id: String,
        failed: tokio::sync::oneshot::Sender<()>,
    ) -> tokio::task::JoinHandle<()> {
        let repo = self.repo.clone();
        let ttl = self.config.lease_ttl;
        tokio::spawn(async move {
            let interval = std::time::Duration::from_millis((ttl.as_millis() as u64 / 3).max(1));
            loop {
                tokio::time::sleep(interval).await;
                if repo.heartbeat(id, &worker_id, ttl).await.is_err() {
                    let _ = failed.send(());
                    break;
                }
            }
        })
    }
}
