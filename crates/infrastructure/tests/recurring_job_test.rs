//! Recurring registration and reconciliation through the real worker bootstrap.

use async_trait::async_trait;
use bikesnest_application::{JobError, JobHandler, JobPayload};
use bikesnest_infrastructure::{
    JobConfig, JobRegistry, RecurringKind, RecurringRegistrationOutcome, SqlxJobRepository, Worker,
};
use bikesnest_test_support::db_test;
use chrono::{Duration, Utc};
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio_util::sync::CancellationToken;

const FLOW_KIND: &str = "test.recurring.bootstrap.flow";
const FLOW_KEY: &str = "test:recurring:bootstrap:flow";
const LEGACY_KIND: &str = "test.recurring.bootstrap.legacy";
const REVISIT_KIND: &str = "test.recurring.bootstrap.revisit";
const REVISIT_KEY: &str = "test:recurring:bootstrap:revisit";

struct HarmlessHandler {
    kind: &'static str,
    runs: Arc<AtomicUsize>,
}

#[derive(sqlx::FromRow)]
struct LiveRow {
    state: String,
    claimed_by: Option<String>,
    schedule: Option<serde_json::Value>,
    attempts: i32,
    run_at: chrono::DateTime<Utc>,
    lease_expires_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, PartialEq, sqlx::FromRow)]
struct StoredRow {
    id: i64,
    kind: String,
    payload: serde_json::Value,
    state: String,
    schedule: Option<serde_json::Value>,
    attempts: i32,
    max_attempts: i32,
    run_at: chrono::DateTime<Utc>,
    claimed_by: Option<String>,
    lease_expires_at: Option<chrono::DateTime<Utc>>,
    last_error: Option<String>,
    finished_at: Option<chrono::DateTime<Utc>>,
}

async fn stored(db: &bikesnest_infrastructure::Db, key: &str) -> StoredRow {
    let mut conn = db.acquire().await.unwrap();
    sqlx::query_as("SELECT id,kind,payload,state,schedule,attempts,max_attempts,run_at,claimed_by,lease_expires_at,last_error,finished_at FROM background_job WHERE idempotency_key=$1")
        .bind(key).fetch_one(&mut *conn).await.unwrap()
}

#[async_trait]
impl JobHandler for HarmlessHandler {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn run(&self, _payload: &JobPayload) -> Result<(), JobError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

struct BusyHandler {
    repo: SqlxJobRepository,
    runs: Arc<AtomicUsize>,
    reached_target: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl JobHandler for BusyHandler {
    fn kind(&self) -> &'static str {
        "test.recurring.bootstrap.busy-work"
    }

    async fn run(&self, _payload: &JobPayload) -> Result<(), JobError> {
        let completed = self.runs.fetch_add(1, Ordering::SeqCst) + 1;
        if completed >= 3 {
            self.reached_target.notify_one();
        }
        self.repo
            .enqueue(self.kind(), &json!({}), Utc::now(), Some(1), None)
            .await
            .map_err(|error| JobError::Failed(error.to_string()))?;
        Ok(())
    }
}

fn registry(
    kind: &'static str,
    key: &'static str,
    every_seconds: i64,
    runs: Arc<AtomicUsize>,
) -> Arc<JobRegistry> {
    Arc::new(JobRegistry::new(
        vec![Box::new(HarmlessHandler { kind, runs })],
        vec![RecurringKind {
            job_kind: kind,
            payload: json!({}),
            schedule: json!({"every_seconds": every_seconds}),
            idempotency_key: key,
            max_attempts: 5,
        }],
    ))
}

#[db_test]
async fn bootstrap_claim_execute_finish_and_restart_recur(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    let runs = Arc::new(AtomicUsize::new(0));
    let worker = Worker::new(
        repo.clone(),
        registry(FLOW_KIND, FLOW_KEY, 1, runs.clone()),
        JobConfig::default(),
    );

    worker.bootstrap().await.unwrap();
    let mut claimed = repo
        .claim_kinds(
            1,
            worker.id(),
            std::time::Duration::from_secs(60),
            &[FLOW_KIND],
        )
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    worker.process_claimed(claimed.pop().unwrap()).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    let mut conn = db.acquire().await.unwrap();
    let (id, state, schedule, run_at, finished_at): (
        i64,
        String,
        Option<serde_json::Value>,
        chrono::DateTime<Utc>,
        Option<chrono::DateTime<Utc>>,
    ) = sqlx::query_as(
        "SELECT id, state, schedule, run_at, finished_at FROM background_job WHERE idempotency_key=$1",
    )
    .bind(FLOW_KEY)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(state, "pending");
    assert_eq!(schedule, Some(json!({"every_seconds": 1})));
    assert!(run_at > Utc::now());
    assert!(finished_at.is_some());

    // Let the next occurrence come due without waiting for the wall clock:
    // move the persisted run time into the past. The next occurrence is
    // computed from the real clock again, so it still lands after `run_at`.
    sqlx::query(
        "UPDATE background_job SET run_at = clock_timestamp() - interval '1 second' WHERE id=$1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await
    .unwrap();
    drop(conn);

    let mut claimed = repo
        .claim_kinds(
            1,
            worker.id(),
            std::time::Duration::from_secs(60),
            &[FLOW_KIND],
        )
        .await
        .unwrap();
    assert_eq!(
        claimed.len(),
        1,
        "the persisted future occurrence becomes due"
    );
    worker.process_claimed(claimed.pop().unwrap()).await;
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the recurring handler executes again"
    );

    let second_run_at = stored(&db, FLOW_KEY).await.run_at;
    assert!(second_run_at > run_at);
    worker.bootstrap().await.unwrap();
    let mut conn = db.acquire().await.unwrap();
    let rows: Vec<(i64, chrono::DateTime<Utc>)> =
        sqlx::query_as("SELECT id, run_at FROM background_job WHERE idempotency_key=$1")
            .bind(FLOW_KEY)
            .fetch_all(&mut *conn)
            .await
            .unwrap();
    assert_eq!(
        rows,
        vec![(id, second_run_at)],
        "restart preserves one future row and its run time"
    );
}

#[db_test]
async fn exact_legacy_rows_are_repaired_and_scheduled_failures_revived(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    let now = Utc::now();
    for (key, state, expected) in [
        (
            "test:recurring:legacy-pending",
            "pending",
            RecurringRegistrationOutcome::PendingScheduleRepaired,
        ),
        (
            "test:recurring:legacy-succeeded",
            "succeeded",
            RecurringRegistrationOutcome::TerminalReactivated,
        ),
        (
            "test:recurring:legacy-failed",
            "failed",
            RecurringRegistrationOutcome::TerminalReactivated,
        ),
    ] {
        let initial_run_at = if state == "pending" {
            now + Duration::hours(2)
        } else {
            now - Duration::days(1)
        };
        let mut conn = db.acquire().await.unwrap();
        let id: i64 = sqlx::query_scalar("INSERT INTO background_job(kind,payload,state,run_at,idempotency_key,finished_at,last_error,attempts) VALUES($1,'{}',$2,$3,$4,$5,'legacy error',3) RETURNING id")
            .bind(LEGACY_KIND).bind(state).bind(initial_run_at).bind(key)
            .bind((state != "pending").then_some(now - Duration::hours(1)))
            .fetch_one(&mut *conn).await.unwrap();
        drop(conn);
        let persisted_initial_run_at = stored(&db, key).await.run_at;
        let outcome = repo
            .register_recurring(
                LEGACY_KIND,
                &json!({}),
                &json!({"every_seconds": 60}),
                now,
                4,
                key,
            )
            .await
            .unwrap();
        assert_eq!(outcome, expected);
        let repaired = stored(&db, key).await;
        assert_eq!(repaired.id, id);
        assert_eq!(repaired.state, "pending");
        assert_eq!(repaired.schedule, Some(json!({"every_seconds": 60})));
        assert_eq!(repaired.max_attempts, 4);
        assert_eq!(repaired.claimed_by, None);
        assert_eq!(repaired.lease_expires_at, None);
        if state == "pending" {
            assert_eq!(repaired.run_at, persisted_initial_run_at);
            assert_eq!(repaired.attempts, 3);
        } else {
            assert_eq!(
                repaired
                    .run_at
                    .signed_duration_since(now)
                    .num_microseconds(),
                Some(0)
            );
            assert_eq!(repaired.attempts, 0);
        }
        let stable_run_at = repaired.run_at;
        assert_eq!(
            repo.register_recurring(
                LEGACY_KIND,
                &json!({}),
                &json!({"every_seconds": 60}),
                now + Duration::minutes(1),
                4,
                key,
            )
            .await
            .unwrap(),
            RecurringRegistrationOutcome::HealthyPreserved
        );
        let repeated = stored(&db, key).await;
        assert_eq!(repeated.id, id);
        assert_eq!(repeated.run_at, stable_run_at);
    }

    // A scheduled row an older release dead-lettered is revived on its next
    // scheduled run with a fresh budget; its error and last-success time stay.
    let failed_key = "test:recurring:scheduled-failed";
    let mut conn = db.acquire().await.unwrap();
    let failed_id: i64 = sqlx::query_scalar("INSERT INTO background_job(kind,payload,state,run_at,schedule,idempotency_key,attempts,last_error,finished_at,claimed_by) VALUES($1,'{}','failed',$2,$3,$4,5,'still broken',$2,'old-worker') RETURNING id")
        .bind(LEGACY_KIND).bind(now - Duration::days(3)).bind(json!({"every_seconds": 60})).bind(failed_key)
        .fetch_one(&mut *conn).await.unwrap();
    drop(conn);
    let first_failed = stored(&db, failed_key).await;
    let outcome = repo
        .register_recurring(
            LEGACY_KIND,
            &json!({}),
            &json!({"every_seconds": 60}),
            now,
            5,
            failed_key,
        )
        .await
        .unwrap();
    assert_eq!(outcome, RecurringRegistrationOutcome::FailedRevived);
    let revived = stored(&db, failed_key).await;
    assert_eq!(revived.id, failed_id);
    assert_eq!(revived.state, "pending");
    assert_eq!(revived.attempts, 0);
    assert_eq!(revived.max_attempts, 5);
    assert_eq!(revived.claimed_by, None);
    assert_eq!(revived.lease_expires_at, None);
    assert_eq!(
        revived
            .run_at
            .signed_duration_since(now + Duration::seconds(60))
            .num_microseconds(),
        Some(0),
        "revived on its next scheduled occurrence"
    );
    assert_eq!(revived.last_error.as_deref(), Some("still broken"));
    assert_eq!(revived.finished_at, first_failed.finished_at);
    assert_eq!(
        repo.register_recurring(
            LEGACY_KIND,
            &json!({}),
            &json!({"every_seconds": 60}),
            now + Duration::hours(1),
            5,
            failed_key,
        )
        .await
        .unwrap(),
        RecurringRegistrationOutcome::HealthyPreserved
    );
    assert_eq!(stored(&db, failed_key).await, revived);
    repo.gc(now + Duration::days(8)).await.unwrap();
    let mut conn = db.acquire().await.unwrap();
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM background_job WHERE idempotency_key=$1)")
            .bind(failed_key)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert!(exists, "GC never deletes a scheduled row");
}

/// Fails every run; transient or permanent.
struct FailingRecurringHandler {
    kind: &'static str,
    permanent: bool,
}

#[async_trait]
impl JobHandler for FailingRecurringHandler {
    fn kind(&self) -> &'static str {
        self.kind
    }

    async fn run(&self, _payload: &JobPayload) -> Result<(), JobError> {
        if self.permanent {
            Err(JobError::Permanent("object store down".into()))
        } else {
            Err(JobError::Failed("object store down".into()))
        }
    }
}

#[db_test]
async fn recurring_job_that_exhausts_attempts_is_rescheduled_not_dead_lettered(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    for (kind, key, permanent) in [
        (
            "test.recurring.exhausted.transient",
            "test:recurring:exhausted:transient",
            false,
        ),
        (
            "test.recurring.exhausted.permanent",
            "test:recurring:exhausted:permanent",
            true,
        ),
    ] {
        let schedule = json!({"every_seconds": 3600});
        let worker = Worker::new(
            repo.clone(),
            Arc::new(JobRegistry::new(
                vec![Box::new(FailingRecurringHandler { kind, permanent })],
                vec![RecurringKind {
                    job_kind: kind,
                    payload: json!({}),
                    schedule: schedule.clone(),
                    idempotency_key: key,
                    max_attempts: 2,
                }],
            )),
            JobConfig::default(),
        );
        worker.bootstrap().await.unwrap();
        // Due now, with one attempt already spent: the next claim is the last.
        let mut conn = db.acquire().await.unwrap();
        sqlx::query("UPDATE background_job SET run_at=now()-interval '1 second', attempts=1, finished_at=now()-interval '1 day' WHERE idempotency_key=$1")
            .bind(key).execute(&mut *conn).await.unwrap();
        drop(conn);
        let before = stored(&db, key).await;
        let mut claimed = repo
            .claim_kinds(1, worker.id(), std::time::Duration::from_secs(60), &[kind])
            .await
            .unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].attempts, claimed[0].max_attempts);
        let started = Utc::now();
        worker.process_claimed(claimed.pop().unwrap()).await;
        assert_eq!(worker.diagnostics().observable_failures(), 0);

        let after = stored(&db, key).await;
        assert_eq!(after.id, before.id);
        assert_eq!(
            after.state, "pending",
            "{kind}: a recurring row never goes terminal"
        );
        assert_eq!(
            after.attempts, 0,
            "{kind}: a fresh budget for the next occurrence"
        );
        assert_eq!(after.schedule, Some(schedule));
        assert_eq!(after.claimed_by, None);
        assert_eq!(after.lease_expires_at, None);
        assert_eq!(after.last_error.as_deref(), Some("object store down"));
        assert_eq!(
            after.finished_at, before.finished_at,
            "finished_at stays the last success"
        );
        assert!(
            after.run_at >= started + Duration::seconds(3600)
                && after.run_at <= Utc::now() + Duration::seconds(3600),
            "{kind}: rescheduled to the next occurrence, got {}",
            after.run_at
        );
        assert_eq!(
            worker.bootstrap().await.map_err(|e| e.to_string()),
            Ok(()),
            "boot reconciliation keeps the rescheduled row"
        );
        assert_eq!(stored(&db, key).await, after);
    }
}

#[db_test]
async fn exhausted_expired_recurring_lease_is_rescheduled_not_reclaimed(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const KIND: &str = "test.recurring.exhausted-lease";
    const KEY: &str = "test:recurring:exhausted-lease";
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    let mut conn = db.acquire().await.unwrap();
    sqlx::query("INSERT INTO background_job(kind,payload,state,run_at,schedule,idempotency_key,attempts,max_attempts,claimed_by,lease_expires_at) VALUES($1,'{}','running',now()-interval '1 hour',$2,$3,3,3,'crashed-worker',now()-interval '1 second')")
        .bind(KIND).bind(json!({"every_seconds": 600})).bind(KEY)
        .execute(&mut *conn).await.unwrap();
    drop(conn);
    let started = Utc::now();
    let claimed = repo
        .claim_kinds(10, "reclaimer", std::time::Duration::from_secs(60), &[KIND])
        .await
        .unwrap();
    assert!(claimed.is_empty(), "an exhausted lease is never reclaimed");
    let row = stored(&db, KEY).await;
    assert_eq!(row.state, "pending");
    assert_eq!(row.attempts, 0);
    assert_eq!(row.claimed_by, None);
    assert_eq!(row.lease_expires_at, None);
    assert_eq!(
        row.last_error.as_deref(),
        Some(bikesnest_infrastructure::EXHAUSTED_LEASE_ERROR)
    );
    assert!(row.run_at >= started + Duration::seconds(600));
}

#[db_test]
async fn conflicts_are_protected_and_live_unscheduled_lease_is_revisited(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    let now = Utc::now();
    let mut conn = db.acquire().await.unwrap();
    sqlx::query("INSERT INTO background_job(kind,payload,state,run_at,idempotency_key,claimed_by,lease_expires_at,attempts) VALUES($1,'{}','running',$2,$3,'live-worker',$4,2)")
        .bind(REVISIT_KIND).bind(now).bind(REVISIT_KEY).bind(now + Duration::minutes(10))
        .execute(&mut *conn).await.unwrap();
    let before: (chrono::DateTime<Utc>, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "SELECT run_at,lease_expires_at FROM background_job WHERE idempotency_key=$1",
    )
    .bind(REVISIT_KEY)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    drop(conn);
    let worker = Worker::new(
        repo.clone(),
        registry(
            REVISIT_KIND,
            REVISIT_KEY,
            86_400,
            Arc::new(AtomicUsize::new(0)),
        ),
        JobConfig::default(),
    );
    assert!(
        worker
            .bootstrap()
            .await
            .unwrap_err()
            .to_string()
            .contains(REVISIT_KEY)
    );
    let mut conn = db.acquire().await.unwrap();
    let live: LiveRow = sqlx::query_as(
        "SELECT state,claimed_by,schedule,attempts,run_at,lease_expires_at FROM background_job WHERE idempotency_key=$1",
    )
    .bind(REVISIT_KEY)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(live.state, "running");
    assert_eq!(live.claimed_by.as_deref(), Some("live-worker"));
    assert_eq!(live.schedule, None);
    assert_eq!(live.attempts, 2);
    assert_eq!((live.run_at, live.lease_expires_at), before);
    sqlx::query("UPDATE background_job SET state='succeeded',claimed_by=NULL,lease_expires_at=NULL,finished_at=now() WHERE idempotency_key=$1")
        .bind(REVISIT_KEY).execute(&mut *conn).await.unwrap();
    drop(conn);
    worker.bootstrap().await.unwrap();
    let revisited = stored(&db, REVISIT_KEY).await;
    assert_eq!(revisited.state, "pending");
    assert_eq!(revisited.schedule, Some(json!({"every_seconds": 86_400})));
    assert_eq!(revisited.attempts, 0);
    assert_eq!(revisited.claimed_by, None);
    assert_eq!(revisited.lease_expires_at, None);
    assert!(revisited.finished_at.is_some());

    let scheduled_running_key = "test:recurring:scheduled-running";
    let scheduled_running_schedule = json!({"every_seconds": 60});
    let mut conn = db.acquire().await.unwrap();
    sqlx::query("INSERT INTO background_job(kind,payload,state,run_at,schedule,idempotency_key,claimed_by,lease_expires_at,attempts) VALUES($1,'{}','running',$2,$3,$4,'scheduled-worker',$5,4)")
        .bind(REVISIT_KIND).bind(now + Duration::hours(3)).bind(&scheduled_running_schedule)
        .bind(scheduled_running_key).bind(now + Duration::minutes(8))
        .execute(&mut *conn).await.unwrap();
    drop(conn);
    let before_scheduled = stored(&db, scheduled_running_key).await;
    assert_eq!(
        repo.register_recurring(
            REVISIT_KIND,
            &json!({}),
            &scheduled_running_schedule,
            now,
            5,
            scheduled_running_key,
        )
        .await
        .unwrap(),
        RecurringRegistrationOutcome::RunningPreserved
    );
    let after_scheduled = stored(&db, scheduled_running_key).await;
    assert_eq!(after_scheduled.id, before_scheduled.id);
    assert_eq!(after_scheduled.state, "running");
    assert_eq!(after_scheduled.schedule, before_scheduled.schedule);
    assert_eq!(after_scheduled.claimed_by, before_scheduled.claimed_by);
    assert_eq!(
        after_scheduled.lease_expires_at,
        before_scheduled.lease_expires_at
    );
    assert_eq!(after_scheduled.attempts, before_scheduled.attempts);
    assert_eq!(after_scheduled.run_at, before_scheduled.run_at);

    let conflict_key = "test:recurring:kind-conflict";
    for (key, existing_kind, existing_payload, existing_schedule) in [
        (
            conflict_key,
            "unrelated.kind",
            json!({}),
            json!({"every_seconds": 60}),
        ),
        (
            "test:recurring:payload-conflict",
            REVISIT_KIND,
            json!({"different": true}),
            json!({"every_seconds": 60}),
        ),
        (
            "test:recurring:schedule-conflict",
            REVISIT_KIND,
            json!({}),
            json!({"every_seconds": 7}),
        ),
    ] {
        let mut conn = db.acquire().await.unwrap();
        sqlx::query("INSERT INTO background_job(kind,payload,run_at,schedule,idempotency_key,attempts,last_error) VALUES($1,$2,$3,$4,$5,2,'protected')")
            .bind(existing_kind).bind(existing_payload).bind(now).bind(existing_schedule).bind(key)
            .execute(&mut *conn).await.unwrap();
        drop(conn);
        let before_conflict = stored(&db, key).await;
        assert!(
            repo.register_recurring(
                REVISIT_KIND,
                &json!({}),
                &json!({"every_seconds": 60}),
                now,
                5,
                key
            )
            .await
            .is_err()
        );
        assert_eq!(
            stored(&db, key).await,
            before_conflict,
            "conflicting row {key} must be untouched"
        );
    }

    let second_key = "test:recurring:after-protected-conflict";
    let two = Arc::new(JobRegistry::new(
        Vec::new(),
        vec![
            RecurringKind {
                job_kind: REVISIT_KIND,
                payload: json!({}),
                schedule: json!({"every_seconds": 60}),
                idempotency_key: conflict_key,
                max_attempts: 5,
            },
            RecurringKind {
                job_kind: "test.recurring.bootstrap.after-conflict",
                payload: json!({}),
                schedule: json!({"every_seconds": 60}),
                idempotency_key: second_key,
                max_attempts: 5,
            },
        ],
    ));
    let two_worker = Worker::new(repo.clone(), two, JobConfig::default());
    assert!(two_worker.bootstrap().await.is_err());
    let mut conn = db.acquire().await.unwrap();
    let second_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM background_job WHERE idempotency_key=$1 AND schedule=$2)",
    )
    .bind(second_key)
    .bind(json!({"every_seconds": 60}))
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert!(
        second_exists,
        "one protected key must not starve later built-ins"
    );
    drop(conn);

    for (key, invalid) in [
        ("test:recurring:invalid-null", serde_json::Value::Null),
        ("test:recurring:invalid-empty", json!({})),
        ("test:recurring:invalid-zero", json!({"every_seconds": 0})),
        ("test:recurring:invalid-scalar", json!(60)),
    ] {
        assert!(
            repo.register_recurring(REVISIT_KIND, &json!({}), &invalid, now, 5, key)
                .await
                .is_err(),
            "invalid recurring schedule {invalid} must be rejected"
        );
        let mut conn = db.acquire().await.unwrap();
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM background_job WHERE idempotency_key=$1)",
        )
        .bind(key)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert!(!exists, "invalid registration must not persist a row");
    }
}

#[db_test]
async fn busy_queue_uses_bounded_bootstrap_retry_without_starving_work(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const BUSY_KIND: &str = "test.recurring.bootstrap.busy-work";
    const DEFERRED_KIND: &str = "test.recurring.bootstrap.busy-deferred";
    const DEFERRED_KEY: &str = "test:recurring:bootstrap:busy-deferred";
    let db = tx.db().await;
    let repo = SqlxJobRepository::new(db.clone());
    let now = Utc::now();
    let mut conn = db.acquire().await.unwrap();
    sqlx::query("INSERT INTO background_job(kind,payload,state,run_at,idempotency_key,claimed_by,lease_expires_at,attempts) VALUES($1,'{}','running',$2,$3,'other-worker',$4,1)")
        .bind(DEFERRED_KIND).bind(now).bind(DEFERRED_KEY).bind(now + Duration::minutes(5))
        .execute(&mut *conn).await.unwrap();
    drop(conn);
    repo.enqueue(BUSY_KIND, &json!({}), now, Some(1), None)
        .await
        .unwrap();

    let runs = Arc::new(AtomicUsize::new(0));
    let reached_target = Arc::new(tokio::sync::Notify::new());
    let registry = Arc::new(JobRegistry::new(
        vec![Box::new(BusyHandler {
            repo: repo.clone(),
            runs: runs.clone(),
            reached_target: reached_target.clone(),
        })],
        vec![RecurringKind {
            job_kind: DEFERRED_KIND,
            payload: json!({}),
            schedule: json!({"every_seconds": 60}),
            idempotency_key: DEFERRED_KEY,
            max_attempts: 5,
        }],
    ));
    let config = JobConfig {
        poll_interval: std::time::Duration::from_secs(2),
        batch_size: 1,
        ..JobConfig::default()
    };
    let worker = Worker::new(repo, registry, config);
    let diagnostics = worker.diagnostics();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(worker.run_kinds(shutdown.clone(), vec![BUSY_KIND.to_string()]));
    let reached =
        tokio::time::timeout(std::time::Duration::from_secs(1), reached_target.notified()).await;
    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .expect("worker stops after cancellation")
        .expect("worker loop does not panic");

    assert!(
        reached.is_ok() && runs.load(Ordering::SeqCst) >= 3,
        "independent busy work must continue"
    );
    assert_eq!(
        diagnostics.bootstrap_attempts(),
        1,
        "bootstrap retry waits for its own deadline even while claims stay busy"
    );
}
