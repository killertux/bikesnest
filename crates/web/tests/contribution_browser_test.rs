//! Opt-in Chromium coverage for the rendered contribution forms.

use std::path::PathBuf;
use std::sync::Arc;

use bikesnest_application::SessionStore;
use bikesnest_domain::{CsrfToken, SessionId};
use bikesnest_infrastructure::{
    Db, FakeEmailProvider, InMemoryRateLimiter, MapConfig, SqlxSessionStore,
};
use bikesnest_test_support::{
    ParkingBuilder, TestObjectStorage, TestPasswordHasher, UserBuilder, run_isolated_database_test,
    test_config,
};
use bikesnest_web::{RouterDeps, app_router_with};

struct TestServer(tokio::task::JoinHandle<()>);

impl Drop for TestServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[test]
#[ignore = "requires Node, Playwright Chromium and explicit disposable TEST_DATABASE_URL"]
fn contribution_forms_in_real_browser() {
    run_isolated_database_test(|pool| async move {
        let db = Db::from_pool(pool);
        let mut conn = db.acquire().await.expect("fixture connection");
        let location = ParkingBuilder::new()
            .with_name("B13 editable rack")
            .create(&mut conn)
            .await
            .unwrap();
        let location_id = location.id();
        let user = UserBuilder::new()
            .with_email("b13-browser@example.com")
            .create(&mut *conn)
            .await
            .unwrap();
        sqlx::query("UPDATE users SET account_state='ACTIVE', email_verified_at=now() WHERE id=$1")
            .bind(user.id.0)
            .execute(&mut *conn)
            .await
            .unwrap();
        drop(conn);
        let token = SessionId::new([13; 32]);
        SqlxSessionStore::new(db.clone())
            .create(
                user.id,
                &token,
                &CsrfToken::new([14; 32]),
                chrono::Utc::now(),
            )
            .await
            .unwrap();

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback-only browser server");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut config = test_config();
        config.base_url = origin.clone();
        config.static_root = root.join("web/static");
        config.jobs.enabled = false;
        config.map = MapConfig::MapLibre {
            style_url: format!("{origin}/b13-test-style.json"),
            access_token: String::new(),
        };
        let assertion_db = db.clone();
        let app = app_router_with(
            Arc::new(config),
            db,
            RouterDeps {
                email: Arc::new(FakeEmailProvider::with_root(None)),
                oauth: None,
                hasher: TestPasswordHasher,
                rate_limiter: Box::new(InMemoryRateLimiter::new()),
                storage: Arc::new(TestObjectStorage::new()),
                detail_reads: None,
            },
        );
        let _server = TestServer(tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve browser app");
        }));
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("node")
                .arg(root.join("tests/browser/contribution-app.cjs"))
                .current_dir(root)
                .env("BIKESNEST_CONTRIBUTION_TEST_ORIGIN", origin)
                .env("BIKESNEST_CONTRIBUTION_TEST_ID", location_id.to_string())
                .env("BIKESNEST_CONTRIBUTION_TEST_SESSION", token.to_hex())
                .env_remove("DATABASE_URL")
                .env_remove("TEST_DATABASE_URL")
                .output()
                .expect("run installed browser driver")
        })
        .await
        .expect("browser driver task completed");
        assert!(
            output.status.success(),
            "real contribution browser regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        let mut conn = assertion_db.acquire().await.expect("assertion connection");
        let created: (String, Option<String>, String, i64) = sqlx::query_as(
            "SELECT cost_kind, description, timezone, version
             FROM parking_location WHERE name = 'B13 browser new rack'",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("browser-created location");
        assert_eq!(created.0, "free");
        assert_eq!(
            created.1.as_deref(),
            Some("Kept through rejected submissions")
        );
        assert_eq!(created.2, "America/Sao_Paulo");
        assert_eq!(created.3, 1);
        let (created_cctv,): (i16,) = sqlx::query_as(
            "SELECT state FROM parking_security
             WHERE location_id = (SELECT id FROM parking_location WHERE name = 'B13 browser new rack')
               AND feature_code = 'cctv'",
        )
        .fetch_one(&mut *conn)
        .await
        .expect("browser-created security value");
        assert_eq!(
            created_cctv, 2,
            "keyboard-selected definitive no is persisted"
        );

        let public: (String, String, i64) =
            sqlx::query_as("SELECT name, cost_kind, version FROM parking_location WHERE id = $1")
                .bind(location_id)
                .fetch_one(&mut *conn)
                .await
                .expect("original public listing");
        assert_eq!(
            public,
            ("B13 editable rack".to_string(), "free".to_string(), 1)
        );
        let proposal: (String, String, serde_json::Value) = sqlx::query_as(
            "SELECT kind, status, proposed FROM parking_proposal
             WHERE location_id = $1 ORDER BY id DESC LIMIT 1",
        )
        .bind(location_id)
        .fetch_one(&mut *conn)
        .await
        .expect("browser-created edit proposal");
        assert_eq!(proposal.0, "edit_details");
        assert_eq!(proposal.1, "PENDING");
        assert_eq!(proposal.2["cost"]["kind"], "paid");
        assert_eq!(proposal.2["cost"]["cents"], 500);
        assert_eq!(proposal.2["security"][2][1], 2);
    });
}
