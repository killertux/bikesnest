//! Opt-in Chromium coverage for the real rendered search page and htmx queue.

use std::path::PathBuf;
use std::sync::Arc;

use bikesnest_domain::ParkingType;
use bikesnest_infrastructure::{Db, FakeEmailProvider, InMemoryRateLimiter, MapConfig};
use bikesnest_test_support::{
    ParkingBuilder, TestObjectStorage, TestPasswordHasher, run_isolated_database_test, test_config,
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
fn search_state_in_real_browser() {
    run_isolated_database_test(|pool| async move {
        let db = Db::from_pool(pool);
        let mut conn = db.acquire().await.expect("scoped fixture connection");
        ParkingBuilder::new()
            .with_name("Search browser rack")
            .with_type(ParkingType::Rack)
            .with_security("cctv", 1)
            .with_security("well_lit", 1)
            .at(-33.930_000, -70.630_000)
            .create(&mut conn)
            .await
            .unwrap();
        ParkingBuilder::new()
            .with_name("Search browser indoor")
            .with_type(ParkingType::Indoor)
            .with_security("cctv", 1)
            .at(-33.930_200, -70.630_000)
            .create(&mut conn)
            .await
            .unwrap();
        ParkingBuilder::new()
            .with_name("Search browser locker")
            .with_type(ParkingType::Locker)
            .at(-33.930_400, -70.630_000)
            .create(&mut conn)
            .await
            .unwrap();
        for index in 0..20 {
            ParkingBuilder::new()
                .with_name(format!("Search browser page item {index:02}"))
                .with_type(ParkingType::Rack)
                .with_security("cctv", 1)
                .with_security("well_lit", 1)
                .at(-33.930_500 - f64::from(index) / 100_000.0, -70.630_000)
                .create(&mut conn)
                .await
                .unwrap();
        }
        drop(conn);

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback-only test server");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut config = test_config();
        config.base_url = origin.clone();
        config.static_root = root.join("web/static");
        config.map = MapConfig::MapLibre {
            style_url: format!("{origin}/test-map-style.json"),
            access_token: String::new(),
        };
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
        )
        .route(
            "/test-map-style.json",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "version": 8,
                    "sources": {},
                    "layers": []
                }))
            }),
        );
        let _server = TestServer(tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve browser app");
        }));
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("node")
                .arg(root.join("tests/browser/search-app.cjs"))
                .current_dir(root)
                .env("BIKESNEST_SEARCH_TEST_ORIGIN", origin)
                .env_remove("DATABASE_URL")
                .env_remove("TEST_DATABASE_URL")
                .output()
                .expect("run installed browser driver")
        })
        .await
        .expect("browser driver task completed");
        assert!(
            output.status.success(),
            "real-app search browser regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    });
}
