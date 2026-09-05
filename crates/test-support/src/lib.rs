//! Shared test infrastructure for BikesNest.
//!
//! - ONE multi-threaded tokio runtime shared by all `#[db_test]` tests
//!   (avoids creating a runtime + pool + migrations per test).
//! - ONE real PostgreSQL connection pool, migrated once.
//! - Transaction-per-test with automatic rollback.
//! - Explicit SAVEPOINT helper for nested transactional behavior.
//! - Domain-rich builders.

use sqlx::postgres::{PgPoolOptions, Postgres};
use sqlx::{PgPool, Transaction};
use tokio::sync::OnceCell;

/// Re-exported so test crates only need `bikesnest_test_support` in scope.
pub use bikesnest_test_macros::db_test;

pub mod object_storage;
pub use object_storage::TestObjectStorage;

// ---------------------------------------------------------------------------
// Shared runtime + pool
// ---------------------------------------------------------------------------

/// Single multi-threaded runtime for the entire test suite.
fn shared_runtime() -> &'static tokio::runtime::Runtime {
    static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .expect("test-support: failed to build shared tokio runtime")
    })
}

/// Single connection pool, migrated exactly once per process.
fn shared_pool() -> &'static OnceCell<PgPool> {
    static POOL: OnceCell<PgPool> = OnceCell::const_new();
    &POOL
}

/// Installs a `tracing` subscriber that writes to the test harness's captured
/// output (`with_test_writer()`), so it only shows up under `--nocapture` or
/// for a failing test), filtered by `RUST_LOG` (default `warn`).
///
/// Without this, every `tracing::error!`/`warn!` a repository logs through
/// [`bikesnest_infrastructure::classify_and_log`] is silently dropped: no
/// subscriber means no destination, not "printed and ignored". `try_init` +
/// `OnceLock` make this safe to call once per test (via [`run_db_test`]) even
/// though every test in the binary shares the same process.
pub fn init_test_tracing() {
    static INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    INIT.get_or_init(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_test_writer()
            .try_init();
    });
}

fn database_url() -> String {
    std::env::var("TEST_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .unwrap_or_else(|_| "postgres://bikesnest:bikesnest@localhost:5432/bikesnest".to_string())
}

/// The configuration the HTTP tests build their router from: a development
/// config with every provider on its fake, pointed at the test database. Tests
/// override individual fields rather than touching the process environment.
pub fn test_config() -> bikesnest_infrastructure::Config {
    bikesnest_infrastructure::Config::for_tests(database_url())
}

async fn connect_and_migrate() -> PgPool {
    let pool = PgPoolOptions::new()
        .max_connections(25)
        .connect(&database_url())
        .await
        .expect("test-support: cannot connect to Postgres (run `docker compose up -d`)");

    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .expect("test-support: migrations failed");

    pool
}

// ---------------------------------------------------------------------------
// Test transaction
// ---------------------------------------------------------------------------

/// An open PostgreSQL transaction for one test.
///
/// Dropping it rolls back (sqlx `Transaction` does this in its own `Drop`),
/// so tests are isolated without cleanup logic.
pub struct TestTx {
    tx: Option<Transaction<'static, Postgres>>,
    pool: PgPool,
    scoped: Option<bikesnest_infrastructure::Db>,
}

impl TestTx {
    /// Inject this test's outer transaction into real repositories. Call before
    /// seeding; use `db.acquire()` for fixtures and pass `db.clone()` to adapters.
    /// Repository `begin`/`commit` then establish/release SQLx savepoints. The
    /// harness rolls back the outer transaction, including on a test panic.
    pub async fn db(&mut self) -> bikesnest_infrastructure::Db {
        if let Some(db) = &self.scoped {
            return db.clone();
        }
        let mut outer = self.tx.take().expect("test transaction available");
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
            .execute(&mut *outer)
            .await
            .expect("call tx.db() before fixture queries; snapshot tests need repeatable read");
        let db = bikesnest_infrastructure::Db::from_transaction(outer);
        self.scoped = Some(db.clone());
        db
    }

    async fn rollback(&mut self) {
        if let Some(db) = self.scoped.take() {
            db.rollback_scope()
                .await
                .expect("rollback scoped test transaction");
        }
        if let Some(tx) = self.tx.take() {
            tx.rollback().await.expect("rollback test transaction");
        }
    }
    /// Opens a named SAVEPOINT on the test transaction.
    ///
    /// End it explicitly with [`Savepoint::commit`] (RELEASE) or
    /// [`Savepoint::rollback`] (ROLLBACK TO). A dropped, still-open savepoint
    /// is harmless: the outer transaction rollback discards it anyway.
    pub async fn savepoint(&mut self) -> Savepoint<'_> {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            "__bikesnest_sp_{}",
            N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        sqlx::query(&format!("SAVEPOINT {name}"))
            .execute(self.executor())
            .await
            .expect("SAVEPOINT failed");
        Savepoint {
            conn: self.executor(),
            name,
        }
    }

    /// The transaction connection as an executor for SQLx queries.
    pub fn executor(&mut self) -> &mut sqlx::PgConnection {
        self.tx.as_mut().expect("transaction still open")
    }

    /// Commits the current transaction and opens a fresh one.
    ///
    /// Use case: read-model tests whose queries run on *other* pool
    /// connections (they cannot see uncommitted rows of this transaction).
    /// The test commits a tagged fixture, asserts against the readers, then
    /// deletes the fixture rows (by tag) via the pool. The fresh transaction
    /// the harness opened is simply rolled back at test end.
    pub async fn commit_fixture(&mut self) {
        assert!(
            self.scoped.is_none(),
            "scoped repository tests must never commit fixtures"
        );
        if let Some(tx) = self.tx.take() {
            tx.commit().await.expect("commit test fixture");
        }
        self.tx = Some(
            self.pool
                .begin()
                .await
                .expect("begin tx after fixture commit"),
        );
    }
}

/// A named SAVEPOINT opened inside a [`TestTx`].
pub struct Savepoint<'a> {
    conn: &'a mut sqlx::PgConnection,
    name: String,
}

impl Savepoint<'_> {
    /// Connection executor for queries inside the savepoint.
    pub fn executor(&mut self) -> &mut sqlx::PgConnection {
        self.conn
    }

    /// `RELEASE SAVEPOINT` — simulates an application transaction committing
    /// inside the test transaction; the row survives within the test tx.
    pub async fn commit(self) {
        sqlx::query(&format!("RELEASE SAVEPOINT {}", self.name))
            .execute(self.conn)
            .await
            .expect("RELEASE SAVEPOINT failed");
    }

    /// `ROLLBACK TO SAVEPOINT` — undoes everything done inside the savepoint.
    pub async fn rollback(self) {
        sqlx::query(&format!("ROLLBACK TO SAVEPOINT {}", self.name))
            .execute(self.conn)
            .await
            .expect("ROLLBACK TO SAVEPOINT failed");
    }
}

// ---------------------------------------------------------------------------
// Runner used by #[db_test]
// ---------------------------------------------------------------------------

/// Clone of the shared, migrated pool (for wiring routers in HTTP tests).
/// Await this inside a `#[db_test]` body — never `block_on` there.
pub async fn pool() -> PgPool {
    shared_pool().get_or_init(connect_and_migrate).await.clone()
}

/// Lock id for the system-wide ADMIN set. Arbitrary but fixed; it only has to
/// be unique among this suite's advisory locks.
const ADMIN_SET_LOCK: i64 = 0x62_69_6b_65_00_01;

/// A transaction holding the shared lock on the system's ADMIN set.
///
/// "Never zero administrators" is a property of the whole `user_roles` table,
/// so a test that needs to be the *only* admin has to exclude every other
/// writer — including the other test binaries running against the same
/// database. `pg_advisory_xact_lock` reaches across processes and is released
/// when the returned transaction is dropped, so a panicking test cannot wedge
/// the suite. Hold it for as long as the exclusive state must last.
///
/// Every test that creates or removes an ADMIN row must take this lock, or the
/// exclusion is one-sided: see [`hold_admin_set_lock_for_process`] for the
/// fixture-helper side.
pub async fn admin_set_lock(pool: &PgPool) -> Transaction<'static, Postgres> {
    let mut tx = pool.begin().await.expect("begin admin-set lock");
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(ADMIN_SET_LOCK)
        .execute(&mut *tx)
        .await
        .expect("take admin-set lock");
    tx
}

/// Claims the ADMIN-set lock for the rest of this test process.
///
/// The counterpart to [`admin_set_lock`], for fixture helpers that grant ADMIN
/// and then need the row to stay put for the remainder of their test. A
/// transaction-scoped lock cannot express that (the helper returns long before
/// its caller is done), so this takes a *session*-scoped lock on a dedicated
/// connection and keeps it for the process. Idempotent: the first caller takes
/// it, every later one returns immediately.
pub async fn hold_admin_set_lock_for_process(pool: &PgPool) {
    static HELD: OnceCell<()> = OnceCell::const_new();
    HELD.get_or_init(|| async {
        let mut conn = pool.acquire().await.expect("acquire admin-set lock conn");
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(ADMIN_SET_LOCK)
            .execute(&mut *conn)
            .await
            .expect("take process admin-set lock");
        // Leak the connection so the session — and with it the lock — outlives
        // this call. It is one connection out of the pool for the process.
        std::mem::forget(conn);
    })
    .await;
}

/// A transaction allowed to mutate `audit_events`.
///
/// The table is append-only (migration 0019): the trigger refuses every UPDATE
/// and DELETE unless the transaction sets `app.audit_purge`, which the
/// production erasure and retention-purge paths do. A test cleaning up the
/// audit rows its own fixture wrote is the same kind of sanctioned mutation, so
/// it says so the same way. Run the DELETE on the returned transaction and
/// commit it.
pub async fn audit_mutation_tx(pool: &PgPool) -> Transaction<'static, Postgres> {
    let mut tx = pool.begin().await.expect("begin audit mutation");
    sqlx::query("SET LOCAL app.audit_purge = 'on'")
        .execute(&mut *tx)
        .await
        .expect("announce audit mutation");
    tx
}

/// Entry point behind `#[db_test]`: runs `f` on the shared runtime with a
/// fresh transaction; the transaction rolls back afterwards no matter what.
pub fn run_db_test(f: impl AsyncFnOnce(&mut TestTx)) {
    init_test_tracing();
    let mut tx = shared_runtime().block_on(async {
        let pool = shared_pool().get_or_init(connect_and_migrate).await;
        TestTx {
            tx: Some(pool.begin().await.expect("begin test transaction")),
            pool: pool.clone(),
            scoped: None,
        }
    });
    // Keep the owner outside the future so we can await rollback even when the
    // test panics. Invalidate Db clones before returning control to the harness.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        shared_runtime().block_on(f(&mut tx));
    }));
    shared_runtime().block_on(tx.rollback());
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

// ---------------------------------------------------------------------------
// Builders
// ---------------------------------------------------------------------------

/// Builder: creates `users` rows and returns domain entities.
pub struct UserBuilder {
    email: String,
    display_name: Option<String>,
}

impl Default for UserBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl UserBuilder {
    pub fn new() -> Self {
        Self {
            email: "user@example.com".to_string(),
            display_name: None,
        }
    }

    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        // Normalize through the domain type so stored data matches it.
        self.email = bikesnest_domain::UserEmail::parse(&email.into())
            .expect("builder email is valid")
            .to_string();
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.display_name = Some(name.into());
        self
    }

    /// Inserts the row using the test transaction; returns the domain `User`.
    ///
    /// Runtime query (not `query!`) so the workspace builds without
    /// `DATABASE_URL` at compile time; compile-time checked macros arrive
    /// with the M1 schema work (with `.env` + offline cache).
    pub async fn create<'e, E>(&self, exec: E) -> Result<bikesnest_domain::User, sqlx::Error>
    where
        E: sqlx::Executor<'e, Database = Postgres>,
    {
        let row: (i64,) =
            sqlx::query_as("INSERT INTO users (email, display_name) VALUES ($1, $2) RETURNING id")
                .bind(&self.email)
                .bind(&self.display_name)
                .fetch_one(exec)
                .await?;

        let email =
            bikesnest_domain::UserEmail::parse(&self.email).expect("builder email is valid");
        Ok(bikesnest_domain::User::new(
            bikesnest_domain::UserId(row.0),
            email,
            self.display_name.clone(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Parking builder (M1)
// ---------------------------------------------------------------------------

use bikesnest_domain::{Cost, ParkingType, TimeRange};

/// Builder: creates `parking_location` rows (+ hours + security) and returns
/// the domain aggregate. Coordinates default to Av. Paulista with ~10 m
/// offsets per call so multiple locations sort predictably by distance.
pub struct ParkingBuilder {
    name: String,
    parking_type: ParkingType,
    cost: Cost,
    lat: f64,
    lon: f64,
    hours_rows: Vec<(u8, chrono::NaiveTime, chrono::NaiveTime, bool)>,
    hours_unknown: bool,
    /// IANA identifier the wall-clock hours are read in.
    timezone: &'static str,
    /// (feature_code, state 0/1/2)
    security: Vec<(String, i16)>,
    rating_avg: Option<f64>,
    rating_count: i64,
    verified_days_ago: Option<i64>,
    moderation_state: &'static str,
    /// Tag stored in `seed_key` so committed fixture rows can be cleaned up
    /// by tag (`seed_key` column).
    fixture_tag: Option<String>,
    /// Optimistic-concurrency version. Defaults to 1 for fresh inserts.
    version: i64,
}

impl Default for ParkingBuilder {
    fn default() -> Self {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self {
            name: format!("Test Parking {n}"),
            parking_type: ParkingType::Rack,
            cost: Cost::Free,
            // ~10 m apart, walking north from Av. Paulista, 1578.
            lat: -23.561_414 + n as f64 * 0.000_09,
            lon: -46.655_881,
            hours_rows: Vec::new(),
            hours_unknown: false,
            timezone: "America/Sao_Paulo",
            security: Vec::new(),
            rating_avg: None,
            rating_count: 0,
            verified_days_ago: Some(5),
            moderation_state: "ACTIVE",
            fixture_tag: None,
            version: 1,
        }
    }
}

impl ParkingBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tags the row with `seed_key = marker` for committed-fixture cleanup
    /// (read-model tests that query through pool connections).
    pub fn with_fixture_tag(mut self, marker: impl Into<String>) -> Self {
        self.fixture_tag = Some(marker.into());
        self
    }

    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    pub fn with_type(mut self, t: ParkingType) -> Self {
        self.parking_type = t;
        self
    }

    pub fn with_cost(mut self, cost: Cost) -> Self {
        self.cost = cost;
        self
    }

    pub fn at(mut self, lat: f64, lon: f64) -> Self {
        self.lat = lat;
        self.lon = lon;
        self
    }

    /// Meters north of the Curitiba centroid (distance tests).
    pub fn meters_north_of_center(mut self, meters: f64) -> Self {
        self.lat = -25.4284 + meters / 111_320.0;
        self
    }

    /// Weekly hours, e.g. `.with_hours(1..=5, (6,0), (20,0))`.
    pub fn with_hours(
        mut self,
        days: impl std::iter::Iterator<Item = u8>,
        opens: (u32, u32),
        closes: (u32, u32),
    ) -> Self {
        for d in days {
            self.hours_rows.push((
                d,
                chrono::NaiveTime::from_hms_opt(opens.0, opens.1, 0).expect("hour"),
                chrono::NaiveTime::from_hms_opt(closes.0, closes.1, 0).expect("hour"),
                false,
            ));
        }
        self
    }

    pub fn with_all_day_hours(mut self, days: impl std::iter::Iterator<Item = u8>) -> Self {
        for d in days {
            self.hours_rows.push((
                d,
                chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
                chrono::NaiveTime::from_hms_opt(23, 59, 59).unwrap(),
                true,
            ));
        }
        self
    }

    /// IANA timezone the hours are read in (default `America/Sao_Paulo`).
    /// Coordinates are independent of it, so a fixture can sit in a test patch
    /// and still keep a DST-observing clock.
    pub fn with_timezone(mut self, tz: &'static str) -> Self {
        self.timezone = tz;
        self
    }

    pub fn with_unknown_hours(mut self) -> Self {
        self.hours_unknown = true;
        self.hours_rows.clear();
        self
    }

    pub fn with_security(mut self, code: &str, state: i16) -> Self {
        self.security.push((code.to_string(), state));
        self
    }

    pub fn with_rating(mut self, avg: f64, count: i64) -> Self {
        self.rating_avg = Some(avg);
        self.rating_count = count;
        self
    }

    pub fn verified_days_ago(mut self, days: i64) -> Self {
        self.verified_days_ago = Some(days);
        self
    }

    pub fn never_verified(mut self) -> Self {
        self.verified_days_ago = None;
        self
    }

    pub fn with_moderation_state(mut self, state: &'static str) -> Self {
        self.moderation_state = state;
        self
    }

    pub fn with_version(mut self, version: i64) -> Self {
        self.version = version;
        self
    }

    /// Inserts the location (and hours + security rows) in the test
    /// transaction; returns the domain aggregate.
    pub async fn create(
        &self,
        conn: &mut sqlx::PgConnection,
    ) -> Result<bikesnest_domain::ParkingLocation, sqlx::Error> {
        let (cost_kind, price_cents, price_currency, price_unit) = match &self.cost {
            Cost::Free => ("free", None, None, None),
            Cost::Unknown => ("unknown", None, None, None),
            Cost::Paid { price: None } => ("paid", None, None, None),
            Cost::Paid { price: Some(p) } => (
                "paid",
                Some(p.cents()),
                Some(p.currency().as_str().to_string()),
                Some(p.unit().as_code().to_string()),
            ),
        };
        let last_verified_at = self
            .verified_days_ago
            .map(|d| chrono::Utc::now() - chrono::Duration::days(d));

        let row: (i64,) = sqlx::query_as(
            r#"
            INSERT INTO parking_location
                (name, address, description, parking_type, cost_kind,
                 price_cents, price_currency, price_unit,
                 location, timezone, hours_unknown,
                 rating_avg, rating_count,
                 created_at, updated_at, last_verified_at, moderation_state, seed_key, version)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                    ST_SetSRID(ST_MakePoint($10, $9), 4326)::geography,
                    $18, $11, $12, $13,
                    now(), now(), $14, $15, $16, $17)
            RETURNING id
            "#,
        )
        .bind(&self.name)
        .bind("Rua Teste, 100")
        .bind(None::<String>)
        .bind(self.parking_type.as_code())
        .bind(cost_kind)
        .bind(price_cents)
        .bind(price_currency)
        .bind(price_unit)
        .bind(self.lat)
        .bind(self.lon)
        .bind(self.hours_unknown)
        .bind(self.rating_avg.map(|a| a as f32))
        .bind(self.rating_count)
        .bind(last_verified_at)
        .bind(self.moderation_state)
        .bind(self.fixture_tag.as_deref())
        .bind(self.version)
        .bind(self.timezone)
        .fetch_one(&mut *conn)
        .await?;
        let id = row.0;

        for (day, opens, closes, all_day) in &self.hours_rows {
            sqlx::query(
                "INSERT INTO opening_hours (location_id, day_of_week, opens_at, closes_at, all_day) VALUES ($1,$2,$3,$4,$5)",
            )
            .bind(id)
            .bind(i16::from(*day))
            .bind(opens)
            .bind(closes)
            .bind(all_day)
            .execute(&mut *conn)
            .await?;
        }

        // Every catalog feature is recorded: explicit values, or unknown.
        let recorded: Vec<&str> = self.security.iter().map(|(c, _)| c.as_str()).collect();
        for feature in [
            "dedicated_locking_point",
            "indoor",
            "cctv",
            "staffed",
            "security_guard",
            "controlled_access",
            "well_lit",
            "restricted_access",
        ] {
            let state = self
                .security
                .iter()
                .find(|(c, _)| c == feature)
                .map(|(_, s)| *s)
                .unwrap_or(0);
            let _ = recorded.contains(&feature); // recorded set used above via find()
            sqlx::query(
                "INSERT INTO parking_security (location_id, feature_code, state) VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(feature)
            .bind(state)
            .execute(&mut *conn)
            .await?;
        }

        Ok(bikesnest_domain::ParkingLocation::new(
            id,
            self.name.clone(),
            "Rua Teste, 100",
            None,
            self.parking_type,
            self.cost.clone(),
            bikesnest_domain::GeoPoint::new(self.lat, self.lon).expect("builder coords"),
            self.timezone.parse().expect("builder timezone"),
            if self.hours_unknown {
                bikesnest_domain::OpeningHours::Unknown
            } else {
                bikesnest_domain::OpeningHours::weekly(
                    self.hours_rows
                        .iter()
                        .map(|(d, o, c, ad)| {
                            (
                                *d,
                                if *ad {
                                    TimeRange::all_day()
                                } else {
                                    TimeRange {
                                        opens_at: *o,
                                        closes_at: *c,
                                        all_day: false,
                                    }
                                },
                            )
                        })
                        .collect(),
                )
            },
            self.security
                .iter()
                .map(|(code, state)| {
                    bikesnest_domain::SecurityFeature::new(
                        code.clone(),
                        bikesnest_domain::SecurityState::from_smallint(*state).expect("state"),
                    )
                })
                .collect(),
            bikesnest_domain::ModerationState::from_code(self.moderation_state).expect("state"),
            bikesnest_domain::Rating::new(self.rating_avg, self.rating_count).expect("rating"),
            chrono::Utc::now(),
            chrono::Utc::now(),
            None,
            last_verified_at,
            self.version,
        )
        .expect("builder values are valid"))
    }
}

// ---------------------------------------------------------------------------
// Fast test password hasher (M2)
// ---------------------------------------------------------------------------

use async_trait::async_trait;
use bikesnest_application::{AuthError, PasswordHasher};
use bikesnest_domain::Password;

/// A non-cryptographic password hash for tests. Identical prefixes so `hash` is
/// trivially verifiable, but it never runs argon2 — web/test suites stay fast.
#[derive(Debug, Default, Clone, Copy)]
pub struct TestPasswordHasher;

#[async_trait]
impl PasswordHasher for TestPasswordHasher {
    async fn hash(&self, pw: &Password) -> Result<String, AuthError> {
        Ok(format!("test:{}", pw.as_str()))
    }

    async fn verify(&self, pw: &Password, hash: &str) -> Result<bool, AuthError> {
        Ok(hash == format!("test:{}", pw.as_str()))
    }
}
