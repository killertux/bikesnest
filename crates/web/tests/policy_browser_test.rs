//! Actual rendered terms journeys; fixtures never publish real policies.

use std::path::PathBuf;
use std::sync::Arc;

use bikesnest_application::SessionStore;
use bikesnest_domain::{CsrfToken, SessionId};
use bikesnest_infrastructure::{Db, FakeEmailProvider, InMemoryRateLimiter, SqlxSessionStore};
use bikesnest_test_support::{
    TestObjectStorage, TestPasswordHasher, UserBuilder, run_isolated_database_test, test_config,
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
fn terms_versions_and_acknowledgements_in_real_browser() {
    run_isolated_database_test(|pool| async move {
        let db = Db::from_pool(pool);
        let mut conn = db.acquire().await.expect("fixture connection");
        let now: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        let current_at = now - chrono::Duration::days(1);
        let future_at = now + chrono::Duration::days(365);
        let mut ids = serde_json::Map::new();
        for (version, effective, superseded) in [
            (
                "browser-old",
                now - chrono::Duration::days(2),
                Some(current_at),
            ),
            ("browser-current", current_at, Some(future_at)),
            ("browser-future", future_at, None),
        ] {
            for locale in ["en", "pt-BR"] {
                for kind in ["terms", "privacy", "cookies"] {
                    let content =
                        format!("## {kind} {version} {locale}\n\nOffline browser fixture.");
                    let id: i64 = sqlx::query_scalar(
                        "INSERT INTO policy_version
                         (kind, locale, version, effective_at, superseded_at, content,
                          requires_acknowledgement)
                         VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id",
                    )
                    .bind(kind)
                    .bind(locale)
                    .bind(version)
                    .bind(effective)
                    .bind(superseded)
                    .bind(content)
                    .bind(kind == "terms")
                    .fetch_one(&mut *conn)
                    .await
                    .unwrap();
                    if kind == "terms" {
                        ids.insert(format!("{version}:{locale}"), id.into());
                    }
                }
            }
        }
        let mut users = Vec::new();
        for (index, locale) in ["en", "pt-BR"].into_iter().enumerate() {
            let user = UserBuilder::new()
                .with_email(format!("b15-browser-existing-{locale}@example.com"))
                .create(&mut *conn)
                .await
                .unwrap();
            sqlx::query(
                "UPDATE users SET account_state='ACTIVE', email_verified_at=now(), locale=$2 WHERE id=$1",
            )
            .bind(user.id.0)
            .bind(locale)
            .execute(&mut *conn)
            .await
            .unwrap();
            users.push((user.id, SessionId::new([31 + index as u8; 32])));
        }
        drop(conn);
        for (user, token) in &users {
            SqlxSessionStore::new(db.clone())
                .create(*user, token, &CsrfToken::new([41; 32]), now)
                .await
                .unwrap();
        }
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback-only browser server");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let mut config = test_config();
        config.base_url = origin.clone();
        config.static_root = root.join("web/static");
        config.jobs.enabled = false;
        config.policy.acknowledgement_enabled = true;
        let app = app_router_with(
            Arc::new(config),
            db.clone(),
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
            axum::serve(listener, app).await.expect("browser server");
        }));
        let inputs = serde_json::json!({
            "origin": origin,
            "ids": ids,
            "futureEffectiveAt": future_at.to_rfc3339(),
            "sessions": [users[0].1.to_hex(), users[1].1.to_hex()],
        });
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new("node")
                .arg(root.join("tests/browser/policy-app.cjs"))
                .current_dir(root)
                .env("BIKESNEST_POLICY_BROWSER_INPUT", inputs.to_string())
                .env_remove("DATABASE_URL")
                .env_remove("TEST_DATABASE_URL")
                .output()
                .expect("run installed browser driver")
        })
        .await
        .expect("browser task joined");
        assert!(
            output.status.success(),
            "terms browser regression failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let mut conn = db.acquire().await.unwrap();
        let proofs: Vec<(String, String, String, String, String, bool)> = sqlx::query_as(
            "SELECT u.email, a.terms_version, a.shown_locale, a.source, p.locale,
                    a.acknowledged_at >= $1
             FROM terms_acknowledgement a JOIN users u ON u.id=a.user_id
             JOIN policy_version p ON p.id=a.policy_version_id ORDER BY u.email",
        )
        .bind(now)
        .fetch_all(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            proofs.len(),
            4,
            "exactly two signup and two explicit acknowledgements"
        );
        for (email, version, locale, source, document_locale, timestamp_valid) in proofs {
            assert_eq!(version, "browser-current");
            let expected_locale = if email.contains("pt-br") {
                "pt-BR"
            } else {
                "en"
            };
            assert_eq!(locale, expected_locale);
            assert_eq!(document_locale, expected_locale);
            assert_eq!(
                source,
                if email.contains("existing") {
                    "in_product"
                } else {
                    "signup"
                }
            );
            assert!(timestamp_valid);
        }
        let future_acks: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM terms_acknowledgement WHERE terms_version <> 'browser-current'",
        )
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(
            future_acks, 0,
            "viewing old/future documents cannot acknowledge them"
        );
        let consent_records: i64 = sqlx::query_scalar("SELECT count(*) FROM consent_record")
            .fetch_one(&mut *conn)
            .await
            .unwrap();
        assert_eq!(
            consent_records, 0,
            "terms acknowledgement must not manufacture blanket privacy consent"
        );
        for (user, _) in users {
            for version in ["browser-current", "browser-future"] {
                let count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM terms_notice_presentation WHERE user_id=$1 AND terms_version=$2",
                )
                .bind(user.0)
                .bind(version)
                .fetch_one(&mut *conn)
                .await
                .unwrap();
                assert_eq!(count, 1, "repeat notice GET is idempotent for {version}");
            }
        }
    });
}
