//! Integration tests for the PostgreSQL background job repository
//! PostgreSQL-backed background-job integration tests.
//!
//! The repo operates on the shared pool, so each test seeds rows with a unique
//! `kind` prefix and cleans them up at the end (rows are not rolled back — they
//! are on the pool, not the test transaction). Where a test needs a "claimed"
//! row it simulates it with a direct `UPDATE` so it does not race other tests'
//! `claim` calls; the one real `claim` test asserts only the `SKIP LOCKED`
//! disjointness property, which holds regardless of concurrent claims.

use bikesnest_infrastructure::{Db, JobConfig, JobRegistry, SqlxJobRepository, Worker};
use bikesnest_test_support::{db_test, pool, run_isolated_database_test};
use chrono::{Duration, Utc};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use tracing_subscriber::{Layer, layer::SubscriberExt};

const CAPACITY_KIND: &str = "jobtest.worker-capacity";
const PANIC_KIND: &str = "jobtest.worker-panic";
const TIMEOUT_KIND: &str = "jobtest.worker-timeout";
const OUTCOME_KIND: &str = "jobtest.worker-outcome";
const HEARTBEAT_KIND: &str = "jobtest.worker-heartbeat";
const SHUTDOWN_KIND: &str = "jobtest.worker-shutdown";
const RETRY_OUTCOME_KIND: &str = "jobtest.worker-retry-outcome";
const DEAD_OUTCOME_KIND: &str = "jobtest.worker-dead-outcome";
const HOOK_PANIC_KIND: &str = "jobtest.worker-hook-panic";
const HOOK_TIMEOUT_KIND: &str = "jobtest.worker-hook-timeout";

struct BlockingHandler {
    kind: &'static str,
    started: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    active: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    peak: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    gate: std::sync::Arc<tokio::sync::Semaphore>,
}

struct ActiveGuard(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl bikesnest_application::JobHandler for BlockingHandler {
    fn kind(&self) -> &'static str {
        self.kind
    }
    async fn run(&self, _: &serde_json::Value) -> Result<(), bikesnest_application::JobError> {
        self.started
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let active = self
            .active
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let _active_guard = ActiveGuard(self.active.clone());
        self.peak
            .fetch_max(active, std::sync::atomic::Ordering::SeqCst);
        let permit = self.gate.acquire().await.unwrap();
        permit.forget();
        Ok(())
    }
}

struct PanicHandler;
#[async_trait::async_trait]
impl bikesnest_application::JobHandler for PanicHandler {
    fn kind(&self) -> &'static str {
        PANIC_KIND
    }
    async fn run(&self, _: &serde_json::Value) -> Result<(), bikesnest_application::JobError> {
        panic!("test panic payload is emitted by Rust's standard panic hook")
    }
}

struct SuccessHandler;
#[async_trait::async_trait]
impl bikesnest_application::JobHandler for SuccessHandler {
    fn kind(&self) -> &'static str {
        OUTCOME_KIND
    }
    async fn run(&self, _: &serde_json::Value) -> Result<(), bikesnest_application::JobError> {
        Ok(())
    }
}

struct FailureHandler {
    kind: &'static str,
    permanent: bool,
}
#[async_trait::async_trait]
impl bikesnest_application::JobHandler for FailureHandler {
    fn kind(&self) -> &'static str {
        self.kind
    }
    async fn run(&self, _: &serde_json::Value) -> Result<(), bikesnest_application::JobError> {
        if self.permanent {
            Err(bikesnest_application::JobError::Permanent(
                "bounded failure".into(),
            ))
        } else {
            Err(bikesnest_application::JobError::Failed(
                "bounded failure".into(),
            ))
        }
    }
}

enum HookMode {
    Panic,
    Timeout,
}
struct HookHandler {
    kind: &'static str,
    mode: HookMode,
}
#[async_trait::async_trait]
impl bikesnest_application::JobHandler for HookHandler {
    fn kind(&self) -> &'static str {
        self.kind
    }
    async fn run(&self, _: &serde_json::Value) -> Result<(), bikesnest_application::JobError> {
        Err(bikesnest_application::JobError::Permanent(
            "bounded failure".into(),
        ))
    }
    async fn on_dead_letter(&self, _: &serde_json::Value, _: &str) {
        match self.mode {
            HookMode::Panic => panic!("test hook panic"),
            HookMode::Timeout => std::future::pending().await,
        }
    }
}

#[derive(Clone)]
struct CaptureLayer(std::sync::Arc<std::sync::Mutex<Vec<String>>>);
struct FieldVisitor(String);
impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(field.name());
        self.0.push_str(&format!("={value:?};"));
    }
}
impl<S: tracing::Subscriber> Layer<S> for CaptureLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = FieldVisitor(String::new());
        event.record(&mut visitor);
        self.0.lock().unwrap().push(visitor.0);
    }
}

async fn db() -> Db {
    Db::from_pool(pool().await)
}

async fn repo() -> SqlxJobRepository {
    SqlxJobRepository::new(db().await)
}

/// Delete every background_job row whose kind starts with `jobtest.` (cleanup for
/// whatever this test created; other tests use their own distinct kinds).
async fn clear_kind(kind_prefix: &str) {
    sqlx::query("DELETE FROM background_job WHERE kind LIKE $1")
        .bind(format!("{kind_prefix}%"))
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn enqueue_is_idempotent_on_key(_tx: &mut bikesnest_test_support::TestTx) {
    let r = repo().await;
    let now = Utc::now();
    // First insert with a stable key → Ok(Some(id)).
    let first = r
        .enqueue(
            "jobtest.idem",
            &json!({"n": 1}),
            now,
            Some(3),
            Some("recurring:jobtest.idem"),
        )
        .await
        .unwrap();
    assert!(first.is_some());
    // Second insert with the same key is a no-op → Ok(None).
    let second = r
        .enqueue(
            "jobtest.idem",
            &json!({"n": 2}),
            now,
            Some(3),
            Some("recurring:jobtest.idem"),
        )
        .await
        .unwrap();
    assert!(second.is_none(), "idempotency_key must dedup enqueue");
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM background_job WHERE kind = 'jobtest.idem'")
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(n, 1);
    clear_kind("jobtest.idem").await;
}

#[db_test]
async fn finish_success_completes_oneshot(_tx: &mut bikesnest_test_support::TestTx) {
    let r = repo().await;
    let now = Utc::now();
    let id = r
        .enqueue("jobtest.oneshot", &json!({}), now, Some(5), None)
        .await
        .unwrap()
        .unwrap();
    // Simulate a claim (state running, attempt 1).
    let claimed = sqlx::query(
        "UPDATE background_job SET state='running', claimed_by='w', lease_expires_at=now()+interval '60 seconds', attempts=1 WHERE id=$1",
    )
    .bind(id)
    .execute(&pool().await)
    .await
    .unwrap();
    assert_eq!(claimed.rows_affected(), 1);

    r.finish_success(id, "w", None, now).await.unwrap();

    let (state, finished): (String, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("SELECT state, finished_at FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "succeeded");
    assert!(finished.is_some());
    clear_kind("jobtest.oneshot").await;
}

#[db_test]
async fn finish_success_reschedules_recurring(_tx: &mut bikesnest_test_support::TestTx) {
    let r = repo().await;
    let now = Utc::now();
    let id = r
        .enqueue("jobtest.recurr", &json!({}), now, Some(5), None)
        .await
        .unwrap()
        .unwrap();
    // Claim + mark the row as recurring.
    sqlx::query(
        "UPDATE background_job SET state='running', claimed_by='w', lease_expires_at=clock_timestamp()+interval '60 seconds', schedule='{\"every_seconds\": 60}'::jsonb, attempts=1 WHERE id=$1",
    )
    .bind(id)
    .execute(&pool().await)
    .await
    .unwrap();

    let next = now + Duration::seconds(60);
    r.finish_success(id, "w", Some(next), now).await.unwrap();

    let (state, attempts, run_at): (String, i32, chrono::DateTime<Utc>) =
        sqlx::query_as("SELECT state, attempts, run_at FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "pending");
    assert_eq!(attempts, 0, "recurring success resets the attempt budget");
    assert!(run_at > now, "next run is in the future");
    clear_kind("jobtest.recurr").await;
}

#[db_test]
async fn retry_then_dead_letter(_tx: &mut bikesnest_test_support::TestTx) {
    let r = repo().await;
    let now = Utc::now();
    let id = r
        .enqueue("jobtest.retry", &json!({}), now, Some(2), None)
        .await
        .unwrap()
        .unwrap();
    sqlx::query(
        "UPDATE background_job SET state='running', claimed_by='w', lease_expires_at=clock_timestamp()+interval '60 seconds', attempts=1 WHERE id=$1",
    )
    .bind(id)
    .execute(&pool().await)
    .await
    .unwrap();

    // Attempt 1 < max(2) → retry (state pending, future run_at, last_error set).
    let run_at = now + Duration::seconds(60);
    r.retry(id, "w", "boom", run_at).await.unwrap();
    let (state, last_error): (String, Option<String>) =
        sqlx::query_as("SELECT state, last_error FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "pending");
    assert_eq!(last_error.as_deref(), Some("boom"));

    // Attempt 2 == max(2) → dead-letter (state failed, finished_at set).
    sqlx::query(
        "UPDATE background_job SET state='running', claimed_by='w', lease_expires_at=clock_timestamp()+interval '60 seconds', attempts=2 WHERE id=$1",
    )
    .bind(id)
    .execute(&pool().await)
    .await
    .unwrap();
    r.fail(id, "w", "boom-again").await.unwrap();
    let (state, finished): (String, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("SELECT state, finished_at FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "failed");
    assert!(finished.is_some());
    clear_kind("jobtest.retry").await;
}

#[db_test]
async fn gc_deletes_only_old_terminal_rows(_tx: &mut bikesnest_test_support::TestTx) {
    let r = repo().await;
    let now = Utc::now();
    let cut_off = now - Duration::days(7);
    let id_old = r
        .enqueue("jobtest.gc_old", &json!({}), now, Some(5), None)
        .await
        .unwrap()
        .unwrap();
    let id_fresh = r
        .enqueue("jobtest.gc_fresh", &json!({}), now, Some(5), None)
        .await
        .unwrap()
        .unwrap();
    // terminal + old → should be deleted; terminal + fresh → kept; pending → kept.
    sqlx::query("UPDATE background_job SET state='succeeded', finished_at=$2 WHERE id=$1")
        .bind(id_old)
        .bind(now - Duration::days(10))
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE background_job SET state='failed', finished_at=now(), last_error='x' WHERE id=$1",
    )
    .bind(id_fresh)
    .execute(&pool().await)
    .await
    .unwrap();
    let id_pending = r
        .enqueue("jobtest.gc_pending", &json!({}), now, Some(5), None)
        .await
        .unwrap()
        .unwrap();

    let deleted = r.gc(cut_off).await.unwrap();
    assert!(deleted >= 1, "returns the rows it removed");

    let gone: i64 = sqlx::query_scalar("SELECT count(*) FROM background_job WHERE id=$1")
        .bind(id_old)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(gone, 0, "old terminal row is deleted");
    let fresh: i64 = sqlx::query_scalar("SELECT count(*) FROM background_job WHERE id=$1")
        .bind(id_fresh)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(fresh, 1, "fresh terminal row is kept");
    let pending: i64 = sqlx::query_scalar("SELECT count(*) FROM background_job WHERE id=$1")
        .bind(id_pending)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(pending, 1, "pending row is never deleted");
    clear_kind("jobtest.gc").await;
}

#[db_test]
async fn concurrent_claims_are_disjoint(_tx: &mut bikesnest_test_support::TestTx) {
    let r = repo().await;
    let now = Utc::now();
    // A kind unique to this test run: `claim_kinds` scopes both workers to it,
    // so no concurrently-running test (in this binary or another) can crowd
    // our rows out, or be crowded out by our large batch. That is what lets
    // the assertions below be exact instead of "claimed by *someone*".
    let kind = format!(
        "test.{}.concurrent_claims_are_disjoint.{}",
        module_path!(),
        std::process::id()
    );
    // Seed a handful of due, unique-kind rows.
    let mut ids = Vec::new();
    for i in 0..6 {
        let id = r
            .enqueue(&kind, &json!({"i": i}), now, Some(5), None)
            .await
            .unwrap()
            .unwrap();
        ids.push(id);
    }

    // Two workers claim concurrently, each with a large batch so the whole due
    // set is covered — safe now that `claim_kinds` confines both to our kind.
    let repo_a = r.clone();
    let repo_b = r.clone();
    let (kind_a, kind_b) = (kind.clone(), kind.clone());
    let a = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let b = std::sync::Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let (a2, b2) = (a.clone(), b.clone());
    let h1 = tokio::spawn(async move {
        let got = repo_a
            .claim_kinds(
                1000,
                "worker-a",
                std::time::Duration::from_secs(60),
                &[&kind_a],
            )
            .await
            .unwrap();
        *a2.lock().await = got;
    });
    let h2 = tokio::spawn(async move {
        let got = repo_b
            .claim_kinds(
                1000,
                "worker-b",
                std::time::Duration::from_secs(60),
                &[&kind_b],
            )
            .await
            .unwrap();
        *b2.lock().await = got;
    });
    let _ = (h1.await.unwrap(), h2.await.unwrap());
    let claim_a = std::mem::take(&mut *a.lock().await);
    let claim_b = std::mem::take(&mut *b.lock().await);

    // SKIP LOCKED → the two claims never share an id.
    let ids_a: std::collections::HashSet<i64> = claim_a.iter().map(|j| j.id).collect();
    let ids_b: std::collections::HashSet<i64> = claim_b.iter().map(|j| j.id).collect();
    assert!(
        ids_a.is_disjoint(&ids_b),
        "two workers must never claim the same job ({ids_a:?} vs {ids_b:?})"
    );

    // Kind-scoped claims mean nothing else could have touched these rows:
    // every one of them must be claimed by exactly worker-a or worker-b, no
    // more, no less.
    let all_ids: std::collections::HashSet<i64> = ids.iter().copied().collect();
    let claimed: std::collections::HashSet<i64> = ids_a.union(&ids_b).copied().collect();
    assert_eq!(
        claimed, all_ids,
        "every seeded row must be claimed by exactly worker-a or worker-b"
    );

    for id in &ids {
        let (state, attempts, claimed_by): (String, i32, Option<String>) =
            sqlx::query_as("SELECT state, attempts, claimed_by FROM background_job WHERE id=$1")
                .bind(id)
                .fetch_one(&pool().await)
                .await
                .unwrap();
        assert_eq!(state, "running");
        assert_eq!(attempts, 1);
        assert!(
            matches!(claimed_by.as_deref(), Some("worker-a") | Some("worker-b")),
            "claimed row must be held by one of this test's own workers, got {claimed_by:?}"
        );
    }
    clear_kind(&kind).await;
}

#[db_test]
async fn claim_reclaims_a_crashed_workers_running_job(_tx: &mut bikesnest_test_support::TestTx) {
    let r = repo().await;
    let now = Utc::now();
    let kind = format!(
        "test.{}.claim_reclaims_a_crashed_workers_running_job.{}",
        module_path!(),
        std::process::id()
    );
    let id = r
        .enqueue(&kind, &json!({}), now, Some(5), None)
        .await
        .unwrap()
        .unwrap();
    // Simulate a worker that claimed the job and then crashed: state left
    // 'running' with a lease that already expired.
    sqlx::query(
        "UPDATE background_job SET state='running', claimed_by='dead-worker',
            lease_expires_at=now() - interval '1 second', attempts=1 WHERE id=$1",
    )
    .bind(id)
    .execute(&pool().await)
    .await
    .unwrap();

    // `claim_kinds` scopes this call to our own unique kind, so — unlike the
    // unscoped `claim` — nothing else in the suite can compete for this row.
    let claimed = r
        .claim_kinds(10, "worker-b", std::time::Duration::from_secs(60), &[&kind])
        .await
        .unwrap();
    let we_claimed_it = claimed.iter().any(|j| j.id == id);

    let (state, claimed_by, attempts, lease_expires_at): (
        String,
        Option<String>,
        i32,
        Option<chrono::DateTime<Utc>>,
    ) = sqlx::query_as(
        "SELECT state, claimed_by, attempts, lease_expires_at FROM background_job WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(state, "running");
    assert_ne!(
        claimed_by.as_deref(),
        Some("dead-worker"),
        "the crashed worker's stale claim must have been superseded"
    );
    assert!(attempts >= 2, "reclaim increments attempts again");
    assert!(
        lease_expires_at.is_some_and(|t| t > Utc::now()),
        "the reclaiming worker holds a fresh, unexpired lease"
    );
    // Kind-scoped, so `worker-b` must be the one that reclaimed it — no other
    // actor in the suite can hold this kind.
    assert!(
        we_claimed_it,
        "worker-b's claim_kinds is scoped to our own kind; it must be the reclaimer"
    );
    assert_eq!(claimed_by.as_deref(), Some("worker-b"));
    assert_eq!(attempts, 2);

    // A second claim right after must NOT pick it up again — it now holds a
    // fresh, unexpired lease (held by worker-b).
    let claimed_again = r
        .claim_kinds(10, "worker-c", std::time::Duration::from_secs(60), &[&kind])
        .await
        .unwrap();
    assert!(
        !claimed_again.iter().any(|j| j.id == id),
        "a freshly (re)claimed job must not be claimed again"
    );

    clear_kind(&kind).await;
}

#[db_test]
async fn finish_success_reports_lost_ownership_for_the_wrong_claimant(
    _tx: &mut bikesnest_test_support::TestTx,
) {
    let r = repo().await;
    let now = Utc::now();
    let id = r
        .enqueue("jobtest.zombie", &json!({}), now, Some(5), None)
        .await
        .unwrap()
        .unwrap();
    // The row is currently (re)claimed by "worker-b" (as if worker-a's original
    // claim expired and was reassigned).
    sqlx::query(
        "UPDATE background_job SET state='running', claimed_by='worker-b',
            lease_expires_at=now()+interval '60 seconds', attempts=2 WHERE id=$1",
    )
    .bind(id)
    .execute(&pool().await)
    .await
    .unwrap();

    // The zombie worker-a wakes up and tries to finish its stale claim.
    assert!(matches!(
        r.finish_success(id, "worker-a", None, now).await,
        Err(bikesnest_infrastructure::JobRepoError::LostOwnership(lost)) if lost == id
    ));

    let (state, claimed_by): (String, Option<String>) =
        sqlx::query_as("SELECT state, claimed_by FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "running", "wrong claimant's write must not apply");
    assert_eq!(claimed_by.as_deref(), Some("worker-b"));

    clear_kind("jobtest.zombie").await;
}

/// Graceful shutdown (WP7): cancelling the token while the worker sits in its
/// idle poll must return from `run` well inside one poll interval — not after
/// it — and must leave nothing claimed.
///
/// `batch_size` 0 makes `claim` a `LIMIT 0` query, so the worker only ever
/// idle-polls and cannot disturb rows other tests own.
#[db_test]
async fn cancelling_the_token_stops_an_idle_worker(_tx: &mut bikesnest_test_support::TestTx) {
    let config = JobConfig {
        enabled: true,
        // Far longer than the test may take: if cancellation did not interrupt
        // the sleep, the timeout below would fire instead.
        poll_interval: std::time::Duration::from_secs(60),
        batch_size: 0,
        ..JobConfig::default()
    };
    let worker = Worker::new(
        repo().await,
        std::sync::Arc::new(JobRegistry::new(Vec::new(), Vec::new())),
        config,
    );
    let worker_id = worker.id().to_string();

    let token = CancellationToken::new();
    let handle = tokio::spawn(worker.run(token.clone()));
    // Let the loop reach its idle sleep before signalling.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    let started = std::time::Instant::now();
    token.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("run must return promptly after cancellation, not after the poll interval")
        .expect("worker task must not panic");
    assert!(
        started.elapsed() < config.poll_interval,
        "returned only after the full poll interval: {:?}",
        started.elapsed()
    );

    let running: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM background_job WHERE state = 'running' AND claimed_by = $1",
    )
    .bind(&worker_id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(running, 0, "a stopped worker must leave no job running");
}

async fn wait_for_count(counter: &std::sync::atomic::AtomicUsize, expected: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while counter.load(std::sync::atomic::Ordering::SeqCst) < expected {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn worker_claims_only_capacity_and_heartbeats_every_active_lease() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let repo = SqlxJobRepository::new(Db::from_pool(pool.clone()));
        repo.enqueue(CAPACITY_KIND, &json!({}), Utc::now(), Some(3), None)
            .await
            .unwrap();
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let registry = std::sync::Arc::new(JobRegistry::new(
            vec![Box::new(BlockingHandler {
                kind: CAPACITY_KIND,
                started: started.clone(),
                active: active.clone(),
                peak: peak.clone(),
                gate: gate.clone(),
            })],
            vec![],
        ));
        let config = JobConfig {
            batch_size: 2,
            poll_interval: std::time::Duration::from_millis(5),
            lease_ttl: std::time::Duration::from_millis(120),
            handler_timeout: std::time::Duration::from_secs(5),
            ..JobConfig::default()
        };
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(
            Worker::new(repo.clone(), registry, config)
                .run_kinds(shutdown.clone(), vec![CAPACITY_KIND.into()]),
        );
        wait_for_count(&started, 1).await;
        // Arrival while one long handler occupies only half the capacity must be
        // observed on the poll cadence, not wait for that handler to finish.
        repo.enqueue(CAPACITY_KIND, &json!({}), Utc::now(), Some(3), None)
            .await
            .unwrap();
        wait_for_count(&started, 2).await;
        repo.enqueue(CAPACITY_KIND, &json!({}), Utc::now(), Some(3), None)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let states:(i64,i64,i64)=sqlx::query_as("SELECT count(*) FILTER(WHERE state='running'),count(*) FILTER(WHERE state='pending'),count(*) FILTER(WHERE state='running' AND lease_expires_at>clock_timestamp()) FROM background_job WHERE kind=$1").bind(CAPACITY_KIND).fetch_one(&pool).await.unwrap();
        assert_eq!(states, (2, 1, 2));
        assert_eq!(peak.load(std::sync::atomic::Ordering::SeqCst), 2);
        gate.add_permits(3);
        wait_for_count(&started, 3).await;
        shutdown.cancel();
        task.await.unwrap();
    });
}

#[test]
fn worker_contains_panics_and_persists_a_bounded_retry() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let repo = SqlxJobRepository::new(Db::from_pool(pool.clone()));
        let id = repo
            .enqueue(PANIC_KIND, &json!({}), Utc::now(), Some(3), None)
            .await
            .unwrap()
            .unwrap();
        let registry = std::sync::Arc::new(JobRegistry::new(vec![Box::new(PanicHandler)], vec![]));
        let config = JobConfig {
            batch_size: 1,
            poll_interval: std::time::Duration::from_millis(5),
            lease_ttl: std::time::Duration::from_secs(2),
            ..JobConfig::default()
        };
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(
            Worker::new(repo, registry, config)
                .run_kinds(shutdown.clone(), vec![PANIC_KIND.into()]),
        );
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let row: (String, Option<String>) =
                    sqlx::query_as("SELECT state,last_error FROM background_job WHERE id=$1")
                        .bind(id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                if row.0 == "pending" && row.1.is_some() {
                    assert_eq!(row.1.as_deref(), Some("handler task panicked"));
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        shutdown.cancel();
        task.await.unwrap();
    });
}

#[test]
fn worker_times_out_and_does_not_detach_the_handler() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let repo = SqlxJobRepository::new(Db::from_pool(pool.clone()));
        let id = repo
            .enqueue(TIMEOUT_KIND, &json!({}), Utc::now(), Some(3), None)
            .await
            .unwrap()
            .unwrap();
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let registry = std::sync::Arc::new(JobRegistry::new(
            vec![Box::new(BlockingHandler {
                kind: TIMEOUT_KIND,
                started: started.clone(),
                active: active.clone(),
                peak,
                gate,
            })],
            vec![],
        ));
        let config = JobConfig {
            batch_size: 1,
            poll_interval: std::time::Duration::from_secs(5),
            lease_ttl: std::time::Duration::from_secs(2),
            handler_timeout: std::time::Duration::from_millis(50),
            ..JobConfig::default()
        };
        let worker = Worker::new(repo, registry, config);
        let diagnostics = worker.diagnostics();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(worker.run_kinds(shutdown.clone(), vec![TIMEOUT_KIND.into()]));
        wait_for_count(&started, 1).await;
        let row = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let row: (String, Option<String>) =
                    sqlx::query_as("SELECT state,last_error FROM background_job WHERE id=$1")
                        .bind(id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                if row.0 == "pending" {
                    break row;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(row.0, "pending");
        assert_eq!(row.1.as_deref(), Some("handler timed out"));
        assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(diagnostics.observable_failures(), 0);
        shutdown.cancel();
        task.await.unwrap();
    });
}

#[test]
fn expired_and_reclaimed_leases_fence_every_stale_write() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let query_pool = pool.clone();
        let repo = SqlxJobRepository::new(Db::from_pool(pool));
        let id = repo
            .enqueue(OUTCOME_KIND, &json!({}), Utc::now(), Some(3), None)
            .await
            .unwrap()
            .unwrap();
        let first = repo
            .claim_kinds(
                1,
                "first",
                std::time::Duration::from_millis(30),
                &[OUTCOME_KIND],
            )
            .await
            .unwrap()
            .pop()
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(matches!(
            repo.heartbeat(id, &first.owner, std::time::Duration::from_secs(1))
                .await,
            Err(bikesnest_infrastructure::JobRepoError::LostOwnership(_))
        ));
        assert!(matches!(
            repo.finish_success(id, &first.owner, None, Utc::now())
                .await,
            Err(bikesnest_infrastructure::JobRepoError::LostOwnership(_))
        ));
        let second = repo
            .claim_kinds(
                1,
                "second",
                std::time::Duration::from_secs(1),
                &[OUTCOME_KIND],
            )
            .await
            .unwrap()
            .pop()
            .unwrap();
        assert_ne!(first.owner, second.owner);
        assert!(matches!(
            repo.retry(id, &first.owner, "stale", Utc::now()).await,
            Err(bikesnest_infrastructure::JobRepoError::LostOwnership(_))
        ));
        assert!(matches!(
            repo.fail(id, &first.owner, "stale").await,
            Err(bikesnest_infrastructure::JobRepoError::LostOwnership(_))
        ));
        repo.finish_success(id, &second.owner, None, Utc::now())
            .await
            .unwrap();
        let state: String = sqlx::query_scalar("SELECT state FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&query_pool)
            .await
            .unwrap();
        assert_eq!(state, "succeeded");
    });
}

#[db_test]
async fn outcome_write_failure_is_observable_and_not_reported_as_success(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    let id = repo
        .enqueue(OUTCOME_KIND, &json!({}), Utc::now(), Some(3), None)
        .await
        .unwrap()
        .unwrap();
    let claimed = repo
        .claim_kinds(
            1,
            "outcome-owner",
            std::time::Duration::from_secs(5),
            &[OUTCOME_KIND],
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    sqlx::raw_sql("CREATE FUNCTION pg_temp.fail_job_outcome() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$; CREATE TRIGGER fail_job_outcome BEFORE UPDATE ON background_job FOR EACH ROW WHEN (NEW.state='succeeded') EXECUTE FUNCTION pg_temp.fail_job_outcome()")
        .execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    let worker = Worker::new(
        repo,
        std::sync::Arc::new(JobRegistry::new(vec![Box::new(SuccessHandler)], vec![])),
        JobConfig::default(),
    );
    let diagnostics = worker.diagnostics();
    worker.process_claimed(claimed).await;
    assert_eq!(diagnostics.observable_failures(), 1);
    let state: String = sqlx::query_scalar("SELECT state FROM background_job WHERE id=$1")
        .bind(id)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(state, "running");
}

async fn failed_outcome_write_case(
    tx: &mut bikesnest_test_support::TestTx,
    kind: &'static str,
    handler: Box<dyn bikesnest_application::JobHandler>,
    terminal_state: &'static str,
) {
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    let id = repo
        .enqueue(kind, &json!({}), Utc::now(), Some(3), None)
        .await
        .unwrap()
        .unwrap();
    let claimed = repo
        .claim_kinds(
            1,
            "failed-outcome-owner",
            std::time::Duration::from_secs(5),
            &[kind],
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
    let sql = format!(
        "CREATE FUNCTION pg_temp.skip_job_outcome() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NULL; END $$; CREATE TRIGGER skip_job_outcome BEFORE UPDATE ON background_job FOR EACH ROW WHEN (NEW.state='{terminal_state}') EXECUTE FUNCTION pg_temp.skip_job_outcome()"
    );
    sqlx::raw_sql(&sql)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let worker = Worker::new(
        repo,
        std::sync::Arc::new(JobRegistry::new(vec![handler], vec![])),
        JobConfig::default(),
    );
    let diagnostics = worker.diagnostics();
    worker.process_claimed(claimed).await;
    assert_eq!(diagnostics.observable_failures(), 1);
    let state: String = sqlx::query_scalar("SELECT state FROM background_job WHERE id=$1")
        .bind(id)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(state, "running");
}

#[db_test]
async fn retry_outcome_write_failure_is_observable(tx: &mut bikesnest_test_support::TestTx) {
    failed_outcome_write_case(
        tx,
        RETRY_OUTCOME_KIND,
        Box::new(FailureHandler {
            kind: RETRY_OUTCOME_KIND,
            permanent: false,
        }),
        "pending",
    )
    .await;
}

#[db_test]
async fn dead_letter_outcome_write_failure_is_observable(tx: &mut bikesnest_test_support::TestTx) {
    failed_outcome_write_case(
        tx,
        DEAD_OUTCOME_KIND,
        Box::new(FailureHandler {
            kind: DEAD_OUTCOME_KIND,
            permanent: true,
        }),
        "failed",
    )
    .await;
}

async fn hook_failure_case(
    tx: &mut bikesnest_test_support::TestTx,
    kind: &'static str,
    mode: HookMode,
) {
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    let id = repo
        .enqueue(kind, &json!({}), Utc::now(), Some(3), None)
        .await
        .unwrap()
        .unwrap();
    let claimed = repo
        .claim_kinds(1, "hook-owner", std::time::Duration::from_secs(5), &[kind])
        .await
        .unwrap()
        .pop()
        .unwrap();
    let config = JobConfig {
        handler_timeout: std::time::Duration::from_millis(30),
        ..JobConfig::default()
    };
    let worker = Worker::new(
        repo,
        std::sync::Arc::new(JobRegistry::new(
            vec![Box::new(HookHandler { kind, mode })],
            vec![],
        )),
        config,
    );
    let diagnostics = worker.diagnostics();
    worker.process_claimed(claimed).await;
    assert_eq!(diagnostics.observable_failures(), 1);
    let state: String = sqlx::query_scalar("SELECT state FROM background_job WHERE id=$1")
        .bind(id)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(state, "failed");
}

#[db_test]
async fn dead_letter_hook_panic_is_contained(tx: &mut bikesnest_test_support::TestTx) {
    hook_failure_case(tx, HOOK_PANIC_KIND, HookMode::Panic).await;
}

#[db_test]
async fn dead_letter_hook_timeout_is_contained(tx: &mut bikesnest_test_support::TestTx) {
    hook_failure_case(tx, HOOK_TIMEOUT_KIND, HookMode::Timeout).await;
}

#[test]
fn heartbeat_ownership_loss_cancels_the_active_handler() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let repo = SqlxJobRepository::new(Db::from_pool(pool.clone()));
        let id = repo
            .enqueue(HEARTBEAT_KIND, &json!({}), Utc::now(), Some(3), None)
            .await
            .unwrap()
            .unwrap();
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let registry = std::sync::Arc::new(JobRegistry::new(
            vec![Box::new(BlockingHandler {
                kind: HEARTBEAT_KIND,
                started: started.clone(),
                active: active.clone(),
                peak,
                gate,
            })],
            vec![],
        ));
        let config = JobConfig {
            batch_size: 1,
            poll_interval: std::time::Duration::from_secs(5),
            lease_ttl: std::time::Duration::from_millis(90),
            handler_timeout: std::time::Duration::from_secs(5),
            ..JobConfig::default()
        };
        let worker = Worker::new(repo, registry, config);
        let diagnostics = worker.diagnostics();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(worker.run_kinds(shutdown.clone(), vec![HEARTBEAT_KIND.into()]));
        wait_for_count(&started, 1).await;
        sqlx::query("UPDATE background_job SET claimed_by='replacement-owner' WHERE id=$1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while active.load(std::sync::atomic::Ordering::SeqCst) != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(diagnostics.observable_failures(), 1);
        shutdown.cancel();
        task.await.unwrap();
    });
}

#[test]
fn shutdown_grace_cancels_and_drains_handler_and_heartbeat_children() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let repo = SqlxJobRepository::new(Db::from_pool(pool.clone()));
        let id = repo
            .enqueue(SHUTDOWN_KIND, &json!({}), Utc::now(), Some(3), None)
            .await
            .unwrap()
            .unwrap();
        let started = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(0));
        let registry = std::sync::Arc::new(JobRegistry::new(
            vec![Box::new(BlockingHandler {
                kind: SHUTDOWN_KIND,
                started: started.clone(),
                active: active.clone(),
                peak,
                gate,
            })],
            vec![],
        ));
        let config = JobConfig {
            batch_size: 1,
            poll_interval: std::time::Duration::from_secs(5),
            lease_ttl: std::time::Duration::from_secs(2),
            handler_timeout: std::time::Duration::from_secs(10),
            shutdown_grace: std::time::Duration::from_millis(50),
            ..JobConfig::default()
        };
        let worker = Worker::new(repo, registry, config);
        let diagnostics = worker.diagnostics();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(worker.run_kinds(shutdown.clone(), vec![SHUTDOWN_KIND.into()]));
        wait_for_count(&started, 1).await;
        shutdown.cancel();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(active.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(diagnostics.observable_failures(), 1);
        let heartbeat: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar("SELECT heartbeat_at FROM background_job WHERE id=$1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let after: Option<chrono::DateTime<Utc>> =
            sqlx::query_scalar("SELECT heartbeat_at FROM background_job WHERE id=$1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(after, heartbeat);
    });
}

// ---------------------------------------------------------------------------
// email.send: the queued transactional email
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use bikesnest_application::{
    AdmittedAuthMail, AuthMailDispatcher, EmailError, EmailKind, EmailMessage, EmailProvider,
    EmailQueue, JobError, JobHandler,
};
use bikesnest_domain::{LocaleCode, UserId, VerificationToken};
use bikesnest_infrastructure::email::idempotency_key;
use bikesnest_infrastructure::{
    FakeEmailProvider, InlineAuthMailDispatcher, JobEmailQueue, SendEmailHandler,
};
use std::sync::Arc;

/// A token nothing else in the suite (or a previous run) can collide with:
/// the idempotency key is derived from it, and the key is UNIQUE forever.
fn unique_token(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("emailtest-{tag}-{nanos}")
}

async fn verify_message(tag: &str, db: &Db) -> EmailMessage {
    use sha2::{Digest, Sha256};
    let bytes: [u8; 32] = Sha256::digest(tag.as_bytes()).into();
    let token = VerificationToken::new(bytes);
    let email = format!("mail-{tag}@example.com");
    let mut conn = db.acquire().await.unwrap();
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users(email, account_state, email_verified_at) VALUES($1, 'ACTIVE', now()) RETURNING id"
    ).bind(&email).fetch_one(&mut *conn).await.unwrap();
    let token_hash: String = Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    sqlx::query("INSERT INTO email_verification_tokens(token_hash,user_id,email,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')")
        .bind(token_hash).bind(user_id).bind(&email).execute(&mut *conn).await.unwrap();
    EmailMessage::linked(
        UserId(user_id),
        email,
        LocaleCode::PtBr,
        EmailKind::VerifyEmail {
            link: format!(
                "http://localhost:8080/verify-email?token={}",
                token.to_base64url()
            ),
        },
    )
}

async fn row_for_key(key: &str, db: &Db) -> Option<(i64, serde_json::Value, i32)> {
    let mut conn = db.acquire().await.unwrap();
    sqlx::query_as(
        "SELECT id, payload, max_attempts FROM background_job WHERE idempotency_key = $1",
    )
    .bind(key)
    .fetch_optional(&mut *conn)
    .await
    .unwrap()
}

async fn delete_job(id: i64, db: &Db) {
    let mut conn = db.acquire().await.unwrap();
    sqlx::query("DELETE FROM background_job WHERE id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await
        .unwrap();
}

/// Simulate a claim with a direct `UPDATE` rather than calling `claim` — which
/// is not scoped by kind and would race the other tests in this file.
async fn simulate_claim(id: i64, worker: &str, attempt: i32, db: &Db) {
    let mut conn = db.acquire().await.unwrap();
    sqlx::query(
        "UPDATE background_job SET state='running', claimed_by=$2,
            lease_expires_at=now()+interval '60 seconds', attempts=$3 WHERE id=$1",
    )
    .bind(id)
    .bind(worker)
    .bind(attempt)
    .execute(&mut *conn)
    .await
    .unwrap();
}

/// A provider that always fails, so the queue's retry path is exercised.
struct BrokenProvider;
#[async_trait]
impl EmailProvider for BrokenProvider {
    async fn send(&self, _msg: &EmailMessage) -> Result<(), EmailError> {
        Err(EmailError::Unexpected(
            "SECRET-PROVIDER-BODY token=LEAK ada@example.com".into(),
        ))
    }
}

struct PermanentProvider;
#[async_trait]
impl EmailProvider for PermanentProvider {
    async fn send(&self, _msg: &EmailMessage) -> Result<(), EmailError> {
        Err(EmailError::Permanent)
    }
}

#[derive(Clone, Copy, Debug)]
enum MailPurpose {
    Verify,
    Reset,
    Change,
}

impl MailPurpose {
    const ALL: [Self; 3] = [Self::Verify, Self::Reset, Self::Change];

    fn code(self) -> &'static str {
        match self {
            Self::Verify => "verify",
            Self::Reset => "reset",
            Self::Change => "change",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum InvalidCredential {
    AccountState,
    TokenHash,
    Recipient,
    Used,
    Expired,
}

impl InvalidCredential {
    const ALL: [Self; 5] = [
        Self::AccountState,
        Self::TokenHash,
        Self::Recipient,
        Self::Used,
        Self::Expired,
    ];
}

async fn lifecycle_message(db: &Db, purpose: MailPurpose, case: &str) -> (EmailMessage, String) {
    use sha2::{Digest, Sha256};

    let token_bytes: [u8; 32] = Sha256::digest(case.as_bytes()).into();
    let token = VerificationToken::new(token_bytes);
    let token_hash: String = Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let email = format!("mail-{case}@example.com");
    let mut conn = db.acquire().await.unwrap();
    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users(email,account_state,email_verified_at) VALUES($1,'ACTIVE',now()) RETURNING id",
    )
    .bind(&email)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    match purpose {
        MailPurpose::Verify | MailPurpose::Change => {
            sqlx::query("INSERT INTO email_verification_tokens(token_hash,user_id,email,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')")
                .bind(&token_hash)
                .bind(user_id)
                .bind(&email)
                .execute(&mut *conn)
                .await
                .unwrap();
        }
        MailPurpose::Reset => {
            sqlx::query("INSERT INTO password_reset_tokens(token_hash,user_id,expires_at) VALUES($1,$2,now()+interval '1 hour')")
                .bind(&token_hash)
                .bind(user_id)
                .execute(&mut *conn)
                .await
                .unwrap();
        }
    }
    let link = format!(
        "https://x/{}?token={}",
        if matches!(purpose, MailPurpose::Reset) {
            "password-reset/new"
        } else {
            "verify-email"
        },
        token.to_base64url()
    );
    let kind = match purpose {
        MailPurpose::Verify => EmailKind::VerifyEmail { link },
        MailPurpose::Reset => EmailKind::ResetPassword { link },
        MailPurpose::Change => EmailKind::ConfirmEmailChange { link },
    };
    (
        EmailMessage::linked(UserId(user_id), email, LocaleCode::En, kind),
        token_hash,
    )
}

async fn invalidate_credential(
    db: &Db,
    purpose: MailPurpose,
    invalid: InvalidCredential,
    msg: &mut EmailMessage,
    token_hash: &str,
) {
    let mut conn = db.acquire().await.unwrap();
    match invalid {
        InvalidCredential::AccountState => {
            sqlx::query("UPDATE users SET account_state='SUSPENDED' WHERE id=$1")
                .bind(msg.account_id)
                .execute(&mut *conn)
                .await
                .unwrap();
        }
        InvalidCredential::TokenHash => {
            let token = VerificationToken::new([0xfe; 32]);
            let link = format!(
                "https://x/{}?token={}",
                if matches!(purpose, MailPurpose::Reset) {
                    "password-reset/new"
                } else {
                    "verify-email"
                },
                token.to_base64url()
            );
            msg.kind = match purpose {
                MailPurpose::Verify => EmailKind::VerifyEmail { link },
                MailPurpose::Reset => EmailKind::ResetPassword { link },
                MailPurpose::Change => EmailKind::ConfirmEmailChange { link },
            };
        }
        InvalidCredential::Recipient => msg.to = format!("wrong-{}@example.com", msg.account_id),
        InvalidCredential::Used => {
            let table = if matches!(purpose, MailPurpose::Reset) {
                "password_reset_tokens"
            } else {
                "email_verification_tokens"
            };
            sqlx::query(&format!(
                "UPDATE {table} SET used_at=now() WHERE token_hash=$1"
            ))
            .bind(token_hash)
            .execute(&mut *conn)
            .await
            .unwrap();
        }
        InvalidCredential::Expired => {
            let table = if matches!(purpose, MailPurpose::Reset) {
                "password_reset_tokens"
            } else {
                "email_verification_tokens"
            };
            sqlx::query(&format!(
                "UPDATE {table} SET expires_at=now()-interval '1 second' WHERE token_hash=$1"
            ))
            .bind(token_hash)
            .execute(&mut *conn)
            .await
            .unwrap();
        }
    }
}

#[db_test]
async fn mail_handler_and_inline_queue_enforce_exact_token_lifecycle(
    tx: &mut bikesnest_test_support::TestTx,
) {
    use bikesnest_infrastructure::InlineEmailQueue;
    let db = tx.db().await;
    for purpose in MailPurpose::ALL {
        for inline in [false, true] {
            let case = format!("positive-{}-{inline}", purpose.code());
            let (msg, _) = lifecycle_message(&db, purpose, &case).await;
            let provider = FakeEmailProvider::with_root(None);
            if inline {
                InlineEmailQueue::new(db.clone(), Arc::new(provider.clone()))
                    .enqueue(msg)
                    .await
                    .unwrap();
            } else {
                SendEmailHandler::new(db.clone(), Arc::new(provider.clone()))
                    .run(&serde_json::to_value(msg).unwrap())
                    .await
                    .unwrap();
            }
            assert_eq!(provider.emails().len(), 1, "{purpose:?}, inline={inline}");
        }

        for invalid in InvalidCredential::ALL {
            for inline in [false, true] {
                let case = format!("negative-{}-{invalid:?}-{inline}", purpose.code());
                let (mut msg, token_hash) = lifecycle_message(&db, purpose, &case).await;
                invalidate_credential(&db, purpose, invalid, &mut msg, &token_hash).await;
                let provider = FakeEmailProvider::with_root(None);
                let rejected = if inline {
                    InlineEmailQueue::new(db.clone(), Arc::new(provider.clone()))
                        .enqueue(msg)
                        .await
                        .is_err()
                } else {
                    matches!(
                        SendEmailHandler::new(db.clone(), Arc::new(provider.clone()))
                            .run(&serde_json::to_value(msg).unwrap())
                            .await,
                        Err(JobError::Permanent(_))
                    )
                };
                assert!(rejected, "{purpose:?}/{invalid:?}, inline={inline}");
                assert!(provider.emails().is_empty(), "{purpose:?}/{invalid:?}");
            }
        }
    }
}

#[db_test]
async fn queue_admission_enforces_every_purpose_and_credential_gate(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let queue = JobEmailQueue::new(SqlxJobRepository::new(db.clone()), 3);
    for purpose in MailPurpose::ALL {
        let (msg, _) = lifecycle_message(&db, purpose, &format!("admit-{}", purpose.code())).await;
        queue.enqueue(msg).await.unwrap();

        for invalid in InvalidCredential::ALL {
            let case = format!("reject-{}-{invalid:?}", purpose.code());
            let (mut msg, token_hash) = lifecycle_message(&db, purpose, &case).await;
            invalidate_credential(&db, purpose, invalid, &mut msg, &token_hash).await;
            assert!(queue.enqueue(msg).await.is_err(), "{purpose:?}/{invalid:?}");
        }
    }
}

#[tokio::test]
async fn queue_admission_database_error_tracing_excludes_hostile_recipient_data() {
    let marker = "HOSTILE-RECIPIENT-MARKER.invalid";
    let token = VerificationToken::new([0xab; 32]);
    let msg = EmailMessage::linked(
        UserId(77),
        format!("ada@{marker}"),
        LocaleCode::En,
        EmailKind::ResetPassword {
            link: format!(
                "https://x/password-reset/new?token={}",
                token.to_base64url()
            ),
        },
    );
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://closed:closed@127.0.0.1:1/closed")
        .unwrap();
    pool.close().await;
    let queue = JobEmailQueue::new(SqlxJobRepository::new(Db::from_pool(pool)), 1);
    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureLayer(events.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);

    assert!(queue.enqueue(msg).await.is_err());
    let captured = events.lock().unwrap().join("\n");
    assert!(captured.contains("kind=\"reset\""), "{captured}");
    assert!(
        captured.contains("reason=\"database_unavailable\""),
        "{captured}"
    );
    assert!(!captured.contains(marker), "{captured}");
    assert!(!captured.contains("closed@"), "{captured}");
}

/// The idempotency key deduplicates queue admission for a double-submitted form
/// or retried request. It does not promise exactly-once provider delivery. A
/// *fresh* token is a different message and must still get its own job.
#[db_test]
async fn queueing_one_message_twice_creates_a_single_job(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let jobs = SqlxJobRepository::new(db.clone());
    let queue = JobEmailQueue::new(jobs.clone(), 3);
    let msg = verify_message(&unique_token("dedupe"), &db).await;

    queue.enqueue(msg.clone()).await.unwrap();
    queue.enqueue(msg.clone()).await.unwrap();

    let key = idempotency_key(&msg);
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM background_job WHERE idempotency_key = $1")
            .bind(&key)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(n, 1, "the same token must never be queued twice");

    // Running that single job delivers exactly one message, rendered in the
    // locale the payload carries.
    let (id, payload, max_attempts) = row_for_key(&key, &db).await.expect("queued row");
    assert_eq!(
        max_attempts, 3,
        "the row carries the configured attempt budget"
    );
    let mail = FakeEmailProvider::with_root(None);
    SendEmailHandler::new(db.clone(), Arc::new(mail.clone()))
        .run(&payload)
        .await
        .unwrap();
    assert_eq!(mail.emails().len(), 1);
    assert_eq!(mail.emails()[0].subject, "Confirme seu e-mail no BikesNest");
    simulate_claim(id, "worker-mail-success", 1, &db).await;
    jobs.finish_success(id, "worker-mail-success", None, Utc::now())
        .await
        .unwrap();
    let (terminal_payload, redacted_at): (serde_json::Value, Option<chrono::DateTime<Utc>>) =
        sqlx::query_as("SELECT payload,payload_redacted_at FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(terminal_payload, serde_json::json!({}));
    assert!(redacted_at.is_some());

    // A re-send issues a new token → a new job, not a swallowed duplicate.
    let resend = verify_message(&unique_token("dedupe-resend"), &db).await;
    queue.enqueue(resend.clone()).await.unwrap();
    let resend_key = idempotency_key(&resend);
    assert_ne!(resend_key, key);
    let (resend_id, _, _) = row_for_key(&resend_key, &db)
        .await
        .expect("second job queued");

    delete_job(id, &db).await;
    delete_job(resend_id, &db).await;
}

/// A provider outage is transient: the job is retried while its budget lasts
/// and dead-lettered when it runs out. The stored error is a bounded
/// classification and never copies provider text, an address, or a link.
#[db_test]
async fn a_failing_email_send_is_retried_then_dead_lettered(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    // A deliberately small budget (production's default is 5).
    let jobs = SqlxJobRepository::new(db.clone());
    let queue = JobEmailQueue::new(jobs.clone(), 2);
    let msg = verify_message(&unique_token("deadletter"), &db).await;
    queue.enqueue(msg.clone()).await.unwrap();

    let key = idempotency_key(&msg);
    let (id, payload, max_attempts) = row_for_key(&key, &db).await.expect("queued row");
    assert_eq!(max_attempts, 2);
    let handler = SendEmailHandler::new(db.clone(), Arc::new(BrokenProvider));

    // Attempt 1 of 2 → within budget → requeued with the error recorded.
    simulate_claim(id, "worker-mail", 1, &db).await;
    let first = handler.run(&payload).await.unwrap_err();
    assert!(
        matches!(first, JobError::Failed(_)),
        "a provider outage must be retryable, not permanent: {first:?}"
    );
    let run_at = Utc::now() + Duration::seconds(30);
    jobs.retry(id, "worker-mail", &first.to_string(), run_at)
        .await
        .unwrap();
    let (state, attempts): (String, i32) =
        sqlx::query_as("SELECT state, attempts FROM background_job WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(state, "pending", "still retryable");
    assert_eq!(attempts, 1);

    // Attempt 2 == the budget → the worker dead-letters instead of retrying.
    simulate_claim(id, "worker-mail", 2, &db).await;
    let last = handler.run(&payload).await.unwrap_err();
    let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureLayer(events.clone()));
    let _guard = tracing::subscriber::set_default(subscriber);
    handler.on_dead_letter(&payload, &last.to_string()).await;
    jobs.fail(id, "worker-mail", &last.to_string())
        .await
        .unwrap();

    type TerminalMailRow = (
        String,
        Option<chrono::DateTime<Utc>>,
        Option<String>,
        serde_json::Value,
        Option<chrono::DateTime<Utc>>,
    );
    let (state, finished, last_error, terminal_payload, redacted_at): TerminalMailRow =
        sqlx::query_as("SELECT state, finished_at, last_error, payload, payload_redacted_at FROM background_job WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(state, "failed", "a spent budget dead-letters the job");
    assert!(finished.is_some());
    assert_eq!(terminal_payload, serde_json::json!({}));
    assert!(redacted_at.is_some());
    let recorded = last_error.unwrap_or_default();
    assert_eq!(recorded, "job failed: verify mail provider failed");
    assert!(
        !recorded.contains("ada@")
            && !recorded.contains("token=")
            && !recorded.contains("example.com"),
        "no address and no live link in a stored error: {recorded}"
    );
    assert!(!recorded.contains("SECRET-PROVIDER-BODY"));
    let captured = events.lock().unwrap().join("\n");
    assert!(
        !captured.contains("SECRET-PROVIDER-BODY")
            && !captured.contains("ada@example.com")
            && !captured.contains("token="),
        "{captured}"
    );

    delete_job(id, &db).await;
}

#[db_test]
async fn inline_dispatch_persists_backoff_then_retries_only_the_admitted_job(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let jobs = SqlxJobRepository::new(db.clone());
    let queue = JobEmailQueue::new(jobs, 3);
    let msg = verify_message(&unique_token("inline-retry"), &db).await;
    queue.enqueue(msg.clone()).await.unwrap();
    let (id, _, _) = row_for_key(&idempotency_key(&msg), &db).await.unwrap();
    let admitted = AdmittedAuthMail {
        job_id: id,
        message: msg,
    };
    let broken = InlineAuthMailDispatcher::new(db.clone(), Arc::new(BrokenProvider));
    assert_eq!(
        broken.dispatch(admitted.clone()).await.unwrap_err(),
        bikesnest_application::AuthError::Unavailable
    );
    let attempts: i32 = sqlx::query_scalar("SELECT attempts FROM background_job WHERE id=$1")
        .bind(id)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(attempts, 1);
    assert!(broken.dispatch(admitted.clone()).await.is_err());
    let attempts_again: i32 = sqlx::query_scalar("SELECT attempts FROM background_job WHERE id=$1")
        .bind(id)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        attempts_again, 1,
        "inline HTTP retry must honor persisted backoff"
    );
    sqlx::query(
        "UPDATE background_job SET run_at=clock_timestamp()-interval '1 second' WHERE id=$1",
    )
    .bind(id)
    .execute(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    let delivered = FakeEmailProvider::with_root(None);
    InlineAuthMailDispatcher::new(db.clone(), Arc::new(delivered.clone()))
        .dispatch(admitted)
        .await
        .unwrap();
    assert_eq!(delivered.emails().len(), 1);
    let terminal: (String, serde_json::Value) =
        sqlx::query_as("SELECT state,payload FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(terminal, ("succeeded".into(), serde_json::json!({})));
}

#[db_test]
async fn inline_dispatch_dead_letters_permanent_provider_rejection_immediately(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let queue = JobEmailQueue::new(SqlxJobRepository::new(db.clone()), 5);
    let msg = verify_message(&unique_token("inline-permanent"), &db).await;
    queue.enqueue(msg.clone()).await.unwrap();
    let (id, _, _) = row_for_key(&idempotency_key(&msg), &db).await.unwrap();
    let error = InlineAuthMailDispatcher::new(db.clone(), Arc::new(PermanentProvider))
        .dispatch(AdmittedAuthMail {
            job_id: id,
            message: msg,
        })
        .await
        .unwrap_err();
    assert_eq!(error, bikesnest_application::AuthError::Internal);
    let row: (String, i32, serde_json::Value) =
        sqlx::query_as("SELECT state,attempts,payload FROM background_job WHERE id=$1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(row, ("failed".into(), 1, serde_json::json!({})));
}

#[db_test]
async fn inline_dispatch_does_not_duplicate_an_active_lease(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let queue = JobEmailQueue::new(SqlxJobRepository::new(db.clone()), 3);
    let msg = verify_message(&unique_token("inline-active"), &db).await;
    queue.enqueue(msg.clone()).await.unwrap();
    let (id, _, _) = row_for_key(&idempotency_key(&msg), &db).await.unwrap();
    simulate_claim(id, "already-sending", 1, &db).await;
    let provider = FakeEmailProvider::with_root(None);
    InlineAuthMailDispatcher::new(db.clone(), Arc::new(provider.clone()))
        .dispatch(AdmittedAuthMail {
            job_id: id,
            message: msg,
        })
        .await
        .unwrap();
    assert!(provider.emails().is_empty());
    let owner: String = sqlx::query_scalar("SELECT claimed_by FROM background_job WHERE id=$1")
        .bind(id)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(owner, "already-sending");
}
