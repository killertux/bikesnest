//! Real HTTP mutations stay inside the harness scope, including after panic.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bikesnest_infrastructure::{Db, FakeEmailProvider, InMemoryRateLimiter};
use bikesnest_test_support::{
    TestObjectStorage, TestPasswordHasher, pool, run_db_test, test_config,
};
use bikesnest_web::{RouterDeps, app_router_with};
use http_body_util::BodyExt;
use tower::ServiceExt;

fn scoped_router(db: Db) -> axum::Router {
    app_router_with(
        Arc::new(test_config()),
        db,
        RouterDeps {
            email: Arc::new(FakeEmailProvider::with_root(None)),
            oauth: None,
            hasher: TestPasswordHasher,
            rate_limiter: Box::new(InMemoryRateLimiter::new()),
            storage: Arc::new(TestObjectStorage::new()),
            detail_reads: None,
        },
    )
}

async fn anonymous_post(app: &axum::Router, path: &str, email: &str) -> axum::response::Response {
    let page = app
        .clone()
        .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let cookie = page.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let csrf = cookie.strip_prefix("__Host-csrf=").unwrap();
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/x-www-form-urlencoded")
                .header("cookie", &cookie)
                .body(Body::from(format!(
                    "email={email}&password=scope-password-123&csrf={csrf}"
                )))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn artifacts(conn: &mut sqlx::PgConnection, id: i64) -> Vec<i64> {
    sqlx::query_scalar(
        "SELECT ARRAY[
           (SELECT count(*) FROM users WHERE id=$1),
           (SELECT count(*) FROM authentication_identities WHERE user_id=$1),
           (SELECT count(*) FROM user_roles WHERE user_id=$1),
           (SELECT count(*) FROM email_verification_tokens WHERE user_id=$1),
           (SELECT count(*) FROM background_job WHERE mail_account_id=$1),
           (SELECT count(*) FROM sessions WHERE user_id=$1),
           (SELECT count(*) FROM audit_events WHERE actor_user_id=$1)
         ]",
    )
    .bind(id)
    .fetch_one(conn)
    .await
    .unwrap()
}

fn exercise_scope(panic_after_requests: bool) {
    let email = format!(
        "router-scope-{}-{panic_after_requests}@example.test",
        std::process::id()
    );
    let mut saved_db = None;
    let mut saved_id = None;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_db_test(async |tx| {
            let db = tx.db().await;
            let app = scoped_router(db.clone());
            let registration = anonymous_post(&app, "/register", &email).await;
            assert_eq!(registration.status(), StatusCode::SEE_OTHER);
            let login = anonymous_post(&app, "/login", &email).await;
            assert_eq!(login.status(), StatusCode::SEE_OTHER);
            let session = login.headers()["set-cookie"]
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap();
            let account = app
                .oneshot(
                    Request::builder()
                        .uri("/account")
                        .header("cookie", session)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(account.status(), StatusCode::OK);
            let body = account.into_body().collect().await.unwrap().to_bytes();
            assert!(String::from_utf8_lossy(&body).contains(&email));

            let mut conn = db.acquire().await.unwrap();
            let id: i64 = sqlx::query_scalar("SELECT id FROM users WHERE email=$1")
                .bind(&email)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
            let stored = artifacts(&mut conn, id).await;
            assert_eq!(&stored[..6], &[1, 1, 1, 1, 1, 1]);
            assert!(
                stored[6] >= 2,
                "registration and login produce durable audit"
            );
            drop(conn);

            // This separate connection is deliberately read-only: commits in
            // every real router adapter must still be invisible outside scope.
            let mut observer = pool().await.acquire().await.unwrap();
            assert_eq!(artifacts(&mut observer, id).await, vec![0; 7]);
            drop(observer);
            saved_id = Some(id);
            saved_db = Some(db);
            if panic_after_requests {
                panic!("intentional panic after successful scoped HTTP mutations");
            }
        });
    }));
    if panic_after_requests {
        let reason = outcome.expect_err("the intentional test panic must propagate");
        assert_eq!(
            reason.downcast_ref::<&str>(),
            Some(&"intentional panic after successful scoped HTTP mutations")
        );
    } else {
        outcome.unwrap();
    }
    run_db_test(async |_tx| {
        assert!(saved_db.as_ref().unwrap().acquire().await.is_err());
        let mut observer = pool().await.acquire().await.unwrap();
        assert_eq!(
            artifacts(&mut observer, saved_id.unwrap()).await,
            vec![0; 7]
        );
    });
}

#[test]
fn successful_http_mutations_are_invisible_and_rolled_back_after_scope() {
    exercise_scope(false);
}

#[test]
fn panicking_http_test_rolls_back_every_artifact_and_invalidates_handles() {
    exercise_scope(true);
}

fn rust_sources(directory: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            paths.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            paths.push(path);
        }
    }
    paths
}

#[test]
fn ordinary_adapters_cannot_escape_scopes_or_restore_legacy_fixture_commits() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pool_call = regex::Regex::new(r"\.\s*pool\s*\(").unwrap();
    for path in rust_sources(&root.join("crates/infrastructure/src")) {
        let source = std::fs::read_to_string(&path).unwrap();
        if path == root.join("crates/infrastructure/src/db.rs") {
            // Migration intentionally detaches a real pooled connection so
            // long DDL can disable request timeouts. It is not a test adapter.
            assert_eq!(pool_call.find_iter(&source).count(), 1);
            assert!(
                !source.contains("pub fn pool("),
                "pool access must remain private"
            );
            let migration = source
                .split_once("pub async fn migrate(")
                .unwrap()
                .1
                .split_once("fn pool(")
                .unwrap()
                .0;
            assert_eq!(pool_call.find_iter(migration).count(), 1);
        } else {
            assert!(
                !pool_call.is_match(&source),
                "{} must use Db::acquire so injected transactions remain authoritative",
                path.display()
            );
        }
    }
    let support = std::fs::read_to_string(root.join("crates/test-support/src/lib.rs")).unwrap();
    for removed_api in [
        "commit_fixture",
        "hold_admin_set_lock_for_process",
        "admin_set_lock",
    ] {
        assert!(
            !support.contains(removed_api),
            "legacy test-support escape {removed_api} must not return"
        );
    }
}
