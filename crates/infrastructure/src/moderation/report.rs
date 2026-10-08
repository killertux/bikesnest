//! SQL-backed report repository.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{ModerationError, NewReport, Report, ReportRepository};
use bikesnest_domain::{ReportOutcome, ReportState, ReportTargetType, UserId};
use chrono::{DateTime, Utc};

pub struct SqlxReportRepository {
    db: Db,
}

impl SqlxReportRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct ReportRow {
    id: i64,
    reporter_id: Option<i64>,
    target_type: String,
    target_id: i64,
    reason: String,
    description: Option<String>,
    state: String,
    claimed_by: Option<i64>,
    resolved_by: Option<i64>,
    resolution_note: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ReportRow {
    fn into_report(self) -> Result<Report, ModerationError> {
        let target_type =
            ReportTargetType::from_code(&self.target_type).map_err(ModerationError::from)?;
        let state = ReportState::from_code(&self.state).map_err(ModerationError::from)?;
        Ok(Report {
            id: self.id,
            reporter_id: self.reporter_id.map(UserId),
            target_type,
            target_id: self.target_id,
            reason: self.reason,
            description: self.description,
            state,
            claimed_by: self.claimed_by.map(UserId),
            resolved_by: self.resolved_by.map(UserId),
            resolution_note: self.resolution_note,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[async_trait]
impl ReportRepository for SqlxReportRepository {
    async fn create(&self, r: &NewReport) -> Result<i64, ModerationError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("report.create", e))?;
        let mut tx = conn.begin().await.map_err(|e| db_err("report.create", e))?;
        #[derive(sqlx::FromRow)]
        struct IdRow {
            id: i64,
        }
        let row = sqlx::query_as::<_, IdRow>(
            r#"
            INSERT INTO report
                (reporter_id, target_type, target_id, reason, description, state)
            VALUES ($1, $2, $3, $4, $5, 'OPEN')
            RETURNING id
            "#,
        )
        .bind(r.reporter_id.0)
        .bind(r.target_type.as_code())
        .bind(r.target_id)
        .bind(&r.reason)
        .bind(r.description.as_str())
        .fetch_one(&mut *tx)
        .await;
        let row = match row {
            Ok(row) => row,
            Err(error) => {
                tx.rollback()
                    .await
                    .map_err(|rollback| db_err("report.create.rollback", rollback))?;
                return Err(db_err("report.create", error));
            }
        };
        tx.commit().await.map_err(|e| db_err("report.create", e))?;
        Ok(row.id)
    }

    /// Oldest first (`id ASC` — the moderation queue is a FIFO work list; `id`
    /// alone also gives a simple, exact keyset cursor with no tie-breaking
    /// needed, unlike the old `created_at, id` order).
    async fn list(
        &self,
        state: Option<ReportState>,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Report>, ModerationError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("report.list", e))?;
        let limit = limit.clamp(1, 200);
        let rows: Vec<ReportRow> = match (state, after_id) {
            (Some(s), Some(after)) => sqlx::query_as::<_, ReportRow>(
                r#"
                    SELECT id, reporter_id, target_type, target_id, reason, description, state,
                           claimed_by, resolved_by, resolution_note, created_at, updated_at
                    FROM report
                    WHERE state = $1 AND id > $2
                    ORDER BY id ASC
                    LIMIT $3
                    "#,
            )
            .bind(s.as_code())
            .bind(after)
            .bind(limit)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| db_err("report.list", e))?,
            (Some(s), None) => sqlx::query_as::<_, ReportRow>(
                r#"
                    SELECT id, reporter_id, target_type, target_id, reason, description, state,
                           claimed_by, resolved_by, resolution_note, created_at, updated_at
                    FROM report
                    WHERE state = $1
                    ORDER BY id ASC
                    LIMIT $2
                    "#,
            )
            .bind(s.as_code())
            .bind(limit)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| db_err("report.list", e))?,
            (None, Some(after)) => sqlx::query_as::<_, ReportRow>(
                r#"
                    SELECT id, reporter_id, target_type, target_id, reason, description, state,
                           claimed_by, resolved_by, resolution_note, created_at, updated_at
                    FROM report
                    WHERE id > $1
                    ORDER BY id ASC
                    LIMIT $2
                    "#,
            )
            .bind(after)
            .bind(limit)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| db_err("report.list", e))?,
            (None, None) => sqlx::query_as::<_, ReportRow>(
                r#"
                    SELECT id, reporter_id, target_type, target_id, reason, description, state,
                           claimed_by, resolved_by, resolution_note, created_at, updated_at
                    FROM report
                    ORDER BY id ASC
                    LIMIT $1
                    "#,
            )
            .bind(limit)
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| db_err("report.list", e))?,
        };
        rows.into_iter().map(ReportRow::into_report).collect()
    }

    async fn get(&self, id: i64) -> Result<Option<Report>, ModerationError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("report.get", e))?;
        let row = sqlx::query_as::<_, ReportRow>(
            r#"
            SELECT id, reporter_id, target_type, target_id, reason, description, state,
                   claimed_by, resolved_by, resolution_note, created_at, updated_at
            FROM report
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_err("report.get", e))?;
        match row {
            Some(r) => Ok(Some(r.into_report()?)),
            None => Ok(None),
        }
    }

    async fn claim(&self, id: i64, moderator: UserId) -> Result<(), ModerationError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("report.claim", e))?;
        let res = sqlx::query(
            r#"
            UPDATE report
            SET state = 'UNDER_REVIEW', claimed_by = $2, updated_at = now()
            WHERE id = $1 AND state = 'OPEN'
            "#,
        )
        .bind(id)
        .bind(moderator.0)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_err("report.claim", e))?;
        if res.rows_affected() != 1 {
            return Err(ModerationError::InvalidState);
        }
        Ok(())
    }

    async fn resolve(
        &self,
        id: i64,
        moderator: UserId,
        note: &str,
        outcome: ReportOutcome,
    ) -> Result<(), ModerationError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("report.resolve", e))?;
        let res = sqlx::query(
            r#"
            UPDATE report
            SET state = $3, resolved_by = $2, resolution_note = $4, updated_at = now()
            WHERE id = $1 AND state = 'UNDER_REVIEW'
            "#,
        )
        .bind(id)
        .bind(moderator.0)
        .bind(outcome.as_code())
        .bind(note)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_err("report.resolve", e))?;
        if res.rows_affected() != 1 {
            return Err(ModerationError::InvalidState);
        }
        Ok(())
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"report.create"`.
fn db_err(context: &'static str, e: sqlx::Error) -> ModerationError {
    crate::db_error::classify_and_log(context, e).into()
}
