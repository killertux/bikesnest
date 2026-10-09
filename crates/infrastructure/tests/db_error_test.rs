//! Real-PostgreSQL tests for the SQL error mapper.
//!
//! `classify` only ever sees a `PgDatabaseError` the driver built, and sqlx
//! gives no way to construct one by hand — so the SQLSTATE branches that matter
//! are provoked against the real server here. The pure branches (`PoolTimedOut`,
//! `RowNotFound`, the code table) are unit-tested inside `db_error.rs`.

use bikesnest_application::{ContributionError, FavoriteRepository};
use bikesnest_infrastructure::{DbFailure, SqlxFavoriteRepository, classify};
use bikesnest_test_support::{ParkingBuilder, UserBuilder, db_test};

/// Runs `sql` inside the test transaction and returns the failure it produced.
async fn failure_of(conn: &mut sqlx::PgConnection, sql: &str) -> DbFailure {
    let err = sqlx::query(sql)
        .execute(conn)
        .await
        .expect_err("statement was supposed to fail");
    classify(&err)
}

#[db_test]
async fn unique_violation_is_a_conflict_with_constraint(tx: &mut bikesnest_test_support::TestTx) {
    UserBuilder::new()
        .with_email("db-error-dup@example.com")
        .create(tx.executor())
        .await
        .expect("first insert succeeds");

    let failure = failure_of(
        tx.executor(),
        "INSERT INTO users (email) VALUES ('db-error-dup@example.com')",
    )
    .await;

    // `idx_users_email` is a unique *index*; PostgreSQL still reports it in the
    // constraint field, which is what operators need to read the log line.
    assert_eq!(
        failure,
        DbFailure::Conflict {
            constraint: Some("idx_users_email".to_string())
        }
    );
}

#[db_test]
async fn check_violation_is_invalid_with_constraint(tx: &mut bikesnest_test_support::TestTx) {
    let location = ParkingBuilder::new()
        .create(tx.executor())
        .await
        .expect("fixture location");

    // `state` is CHECK (state IN (0, 1, 2)).
    let sql = format!(
        "INSERT INTO parking_security (location_id, feature_code, state) VALUES ({}, 'cctv', 9)",
        location.id()
    );
    let failure = failure_of(tx.executor(), &sql).await;

    assert_eq!(
        failure,
        DbFailure::Invalid {
            constraint: Some("parking_security_state_check".to_string())
        }
    );
}

#[db_test]
async fn foreign_key_violation_is_invalid(tx: &mut bikesnest_test_support::TestTx) {
    let failure = failure_of(
        tx.executor(),
        "INSERT INTO favorite (user_id, location_id) VALUES (-1, -1)",
    )
    .await;

    assert_eq!(
        failure,
        DbFailure::Invalid {
            constraint: Some("favorite_user_id_fkey".to_string())
        }
    );
}

#[db_test]
async fn statement_timeout_is_unavailable(tx: &mut bikesnest_test_support::TestTx) {
    // 57014 query_canceled. `SET LOCAL` scopes the timeout to this transaction,
    // which the harness rolls back.
    sqlx::query("SET LOCAL statement_timeout = '50ms'")
        .execute(tx.executor())
        .await
        .expect("set statement_timeout");

    let failure = failure_of(tx.executor(), "SELECT pg_sleep(3)").await;

    assert_eq!(failure, DbFailure::Unavailable);
}

/// End-to-end through a repository: the mapper turns an FK rejection into the
/// feature's own error instead of an opaque `Internal`. (A review of a missing
/// location no longer reaches the FK: the review repository locks the location
/// first and reports `NotFound`. A favorite still relies on the FK alone.)
#[db_test]
async fn repository_maps_a_rejected_write_off_internal(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.expect("test transaction connection");
    let author = UserBuilder::new()
        .with_email("db-error-repository@example.com")
        .create(&mut *conn)
        .await
        .expect("fixture author");
    drop(conn);

    let repo = SqlxFavoriteRepository::new(db);
    let err = repo
        .toggle(author.id, -1)
        .await
        .expect_err("the favorite FK cannot be satisfied");

    assert!(
        matches!(err, ContributionError::InvalidField(_)),
        "expected InvalidField, got {err:?}"
    );
}
