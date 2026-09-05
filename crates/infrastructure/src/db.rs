use crate::config::DbConfig;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, Executor};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};

type SharedTransaction = Arc<Mutex<Option<Transaction<'static, Postgres>>>>;

#[derive(Debug, Clone)]
enum Source {
    Pool(PgPool),
    Transaction(SharedTransaction),
}

/// Real PostgreSQL connection + migration runner (SQLx, hand-written SQL).
#[derive(Debug, Clone)]
pub struct Db {
    source: Source,
}

/// A pooled connection in production, or an exclusive lease on a test's outer
/// transaction. Borrow it to begin a SQLx transaction: SQLx automatically uses
/// SAVEPOINT when the connection already has an open transaction.
pub enum DbConnection {
    Pooled(sqlx::pool::PoolConnection<Postgres>),
    Scoped(OwnedMutexGuard<Option<Transaction<'static, Postgres>>>),
}

impl Deref for DbConnection {
    type Target = PgConnection;
    fn deref(&self) -> &Self::Target {
        match self {
            Self::Pooled(c) => c,
            Self::Scoped(c) => c.as_ref().expect("active transaction lease"),
        }
    }
}

impl DerefMut for DbConnection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            Self::Pooled(c) => c,
            Self::Scoped(c) => c.as_mut().expect("active transaction lease"),
        }
    }
}

impl DbConnection {
    pub async fn begin(&mut self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        self.deref_mut().begin().await
    }

    /// PostgreSQL cannot change isolation inside a savepoint. Scoped tests must
    /// start their outer transaction at REPEATABLE READ (the harness does so).
    pub async fn begin_snapshot(&mut self) -> Result<Transaction<'_, Postgres>, sqlx::Error> {
        if matches!(self, Self::Scoped(_)) {
            let isolation: String = sqlx::query_scalar("SHOW transaction_isolation")
                .fetch_one(&mut **self)
                .await?;
            if isolation != "repeatable read" && isolation != "serializable" {
                return Err(sqlx::Error::Protocol(
                    "snapshot requires a repeatable-read outer transaction".into(),
                ));
            }
            self.begin().await
        } else {
            self.deref_mut()
                .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ")
                .await
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("failed to connect to the database")]
    Connect(#[source] sqlx::Error),
    #[error("failed to run migrations")]
    Migrate(#[source] sqlx::migrate::MigrateError),
}

impl Db {
    /// Connects with the default [`DbConfig`] (10 connections, 5 s statement
    /// timeout). Tests and the test-support fixture use this.
    pub async fn connect(database_url: &str) -> Result<Self, DbError> {
        Self::connect_with(database_url, &DbConfig::default()).await
    }

    /// Connects with an explicit pool configuration.
    ///
    /// Every pooled connection is opened with a `statement_timeout` and an
    /// `idle_in_transaction_session_timeout` so one runaway query (or a
    /// transaction abandoned by a crashed client) cannot hold a connection —
    /// and, past `max_connections` of those, the whole pool — indefinitely.
    /// Connections are also recycled (`max_lifetime` / `idle_timeout`) so a
    /// failed-over primary is picked up without a restart.
    pub async fn connect_with(database_url: &str, config: &DbConfig) -> Result<Self, DbError> {
        let statement_timeout_ms = config.statement_timeout.as_millis().max(1) as u64;
        let idle_in_tx_ms = config.idle_in_tx_timeout.as_millis().max(1) as u64;
        let pool = PgPoolOptions::new()
            .max_connections(config.max_connections)
            .acquire_timeout(config.acquire_timeout)
            .max_lifetime(config.max_lifetime)
            .idle_timeout(config.idle_timeout)
            .after_connect(move |conn, _meta| {
                Box::pin(async move {
                    conn.execute(
                        format!(
                            "SET statement_timeout = {statement_timeout_ms}; \
                             SET idle_in_transaction_session_timeout = {idle_in_tx_ms}"
                        )
                        .as_str(),
                    )
                    .await?;
                    Ok(())
                })
            })
            .connect(database_url)
            .await
            .map_err(DbError::Connect)?;
        Ok(Self::from_pool(pool))
    }

    /// Runs embedded migrations (`migrations/` at the workspace root).
    /// Idempotent: applied migrations are recorded and skipped.
    ///
    /// Migrations run on a connection *detached* from the pool with
    /// `statement_timeout` disabled: an index build on a cold database
    /// legitimately takes minutes, and that is not what the per-request timeout
    /// defends against. The connection is closed afterwards rather than
    /// returned, so its relaxed settings never leak into request handling.
    pub async fn migrate(&self) -> Result<(), DbError> {
        let mut conn = self
            .pool()
            .acquire()
            .await
            .map_err(DbError::Connect)?
            .detach();
        conn.execute("SET statement_timeout = 0; SET idle_in_transaction_session_timeout = 0")
            .await
            .map_err(DbError::Connect)?;
        let result = sqlx::migrate!("../../migrations")
            .run(&mut conn)
            .await
            .map_err(DbError::Migrate);
        let _ = conn.close().await;
        result
    }

    pub fn pool(&self) -> &PgPool {
        match &self.source {
            Source::Pool(pool) => pool,
            Source::Transaction(_) => {
                panic!("transaction-scoped Db cannot escape to a pool; use Db::acquire")
            }
        }
    }

    /// Wraps an existing pool (used by the test suite to share the
    /// test-support fixture).
    pub fn from_pool(pool: PgPool) -> Self {
        Self {
            source: Source::Pool(pool),
        }
    }

    /// Explicit transaction injection for repository/HTTP tests. All clones
    /// share one connection, so concurrent calls are serialized, not race tests.
    pub fn from_transaction(tx: Transaction<'static, Postgres>) -> Self {
        Self {
            source: Source::Transaction(Arc::new(Mutex::new(Some(tx)))),
        }
    }

    pub async fn acquire(&self) -> Result<DbConnection, sqlx::Error> {
        match &self.source {
            Source::Pool(pool) => Ok(DbConnection::Pooled(pool.acquire().await?)),
            Source::Transaction(tx) => {
                let guard = tx.clone().lock_owned().await;
                if guard.is_none() {
                    return Err(sqlx::Error::PoolClosed);
                }
                Ok(DbConnection::Scoped(guard))
            }
        }
    }

    /// Ends an injected scope, invalidating every clone, even if a router is
    /// still alive. Production pooled Db instances cannot be rolled back here.
    pub async fn rollback_scope(&self) -> Result<(), sqlx::Error> {
        let Source::Transaction(tx) = &self.source else {
            return Err(sqlx::Error::Protocol("not a transaction-scoped Db".into()));
        };
        if let Some(tx) = tx.lock().await.take() {
            tx.rollback().await?;
        }
        Ok(())
    }

    /// Simple liveness probe: `SELECT 1` with a short timeout.
    pub async fn ping(&self, timeout: std::time::Duration) -> Result<(), ProbeFailure> {
        let query = sqlx::query("SELECT 1");
        tokio::time::timeout(timeout, async {
            let mut conn = self.acquire().await?;
            query.execute(&mut *conn).await
        })
        .await
        .map_err(|_| ProbeFailure::Timeout)?
        .map_err(|e| {
            crate::db_error::classify_and_log("db.ping", e);
            ProbeFailure::DbError
        })?;
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeFailure {
    #[error("probe timed out")]
    Timeout,
    #[error("database error")]
    DbError,
}
