//! SQL-backed job repository.
//!
//! The claim query uses `FOR UPDATE SKIP LOCKED` so multiple in-process workers
//! (or instances) pull disjoint jobs without blocking. Attempts are incremented
//! at claim so a crash-then-reclaim still burns budget and cannot loop forever.
//! A worker that dies mid-run leaves its job `state = 'running'` with a lease
//! that keeps counting down; once `lease_expires_at` is in the past, `claim`
//! treats that row exactly like a fresh `pending` one and reclaims it
//! (at-least-once). Every post-claim update (`finish_success`, `retry`, `fail`)
//! is scoped to `claimed_by = <the calling worker>`, so if the original
//! (zombie) worker wakes up after its lease has already been reassigned, its
//! stale write returns `LostOwnership` instead of clobbering the new claim.

use crate::Db;
use bikesnest_application::JobPayload;
use chrono::{DateTime, Utc};
use serde_json::Value;

#[derive(Debug, thiserror::Error)]
pub enum JobRepoError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("invalid recurring registration: {0}")]
    InvalidRecurring(String),
    #[error("recurring registration needs retry after active lease: {0}")]
    ReconciliationDeferred(String),
    #[error("recurring bootstrap incomplete for {0:?}")]
    BootstrapIncomplete(Vec<String>),
    #[error("mail credential is not eligible for delivery")]
    InvalidMailCredential,
    #[error("job lease ownership was lost for job {0}")]
    LostOwnership(i64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurringRegistrationOutcome {
    Inserted,
    HealthyPreserved,
    PendingScheduleRepaired,
    TerminalReactivated,
    RunningPreserved,
    FailedPreserved,
}

/// A row claimed by a worker, ready to run.
#[derive(Debug, sqlx::FromRow)]
pub struct ClaimedJob {
    pub id: i64,
    pub kind: String,
    pub payload: JobPayload,
    /// Already incremented by the claim (the current attempt, 1-based).
    pub attempts: i32,
    pub max_attempts: i32,
    /// `{"every_seconds":N}` / `{"cron":"…"}` when recurring, else `NULL`.
    pub schedule: Option<Value>,
    /// Unique claim token used to fence heartbeat and outcome writes.
    pub owner: String,
}

pub enum MailCredential<'a> {
    Token { hash: &'a str },
    SecurityNotice { audit_id: i64 },
}

pub struct MailEnqueue<'a> {
    pub payload: &'a JobPayload,
    pub account_id: i64,
    pub credential: MailCredential<'a>,
    pub purpose: &'a str,
    pub recipient: &'a str,
    pub run_at: DateTime<Utc>,
    pub max_attempts: i32,
    pub idempotency_key: &'a str,
}

pub(crate) async fn enqueue_mail_on(
    conn: &mut sqlx::PgConnection,
    mail: MailEnqueue<'_>,
) -> Result<Option<i64>, JobRepoError> {
    let state: Option<(String, String)> =
        sqlx::query_as("SELECT account_state::text,email FROM users WHERE id=$1 FOR UPDATE")
            .bind(mail.account_id)
            .fetch_optional(&mut *conn)
            .await?;
    let (valid, token_hash, expires_at, transition_audit_id) = match (
        &mail.credential,
        state.as_ref(),
    ) {
        (MailCredential::Token { hash }, Some((state, _))) => {
            let expires_at: Option<DateTime<Utc>> = match (mail.purpose, state.as_str()) {
                ("verify", "PENDING_EMAIL_VERIFICATION" | "ACTIVE")
                | ("change", "ACTIVE") => sqlx::query_scalar("SELECT expires_at FROM email_verification_tokens WHERE token_hash=$1 AND user_id=$2 AND email=$3 AND used_at IS NULL AND expires_at>clock_timestamp()")
                    .bind(hash).bind(mail.account_id).bind(mail.recipient).fetch_optional(&mut *conn).await?,
                ("reset", "PENDING_EMAIL_VERIFICATION" | "ACTIVE") => sqlx::query_scalar("SELECT t.expires_at FROM password_reset_tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=$1 AND t.user_id=$2 AND u.email=$3 AND t.used_at IS NULL AND t.expires_at>clock_timestamp()")
                    .bind(hash).bind(mail.account_id).bind(mail.recipient).fetch_optional(&mut *conn).await?,
                _ => None,
            };
            (expires_at.is_some(), Some(*hash), expires_at, None)
        }
        (MailCredential::SecurityNotice { audit_id }, Some((state, current_email))) => {
            let state_ok = match mail.purpose {
                "password_changed" => {
                    matches!(state.as_str(), "ACTIVE" | "PENDING_EMAIL_VERIFICATION")
                        && current_email.eq_ignore_ascii_case(mail.recipient)
                }
                "email_changed" => {
                    state == "ACTIVE" && !current_email.eq_ignore_ascii_case(mail.recipient)
                }
                _ => false,
            };
            let action = match mail.purpose {
                "password_changed" => "auth.password_changed",
                "email_changed" => "auth.email_changed",
                _ => "",
            };
            let audit_ok = state_ok && sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM audit_events WHERE id=$1 AND actor_user_id=$2 AND action=$3 AND target_type='user' AND target_id=$4 AND result='success')",
            )
            .bind(audit_id)
            .bind(mail.account_id)
            .bind(action)
            .bind(mail.account_id.to_string())
            .fetch_one(&mut *conn)
            .await?;
            (audit_ok, None, None, audit_ok.then_some(*audit_id))
        }
        _ => (false, None, None, None),
    };
    if !valid {
        return Err(JobRepoError::InvalidMailCredential);
    }
    let recipient_hash =
        crate::auth::hash::sha256_hex(mail.recipient.to_ascii_lowercase().as_bytes());
    Ok(sqlx::query_scalar(r#"INSERT INTO background_job
        (kind,payload,run_at,max_attempts,idempotency_key,mail_account_id,mail_token_hash,mail_purpose,mail_token_expires_at,mail_recipient_hash,mail_transition_audit_id)
        VALUES('email.send',$1,$2,$3,$4,$5,$6,$7,$8,$9,$10)
        ON CONFLICT(idempotency_key) DO NOTHING RETURNING id"#)
        .bind(mail.payload).bind(mail.run_at).bind(mail.max_attempts).bind(mail.idempotency_key)
        .bind(mail.account_id).bind(token_hash).bind(mail.purpose).bind(expires_at)
        .bind(recipient_hash).bind(transition_audit_id)
        .fetch_optional(conn).await?)
}

/// A job-queue handle bound to the application's PostgreSQL pool.
#[derive(Clone)]
pub struct SqlxJobRepository {
    db: Db,
}

impl SqlxJobRepository {
    pub async fn claim_id(
        &self,
        id: i64,
        worker_id: &str,
        lease_ttl: std::time::Duration,
    ) -> Result<Option<ClaimedJob>, JobRepoError> {
        let lease_ms = i64::try_from(lease_ttl.as_millis()).unwrap_or(i64::MAX);
        Ok(sqlx::query_as(
            r#"UPDATE background_job SET state='running',claimed_by=$2,
               lease_expires_at=clock_timestamp()+($3*interval '1 millisecond'),
               heartbeat_at=clock_timestamp(),started_at=COALESCE(started_at,clock_timestamp()),
               attempts=attempts+1,updated_at=clock_timestamp()
               WHERE id=$1 AND kind='email.send' AND run_at<=clock_timestamp()
                 AND attempts<max_attempts AND
                 (state='pending' OR (state='running' AND lease_expires_at<=clock_timestamp()))
               RETURNING id,kind,payload,attempts,max_attempts,schedule,claimed_by AS owner"#,
        )
        .bind(id)
        .bind(worker_id)
        .bind(lease_ms)
        .fetch_optional(&mut *self.db.acquire().await?)
        .await?)
    }

    pub async fn mail_dispatch_state(
        &self,
        id: i64,
    ) -> Result<Option<(String, bool, bool)>, JobRepoError> {
        Ok(sqlx::query_as(
            "SELECT state,run_at<=clock_timestamp(),state='running' AND lease_expires_at>clock_timestamp() FROM background_job WHERE id=$1 AND kind='email.send'",
        )
        .bind(id)
        .fetch_optional(&mut *self.db.acquire().await?)
        .await?)
    }
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Register one authoritative recurring row under a stable key.
    ///
    /// Only an exact kind/payload match can be repaired. Active leases are
    /// preserved. A legacy running row without a schedule asks startup to retry
    /// after it finishes, because that worker's claimed snapshot is still
    /// one-shot. Scheduled failed rows remain dead-lettered for explicit
    /// operator recovery; legacy unscheduled failures are reactivated once.
    pub async fn register_recurring(
        &self,
        kind: &str,
        payload: &JobPayload,
        schedule: &Value,
        run_at: DateTime<Utc>,
        max_attempts: i32,
        idempotency_key: &str,
    ) -> Result<RecurringRegistrationOutcome, JobRepoError> {
        if kind.is_empty() || idempotency_key.is_empty() || max_attempts <= 0 {
            return Err(JobRepoError::InvalidRecurring(
                "kind/key must be non-empty and max_attempts must be positive".into(),
            ));
        }
        let next = crate::job::schedule::next_run_at(Some(schedule), run_at)
            .map_err(|e| JobRepoError::InvalidRecurring(e.to_string()))?;
        if next.is_none() {
            return Err(JobRepoError::InvalidRecurring(
                "schedule must produce a future run".into(),
            ));
        }

        let mut conn = self.db.acquire().await?;
        let mut tx = conn.begin().await?;
        let inserted = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO background_job
                (kind, payload, run_at, schedule, max_attempts, idempotency_key)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (idempotency_key) DO NOTHING
            RETURNING id
            "#,
        )
        .bind(kind)
        .bind(payload)
        .bind(run_at)
        .bind(schedule)
        .bind(max_attempts)
        .bind(idempotency_key)
        .fetch_optional(&mut *tx)
        .await?;
        if inserted.is_some() {
            tx.commit().await?;
            return Ok(RecurringRegistrationOutcome::Inserted);
        }

        let (id, existing_kind, existing_payload, state, existing_schedule): (
            i64,
            String,
            Value,
            String,
            Option<Value>,
        ) = sqlx::query_as(
            "SELECT id, kind, payload, state, schedule FROM background_job WHERE idempotency_key=$1 FOR UPDATE",
        )
        .bind(idempotency_key)
        .fetch_one(&mut *tx)
        .await?;

        if existing_kind != kind || existing_payload != *payload {
            return Err(JobRepoError::InvalidRecurring(format!(
                "stable key {idempotency_key:?} belongs to a different kind or payload"
            )));
        }
        if existing_schedule.as_ref().is_some_and(|s| s != schedule) {
            return Err(JobRepoError::InvalidRecurring(format!(
                "stable key {idempotency_key:?} has an unexpected schedule"
            )));
        }

        let outcome = match (state.as_str(), existing_schedule.is_none()) {
            ("running", true) => {
                return Err(JobRepoError::ReconciliationDeferred(
                    idempotency_key.to_string(),
                ));
            }
            ("running", false) => RecurringRegistrationOutcome::RunningPreserved,
            ("pending", false) => RecurringRegistrationOutcome::HealthyPreserved,
            ("pending", true) => {
                sqlx::query(
                    "UPDATE background_job SET schedule=$2, max_attempts=$3, updated_at=now() WHERE id=$1",
                )
                .bind(id)
                .bind(schedule)
                .bind(max_attempts)
                .execute(&mut *tx)
                .await?;
                RecurringRegistrationOutcome::PendingScheduleRepaired
            }
            ("failed", false) => RecurringRegistrationOutcome::FailedPreserved,
            ("succeeded" | "failed", true) | ("succeeded", false) => {
                sqlx::query(
                    r#"UPDATE background_job
                       SET state='pending', attempts=0, run_at=$2, schedule=$3,
                           max_attempts=$4, claimed_by=NULL, lease_expires_at=NULL,
                           heartbeat_at=NULL, started_at=NULL, updated_at=now()
                       WHERE id=$1"#,
                )
                .bind(id)
                .bind(run_at)
                .bind(schedule)
                .bind(max_attempts)
                .execute(&mut *tx)
                .await?;
                RecurringRegistrationOutcome::TerminalReactivated
            }
            _ => {
                return Err(JobRepoError::InvalidRecurring(format!(
                    "stable key {idempotency_key:?} has unexpected state {state:?}"
                )));
            }
        };
        tx.commit().await?;
        Ok(outcome)
    }

    /// Insert a one-shot job. `idempotency_key = Some(k)` is a no-op (`Ok(None)`)
    /// when a row with that key already exists. Recurring jobs must use
    /// [`Self::register_recurring`] so their schedule is persisted and checked.
    pub async fn enqueue(
        &self,
        kind: &str,
        payload: &JobPayload,
        run_at: DateTime<Utc>,
        max_attempts: Option<i32>,
        idempotency_key: Option<&str>,
    ) -> Result<Option<i64>, JobRepoError> {
        let mut conn = self.db.acquire().await?;
        let id = sqlx::query_scalar::<_, i64>(
            r#"
            INSERT INTO background_job (kind, payload, run_at, max_attempts, idempotency_key)
            VALUES ($1, $2, $3, COALESCE($4, 5), $5)
            ON CONFLICT (idempotency_key) DO NOTHING
            RETURNING id
            "#,
        )
        .bind(kind)
        .bind(payload)
        .bind(run_at)
        .bind(max_attempts)
        .bind(idempotency_key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(JobRepoError::Db)?;
        Ok(id)
    }

    /// Lock and validate the account/token, then insert account-linked mail
    /// and its non-secret lifecycle metadata in one transaction. The token hash is safe for equality checks; the raw token
    /// remains only in the short-lived payload until a terminal outcome.
    pub async fn enqueue_mail(&self, mail: MailEnqueue<'_>) -> Result<Option<i64>, JobRepoError> {
        let mut conn = self.db.acquire().await?;
        let mut tx = conn.begin().await?;
        let id = enqueue_mail_on(&mut tx, mail).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Atomically claim up to `batch` due, unleased `pending` jobs for this
    /// worker, giving each a lease of `lease_ttl`. Returns the claimed rows.
    ///
    /// Claims across every `kind` — what production always wants (one queue,
    /// every registered handler). Tests that seed rows of their own kind
    /// alongside a concurrently-running suite want [`Self::claim_kinds`]
    /// instead, or they steal each other's rows.
    pub async fn claim(
        &self,
        batch: usize,
        worker_id: &str,
        lease_ttl: std::time::Duration,
    ) -> Result<Vec<ClaimedJob>, JobRepoError> {
        self.claim_inner(batch, worker_id, lease_ttl, None).await
    }

    /// Like [`Self::claim`], restricted to `kind = ANY(kinds)`.
    ///
    /// Test-only scoping: every job test seeds a `kind` unique to that test
    /// (e.g. a `test.<module>.<pid>` prefix) and claims through this method
    /// instead of [`Self::claim`], so concurrent tests — and the suite's own
    /// background worker, if one is running — can never claim each other's
    /// rows. Production code never needs this: a real worker wants every
    /// registered kind.
    pub async fn claim_kinds(
        &self,
        batch: usize,
        worker_id: &str,
        lease_ttl: std::time::Duration,
        kinds: &[&str],
    ) -> Result<Vec<ClaimedJob>, JobRepoError> {
        self.claim_inner(batch, worker_id, lease_ttl, Some(kinds))
            .await
    }

    async fn claim_inner(
        &self,
        batch: usize,
        worker_id: &str,
        lease_ttl: std::time::Duration,
        kinds: Option<&[&str]>,
    ) -> Result<Vec<ClaimedJob>, JobRepoError> {
        // Not a compile-time-checked `query_as!` (this crate builds without a
        // database; see the module-level note), so branching the SQL text on
        // whether a kind filter was asked for costs nothing extra.
        let kind_clause = if kinds.is_some() {
            "AND kind = ANY($4)"
        } else {
            ""
        };
        let sql = format!(
            r#"
            WITH candidate AS (
                SELECT id FROM background_job
                WHERE (state = 'pending' OR (state = 'running' AND lease_expires_at < clock_timestamp()))
                  AND run_at <= clock_timestamp()
                  {kind_clause}
                ORDER BY run_at, id
                FOR UPDATE SKIP LOCKED
                LIMIT $1
            )
            UPDATE background_job j
            SET state = 'running', claimed_by = $2,
                lease_expires_at = clock_timestamp() + ($3 * interval '1 millisecond'),
                heartbeat_at = now(), started_at = COALESCE(started_at, now()),
                attempts = attempts + 1, updated_at = now()
            FROM candidate c
            WHERE j.id = c.id
            RETURNING j.id, j.kind, j.payload, j.attempts, j.max_attempts, j.schedule, j.claimed_by AS owner
            "#
        );
        let mut query = sqlx::query_as::<_, ClaimedJob>(&sql)
            .bind(batch as i64)
            .bind(worker_id)
            .bind(i64::try_from(lease_ttl.as_millis()).unwrap_or(i64::MAX));
        if let Some(kinds) = kinds {
            let owned: Vec<String> = kinds.iter().map(|k| (*k).to_string()).collect();
            query = query.bind(owned);
        }
        let mut conn = self.db.acquire().await?;
        let rows = query
            .fetch_all(&mut *conn)
            .await
            .map_err(JobRepoError::Db)?;
        Ok(rows)
    }

    /// Refresh a running job's lease (called on a timer while a long handler runs).
    pub async fn heartbeat(
        &self,
        id: i64,
        worker_id: &str,
        lease_ttl: std::time::Duration,
    ) -> Result<(), JobRepoError> {
        let mut conn = self.db.acquire().await?;
        let result = sqlx::query(
            r#"
            UPDATE background_job
            SET heartbeat_at = now(),
                lease_expires_at = clock_timestamp() + ($2 * interval '1 millisecond'),
                updated_at = now()
            WHERE id = $1 AND state = 'running' AND claimed_by = $3
              AND lease_expires_at > clock_timestamp()
            "#,
        )
        .bind(id)
        .bind(i64::try_from(lease_ttl.as_millis()).unwrap_or(i64::MAX))
        .bind(worker_id)
        .execute(&mut *conn)
        .await
        .map_err(JobRepoError::Db)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(JobRepoError::LostOwnership(id))
        }
    }

    /// Mark a job successful. `next_run_at = Some(t)` reschedules a *recurring*
    /// job (`state` back to `pending`, attempts cleared); `None` completes a
    /// one-shot job (`state = 'succeeded'`). `finished_at` records the last-success
    /// time. Scoped to `claimed_by = worker_id` so a zombie worker that wakes up
    /// after its lease was reclaimed cannot stomp on the new claim.
    pub async fn finish_success(
        &self,
        id: i64,
        worker_id: &str,
        next_run_at: Option<DateTime<Utc>>,
        finished_at: DateTime<Utc>,
    ) -> Result<(), JobRepoError> {
        let mut conn = self.db.acquire().await?;
        if let Some(next) = next_run_at {
            let result = sqlx::query(
                r#"
                UPDATE background_job
                SET state = 'pending', attempts = 0, run_at = $2,
                    claimed_by = NULL, lease_expires_at = NULL, heartbeat_at = NULL,
                    started_at = NULL, last_error = NULL, finished_at = $3, updated_at = now()
                WHERE id = $1 AND state='running' AND claimed_by = $4
                  AND lease_expires_at > clock_timestamp()
                "#,
            )
            .bind(id)
            .bind(next)
            .bind(finished_at)
            .bind(worker_id)
            .execute(&mut *conn)
            .await
            .map_err(JobRepoError::Db)?;
            if result.rows_affected() != 1 {
                return Err(JobRepoError::LostOwnership(id));
            }
        } else {
            let result = sqlx::query(
                r#"
                UPDATE background_job
                SET state = 'succeeded', finished_at = $2,
                    payload = CASE WHEN kind = 'email.send' THEN '{}'::jsonb ELSE payload END,
                    payload_redacted_at = CASE WHEN kind = 'email.send' THEN COALESCE(payload_redacted_at,now()) ELSE payload_redacted_at END,
                    mail_recipient_hash = CASE WHEN kind = 'email.send' THEN NULL ELSE mail_recipient_hash END,
                    mail_transition_audit_id = CASE WHEN kind = 'email.send' THEN NULL ELSE mail_transition_audit_id END,
                    claimed_by = NULL, lease_expires_at = NULL, heartbeat_at = NULL,
                    started_at = NULL, updated_at = now()
                WHERE id = $1 AND state='running' AND claimed_by = $3
                  AND lease_expires_at > clock_timestamp()
                "#,
            )
            .bind(id)
            .bind(finished_at)
            .bind(worker_id)
            .execute(&mut *conn)
            .await
            .map_err(JobRepoError::Db)?;
            if result.rows_affected() != 1 {
                return Err(JobRepoError::LostOwnership(id));
            }
        }
        Ok(())
    }

    /// Requeue a transient failure at `run_at`, recording `error`. Scoped to
    /// `claimed_by = worker_id` (see [`Self::finish_success`]).
    pub async fn retry(
        &self,
        id: i64,
        worker_id: &str,
        error: &str,
        run_at: DateTime<Utc>,
    ) -> Result<(), JobRepoError> {
        let mut conn = self.db.acquire().await?;
        let result = sqlx::query(
            r#"
            UPDATE background_job
            SET state = 'pending', run_at = $2, last_error = $3,
                claimed_by = NULL, lease_expires_at = NULL, heartbeat_at = NULL,
                started_at = NULL, updated_at = now()
            WHERE id = $1 AND state='running' AND claimed_by = $4
              AND lease_expires_at > clock_timestamp()
            "#,
        )
        .bind(id)
        .bind(run_at)
        .bind(error)
        .bind(worker_id)
        .execute(&mut *conn)
        .await
        .map_err(JobRepoError::Db)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(JobRepoError::LostOwnership(id))
        }
    }

    /// Dead-letter a job: mark it `failed` with `error` for inspection. The row
    /// is later removed by `jobs.gc`. Scoped to `claimed_by = worker_id` (see
    /// [`Self::finish_success`]).
    pub async fn fail(&self, id: i64, worker_id: &str, error: &str) -> Result<(), JobRepoError> {
        let mut conn = self.db.acquire().await?;
        let result = sqlx::query(
            r#"
            UPDATE background_job
            SET state = 'failed', finished_at = now(), last_error = $2,
                payload = CASE WHEN kind = 'email.send' THEN '{}'::jsonb ELSE payload END,
                payload_redacted_at = CASE WHEN kind = 'email.send' THEN COALESCE(payload_redacted_at,now()) ELSE payload_redacted_at END,
                mail_recipient_hash = CASE WHEN kind = 'email.send' THEN NULL ELSE mail_recipient_hash END,
                mail_transition_audit_id = CASE WHEN kind = 'email.send' THEN NULL ELSE mail_transition_audit_id END,
                claimed_by = NULL, lease_expires_at = NULL, heartbeat_at = NULL, updated_at = now()
            WHERE id = $1 AND state='running' AND claimed_by = $3
              AND lease_expires_at > clock_timestamp()
            "#,
        )
        .bind(id)
        .bind(error)
        .bind(worker_id)
        .execute(&mut *conn)
        .await
        .map_err(JobRepoError::Db)?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(JobRepoError::LostOwnership(id))
        }
    }

    /// Delete terminal (`succeeded`/`failed`) rows whose `finished_at` is before
    /// `cutoff`. Returns the number of rows removed.
    pub async fn gc(&self, cutoff: DateTime<Utc>) -> Result<u64, JobRepoError> {
        let mut conn = self.db.acquire().await?;
        let res = sqlx::query(
            "DELETE FROM background_job WHERE state IN ('succeeded', 'failed') AND schedule IS NULL AND finished_at < $1",
        )
        .bind(cutoff)
        .execute(&mut *conn)
        .await
        .map_err(JobRepoError::Db)?;
        Ok(res.rows_affected())
    }
}
