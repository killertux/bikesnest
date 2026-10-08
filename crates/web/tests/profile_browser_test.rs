//! Opt-in Chromium coverage for the real cyclist-first parking profile.

use std::path::PathBuf;
use std::sync::Arc;

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
fn cyclist_profile_in_real_browser() {
    run_isolated_database_test(|pool| async move {
        let db = Db::from_pool(pool);
        let mut conn = db.acquire().await.expect("scoped fixture connection");
        let location = ParkingBuilder::new()
            .with_name("B12 browser profile")
            .with_security("cctv", 1)
            .with_security("well_lit", 1)
            .create(&mut conn)
            .await
            .unwrap();
        for (position, key) in [(0, "b12/photo-1.jpg"), (1, "b12/photo-2.jpg")] {
            sqlx::query(
                "INSERT INTO parking_photo (location_id, storage_key, thumbnail_key, content_type, alt, position, moderation_state) VALUES ($1, $2, $3, 'image/jpeg', $4, $5, 'APPROVED')",
            )
            .bind(location.id())
            .bind(key)
            .bind(key)
            .bind(format!("B12 fixture photo {position}"))
            .bind(position)
            .execute(&mut *conn)
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
        config.jobs.enabled = false;
        config.map = MapConfig::MapLibre {
            style_url: format!("{origin}/b12-test-style.json"),
            access_token: String::new(),
        };
        let storage = Arc::new(TestObjectStorage::new());
        storage.seed("b12/photo-1.jpg", b"fixture", "image/jpeg");
        storage.seed("b12/photo-2.jpg", b"fixture", "image/jpeg");
        let app = app_router_with(
            Arc::new(config),
            db,
            RouterDeps {
                email: Arc::new(FakeEmailProvider::with_root(None)),
                oauth: None,
                hasher: TestPasswordHasher,
                rate_limiter: Box::new(InMemoryRateLimiter::new()),
                storage,
                detail_reads: None,
            },
        );
        let _server = TestServer(tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve browser app");
        }));
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("node")
                .arg(root.join("tests/browser/profile-app.cjs"))
                .current_dir(root)
                .env("BIKESNEST_PROFILE_TEST_ORIGIN", origin)
                .env("BIKESNEST_PROFILE_TEST_ID", location.id().to_string())
                .env_remove("DATABASE_URL")
                .env_remove("TEST_DATABASE_URL")
                .output()
                .expect("run installed browser driver")
        })
        .await
        .expect("browser driver task completed");
        assert!(
            output.status.success(),
            "real profile browser regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    });
}
