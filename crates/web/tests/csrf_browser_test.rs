//! Opt-in Chromium regression against the real rendered Axum application.
//!
//! This uses the same rollback harness as `#[db_test]`; the explicit wrapper
//! preserves `#[ignore]`, which the current attribute macro does not forward.

use std::path::PathBuf;
use std::sync::Arc;

use bikesnest_infrastructure::{FakeEmailProvider, InMemoryRateLimiter};
use bikesnest_test_support::{TestObjectStorage, TestPasswordHasher, test_config};
use bikesnest_web::{RouterDeps, app_router_with};

struct TestServer(tokio::task::JoinHandle<()>);

impl Drop for TestServer {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[test]
#[ignore = "requires Node, Playwright Chromium and explicit disposable TEST_DATABASE_URL"]
fn csrf_lifecycle_in_real_browser() {
    bikesnest_test_support::run_db_test(async move |tx| {
        let db = tx.db().await;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the loopback-only test server");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut config = test_config();
        config.base_url = origin.clone();
        config.static_root = root.join("web/static");
        config.jobs.enabled = false;
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
            axum::serve(listener, app)
                .await
                .expect("serve the isolated browser-test application");
        }));
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("node")
                .arg(root.join("tests/browser/csrf-app.cjs"))
                .current_dir(root)
                .env("BIKESNEST_CSRF_TEST_ORIGIN", origin)
                // The browser driver needs neither database credentials nor
                // application/provider environment configuration.
                .env_remove("DATABASE_URL")
                .env_remove("TEST_DATABASE_URL")
                .output()
                .expect("run the installed Node browser driver")
        })
        .await
        .expect("browser driver task completed");
        assert!(
            output.status.success(),
            "real-app CSRF browser regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    });
}
