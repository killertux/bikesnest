//! SQL-backed verification repository.
//!
//! Records existence / attribute / parked-here signals. Aggregation
//! uses the latest signal per user (`DISTINCT ON`); `parked_here` signals carry
//! an `expires_at` (now + 90 days) and are never surfaced publicly.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{
    AttributeSummary, ContributionError, NewVerification, VerificationRepository,
};
use bikesnest_domain::{ExistenceResult, ExistenceSignal, UserId};
use chrono::{DateTime, Utc};

/// Parked-here retention (recommended default).
const PARKED_HERE_RETENTION_DAYS: i64 = 90;

pub struct SqlxVerificationRepository {
    db: Db,
}

impl SqlxVerificationRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl VerificationRepository for SqlxVerificationRepository {
    async fn record(
        &self,
        signal: &NewVerification,
        now: DateTime<Utc>,
    ) -> Result<(), ContributionError> {
        let (kind, result, attribute_code, expires_at): (
            &str,
            &str,
            Option<&str>,
            Option<DateTime<Utc>>,
        ) = match signal {
            NewVerification::Existence {
                location_id,
                user_id,
                result,
            } => {
                let _ = (location_id, user_id);
                ("existence", result.as_code(), None, None)
            }
            NewVerification::Attribute {
                location_id,
                user_id,
                code,
                result,
            } => {
                let _ = (location_id, user_id);
                ("attribute", result.as_code(), Some(code.as_str()), None)
            }
            NewVerification::ParkedHere {
                location_id,
                user_id,
            } => {
                let _ = (location_id, user_id);
                (
                    "parked_here",
                    "parked_here",
                    None,
                    Some(now + chrono::Duration::days(PARKED_HERE_RETENTION_DAYS)),
                )
            }
        };
        sqlx::query(
            r#"
            INSERT INTO verification (location_id, user_id, kind, result, attribute_code, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            "#,
        )
        .bind(signal.location_id())
        .bind(signal.user_id().0)
        .bind(kind)
        .bind(result)
        .bind(attribute_code)
        .bind(expires_at)
        .execute(&mut *self.db.acquire().await.map_err(|e| db_err("verification.acquire", e))?)
        .await
        .map_err(|e| db_err("verification.record", e))?;
        Ok(())
    }

    async fn latest_existence_per_user(
        &self,
        location_id: i64,
    ) -> Result<Vec<ExistenceSignal>, ContributionError> {
        #[derive(sqlx::FromRow)]
        struct SignalRow {
            user_id: Option<i64>,
            result: String,
            created_at: DateTime<Utc>,
        }
        let rows = sqlx::query_as::<_, SignalRow>(
            r#"
            SELECT DISTINCT ON (user_id) user_id, result, created_at
            FROM verification
            WHERE location_id = $1 AND kind = 'existence' AND user_id IS NOT NULL
            ORDER BY user_id, created_at DESC
            "#,
        )
        .bind(location_id)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("verification.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("verification.latest_existence_per_user", e))?;

        let mut signals = Vec::with_capacity(rows.len());
        for r in rows {
            // The SQL already excludes NULL users; skip defensively too.
            let Some(uid) = r.user_id else { continue };
            let result = ExistenceResult::from_code(&r.result)
                .map_err(|e| ContributionError::InvalidField(e.to_string()))?;
            signals.push(ExistenceSignal::new(UserId(uid), result, r.created_at));
        }
        Ok(signals)
    }

    /// Folds the per-attribute tally and the parked-here count into one
    /// statement: both read `verification` scoped to this location and differ
    /// only in `kind`, so the two `FILTER`-aggregated subqueries below are
    /// joined on `true` (the parked-here side always returns exactly one row,
    /// so the join carries its count onto every attribute row — or the lone
    /// row when there are no attribute signals at all).
    async fn attribute_and_parked_summary(
        &self,
        location_id: i64,
    ) -> Result<(Vec<AttributeSummary>, i64), ContributionError> {
        #[derive(sqlx::FromRow)]
        struct Row {
            attribute_code: Option<String>,
            correct: Option<i64>,
            incorrect: Option<i64>,
            parked_here_count: i64,
        }
        let rows = sqlx::query_as::<_, Row>(
            r#"
            SELECT a.attribute_code, a.correct, a.incorrect, p.parked_here_count
            FROM (
                -- Only count signals still within the retention window (purge
                -- is M6); an expired park shouldn't show as recent usage.
                SELECT COUNT(*) AS parked_here_count
                FROM verification
                WHERE location_id = $1 AND kind = 'parked_here'
                  AND (expires_at IS NULL OR expires_at > now())
            ) p
            LEFT JOIN (
                SELECT attribute_code,
                       COUNT(*) FILTER (WHERE result = 'correct') AS correct,
                       COUNT(*) FILTER (WHERE result = 'incorrect') AS incorrect
                FROM verification
                WHERE location_id = $1 AND kind = 'attribute'
                GROUP BY attribute_code
            ) a ON true
            ORDER BY a.attribute_code
            "#,
        )
        .bind(location_id)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("verification.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("verification.attribute_and_parked_summary", e))?;

        // `p` always has exactly one row, so `rows` is never empty even when
        // there are no attribute signals (the LEFT JOIN keeps that one row
        // with every `a.*` column NULL) — read the count from whichever row
        // is present and skip attribute rows the join padded with NULLs.
        let parked_here_count = rows.first().map(|r| r.parked_here_count).unwrap_or(0);
        let attribute_summary = rows
            .into_iter()
            .filter_map(|r| {
                let code = r.attribute_code?;
                Some(AttributeSummary {
                    code,
                    correct: r.correct.unwrap_or(0),
                    incorrect: r.incorrect.unwrap_or(0),
                })
            })
            .collect();
        Ok((attribute_summary, parked_here_count))
    }

    async fn mark_verified_at(
        &self,
        location_id: i64,
        at: DateTime<Utc>,
    ) -> Result<(), ContributionError> {
        sqlx::query("UPDATE parking_location SET last_verified_at = $1 WHERE id = $2")
            .bind(at)
            .bind(location_id)
            .execute(
                &mut *self
                    .db
                    .acquire()
                    .await
                    .map_err(|e| db_err("verification.acquire", e))?,
            )
            .await
            .map_err(|e| db_err("verification.mark_verified_at", e))?;
        Ok(())
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"verification.record"`.
fn db_err(context: &'static str, e: sqlx::Error) -> ContributionError {
    crate::db_error::classify_and_log(context, e).into()
}
