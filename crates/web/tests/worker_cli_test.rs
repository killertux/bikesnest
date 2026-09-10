//! Process-boundary checks for the worker-only command.

use bikesnest_test_support::run_isolated_database_test;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

fn isolated_url(database: &str) -> String {
    let base = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let (address, query) = base
        .split_once('?')
        .map_or((base.as_str(), None), |parts| (parts.0, Some(parts.1)));
    let (prefix, _) = address
        .rsplit_once('/')
        .expect("TEST_DATABASE_URL must contain a database path");
    let query = query.map(|value| {
        value
            .split('&')
            .map(|option| {
                if option.starts_with("dbname=") {
                    format!("dbname={database}")
                } else {
                    option.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("&")
    });
    match query {
        Some(query) => format!("{prefix}/{database}?{query}"),
        None => format!("{prefix}/{database}"),
    }
}

struct TempWorkingDirectory(std::path::PathBuf);

impl TempWorkingDirectory {
    fn create() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "bikesnest-worker-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).expect("create worker test directory");
        std::fs::write(path.join(".env"), []).expect("create dotenv traversal sentinel");
        Self(path)
    }
}

struct ChildGuard(Option<Child>);

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn child_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("child guard is armed")
    }

    fn id(&self) -> u32 {
        self.0.as_ref().expect("child guard is armed").id()
    }

    fn disarm(&mut self, status: ExitStatus) -> ExitStatus {
        self.0.take();
        status
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for TempWorkingDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn base_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_bikesnest-web"));
    command
        .env_clear()
        .env("APP_ENV", "development")
        .env("EMAIL_PROVIDER", "fake")
        .env("S3_ENDPOINT", "http://127.0.0.1:9")
        .env("S3_ACCESS_KEY", "worker-test")
        .env("S3_SECRET_KEY", "worker-test")
        .env("S3_BUCKET", "worker-test")
        .env("S3_REGION", "us-east-1")
        .env("RUST_LOG", "error");
    command
}

#[test]
fn worker_only_uses_owned_database_and_does_not_bind_http() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&pool)
            .await
            .expect("read isolated database name");
        let database_url = isolated_url(&database);
        let working_directory = TempWorkingDirectory::create();
        let mut child = Command::new(env!("CARGO_BIN_EXE_bikesnest-web"));
        child
            .arg("worker")
            .env_clear()
            .current_dir(&working_directory.0)
            .env("APP_ENV", "development")
            .env("DATABASE_URL", database_url)
            .env("EMAIL_PROVIDER", "fake")
            // If the worker command tried to start HTTP, this invalid address
            // would make the process fail before the timeout sends SIGTERM.
            .env("BIND_ADDR", "not-a-socket-address")
            .env("S3_ENDPOINT", "http://127.0.0.1:9")
            .env("S3_ACCESS_KEY", "worker-test")
            .env("S3_SECRET_KEY", "worker-test")
            .env("S3_BUCKET", "worker-test")
            .env("S3_REGION", "us-east-1")
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut child = ChildGuard::new(child.spawn().expect("start worker-only subprocess"));

        let ready_by = Instant::now() + Duration::from_secs(10);
        loop {
            let registered: i64 = sqlx::query_scalar("SELECT count(*) FROM background_job")
                .fetch_one(&pool)
                .await
                .expect("read worker bootstrap marker");
            if registered > 0 {
                break;
            }
            assert!(Instant::now() < ready_by, "worker bootstrap timed out");
            assert!(
                child.child_mut().try_wait().expect("poll worker").is_none(),
                "worker exited before bootstrap"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let signal_status = Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .expect("signal worker-only subprocess");
        assert!(signal_status.success(), "SIGTERM command failed");

        let exit_by = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = child.child_mut().try_wait().expect("poll worker shutdown") {
                break child.disarm(status);
            }
            if Instant::now() >= exit_by {
                panic!("worker shutdown exceeded hard test bound");
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };

        assert!(
            status.success(),
            "worker-only process returned a failure status"
        );
    });
}

#[test]
fn child_guard_kills_and_reaps_during_early_unwind() {
    let mut pid = None;
    let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("start cleanup sentinel child");
        pid = Some(child.id());
        let _guard = ChildGuard::new(child);
        panic!("synthetic early failure");
    }));
    assert!(unwind.is_err());

    let status = Command::new("kill")
        .args(["-0", &pid.expect("sentinel pid").to_string()])
        .stderr(Stdio::null())
        .status()
        .expect("probe sentinel child");
    assert!(!status.success(), "child survived guard unwinding");
}

#[test]
fn worker_config_is_validated_before_database_connection() {
    let working_directory = TempWorkingDirectory::create();
    let mut command = base_command();
    let output = command
        .current_dir(&working_directory.0)
        .arg("worker")
        .env("DATABASE_URL", "postgres://127.0.0.1:1/not-reached")
        .env("JOBS_BATCH_SIZE", "0")
        .output()
        .expect("run invalid worker configuration");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid background-job configuration"));
    assert!(!stderr.contains("database connection error"));
}
