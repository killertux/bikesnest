//! The admin job-health read model against real PostgreSQL.

use bikesnest_application::{JobHealth, JobHealthReader};
use bikesnest_infrastructure::SqlxJobHealthReader;
use bikesnest_test_support::db_test;
use chrono::{Duration, Utc};
use serde_json::json;

const HEALTHY_KIND: &str = "test.health.healthy";
const LATE_KIND: &str = "test.health.late";
const STUCK_KIND: &str = "test.health.stuck";

#[db_test]
async fn recurring_rows_report_last_success_lateness_and_errors(tx: &mut TestTx) {
    let db = tx.db().await;
    let now = Utc::now();
    let mut conn = db.acquire().await.unwrap();
    // On schedule, last run succeeded an hour ago.
    sqlx::query(
        "INSERT INTO background_job (kind, payload, schedule, run_at, finished_at, idempotency_key)
         VALUES ($1, '{}', $2, $3, $4, 'test:health:healthy')",
    )
    .bind(HEALTHY_KIND)
    .bind(json!({"every_seconds": 3600}))
    .bind(now + Duration::minutes(5))
    .bind(now - Duration::hours(1))
    .execute(&mut *conn)
    .await
    .unwrap();
    // Due an hour ago, still pending, and its last occurrence failed.
    sqlx::query(
        "INSERT INTO background_job (kind, payload, schedule, run_at, attempts, last_error, idempotency_key)
         VALUES ($1, '{}', $2, $3, 2, 'job failed: boom', 'test:health:late')",
    )
    .bind(LATE_KIND)
    .bind(json!({"cron": "0 3 * * *"}))
    .bind(now - Duration::hours(1))
    .execute(&mut *conn)
    .await
    .unwrap();
    // Claimed, but the lease lapsed without a heartbeat.
    sqlx::query(
        "INSERT INTO background_job (kind, payload, schedule, state, run_at, claimed_by,
                                     lease_expires_at, heartbeat_at, started_at, attempts,
                                     idempotency_key)
         VALUES ($1, '{}', $2, 'running', $3, 'gone-worker', $4, $3, $3, 1, 'test:health:stuck')",
    )
    .bind(STUCK_KIND)
    .bind(json!({"every_seconds": 60}))
    .bind(now - Duration::minutes(10))
    .bind(now - Duration::minutes(5))
    .execute(&mut *conn)
    .await
    .unwrap();
    // A one-shot job: never listed as recurring.
    sqlx::query(
        "INSERT INTO background_job (kind, payload, run_at) VALUES ('test.health.oneshot', '{}', $1)",
    )
    .bind(now - Duration::hours(2))
    .execute(&mut *conn)
    .await
    .unwrap();
    drop(conn);

    let reader = SqlxJobHealthReader::new(db.clone());
    let rows = reader.recurring().await.unwrap();
    let find = |kind: &str| {
        rows.iter()
            .find(|r| r.kind == kind)
            .unwrap_or_else(|| panic!("{kind} listed: {rows:?}"))
    };
    assert!(rows.iter().all(|r| r.kind != "test.health.oneshot"));

    let healthy = find(HEALTHY_KIND);
    assert_eq!(healthy.health(now), JobHealth::Healthy);
    let last = healthy
        .last_success_at
        .expect("finished_at is the last success");
    assert!((last - (now - Duration::hours(1))).num_seconds().abs() <= 1);
    assert_eq!(healthy.lateness(now), None);

    let late = find(LATE_KIND);
    assert_eq!(late.health(now), JobHealth::Late);
    assert_eq!(late.last_error.as_deref(), Some("job failed: boom"));
    assert_eq!(late.attempts, 2);
    assert_eq!(late.schedule, json!({"cron": "0 3 * * *"}));
    assert!(late.lateness(now).unwrap() >= Duration::minutes(59));
    assert_eq!(late.last_success_at, None);

    assert_eq!(find(STUCK_KIND).health(now), JobHealth::Stuck);
}

#[db_test]
async fn queue_summary_counts_overdue_and_dead_lettered_one_shots(tx: &mut TestTx) {
    let db = tx.db().await;
    let now = Utc::now();
    let reader = SqlxJobHealthReader::new(db.clone());
    let before = reader.queue_summary(now).await.unwrap();

    let mut conn = db.acquire().await.unwrap();
    // Overdue (an hour past due) and not yet overdue (a minute past due).
    for run_at in [now - Duration::hours(1), now - Duration::minutes(1)] {
        sqlx::query(
            "INSERT INTO background_job (kind, payload, run_at) VALUES ('test.health.queue', '{}', $1)",
        )
        .bind(run_at)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    // Dead-lettered an hour ago, and two days ago (outside the window).
    for finished in [now - Duration::hours(1), now - Duration::days(2)] {
        sqlx::query(
            "INSERT INTO background_job (kind, payload, state, run_at, finished_at, last_error)
             VALUES ('test.health.queue', '{}', 'failed', $1, $1, 'job failed: x')",
        )
        .bind(finished)
        .execute(&mut *conn)
        .await
        .unwrap();
    }
    drop(conn);

    let after = reader.queue_summary(now).await.unwrap();
    assert_eq!(after.overdue_pending - before.overdue_pending, 1);
    assert_eq!(after.failed_last_day - before.failed_last_day, 1);
}
