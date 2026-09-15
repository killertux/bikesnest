//! Observability tests: request trace logging never leaks sensitive
//! headers. The `TraceLayer` records only method/path/status/latency, so we
//! attach a capturing tracing subscriber, issue a request carrying a cookie, an
//! authorization header and a CSRF token, and assert none of them reach the log.

use axum::body::Body;
use axum::http::Request;
use bikesnest_test_support::db_test;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;
use tracing::Subscriber;
use tracing::field::{Field, Visit};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;

use bikesnest_infrastructure::Db;
use bikesnest_web::{RouterDeps, app_router_with};

async fn test_app(db: Db) -> axum::Router {
    let config = std::sync::Arc::new(bikesnest_test_support::test_config());
    let deps = RouterDeps {
        email: std::sync::Arc::new(bikesnest_infrastructure::FakeEmailProvider::with_root(None)),
        oauth: None,
        hasher: bikesnest_test_support::TestPasswordHasher,
        rate_limiter: Box::new(bikesnest_infrastructure::InMemoryRateLimiter::new()),
        storage: std::sync::Arc::new(bikesnest_test_support::TestObjectStorage::new()),
        detail_reads: None,
    };
    app_router_with(config, db, deps)
}

/// Collects every span-field and event-field value into a shared buffer.
#[derive(Default)]
struct CaptureLayer {
    lines: Arc<Mutex<Vec<String>>>,
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber,
{
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut out = Vec::new();
        attrs.record(&mut FieldGrabber(&mut out));
        self.lines.lock().unwrap().extend(out);
    }

    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut out = Vec::new();
        event.record(&mut FieldGrabber(&mut out));
        self.lines.lock().unwrap().extend(out);
        // Include the event target so assertions can match on the span context.
        self.lines
            .lock()
            .unwrap()
            .push(event.metadata().target().to_string());
    }
}

struct FieldGrabber<'a>(&'a mut Vec<String>);

impl Visit for FieldGrabber<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push(format!("{}={:?}", field.name(), value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.push(format!("{}={}", field.name(), value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.push(format!("{}={}", field.name(), value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push(format!("{}={}", field.name(), value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.push(format!("{}={}", field.name(), value));
    }
}

#[db_test]
async fn request_logs_never_record_sensitive_headers(tx: &mut bikesnest_test_support::TestTx) {
    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let subscriber = tracing_subscriber::registry().with(CaptureLayer {
        lines: lines.clone(),
    });
    let app = test_app(tx.db().await).await;

    let secret = "csrf-secret-value-12345";
    let guard = tracing::subscriber::set_default(subscriber);
    let res = app
        .oneshot(
            Request::builder()
                .uri("/login")
                .header("Accept-Language", "en")
                .header("cookie", "session=abc123; __Host-session=def")
                .header("authorization", "Bearer token-value")
                .header("x-csrf-token", secret)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    drop(guard);

    assert_eq!(res.status(), 200, "login page renders");

    let captured = lines.lock().unwrap();
    let all = captured.join("\n");
    assert!(
        !all.to_lowercase().contains("cookie"),
        "cookie header leaked into request logs:\n{all}"
    );
    assert!(
        !all.to_lowercase().contains("authorization"),
        "authorization header leaked into request logs:\n{all}"
    );
    assert!(
        !all.contains(secret),
        "csrf token leaked into request logs:\n{all}"
    );
    assert!(
        !all.to_lowercase().contains("session="),
        "session value leaked into request logs:\n{all}"
    );
}
