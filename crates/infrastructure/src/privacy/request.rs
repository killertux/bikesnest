//! SQL-backed privacy-request repository.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{
    NewPrivacyRequest, PrivacyError, PrivacyRequest, PrivacyRequestRepository,
};
use bikesnest_domain::{PrivacyRequestKind, PrivacyRequestState, UserId};
use chrono::{DateTime, Utc};

pub struct SqlxPrivacyRequestRepository {
    db: Db,
}

impl SqlxPrivacyRequestRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct RequestRow {
    id: i64,
    user_id: Option<i64>,
    kind: String,
    state: String,
    details: serde_json::Value,
    fulfilled_by: Option<i64>,
    fulfilled_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl RequestRow {
    fn into_request(self) -> Result<PrivacyRequest, PrivacyError> {
        let kind = PrivacyRequestKind::from_code(&self.kind).map_err(|_| PrivacyError::Internal)?;
        let state =
            PrivacyRequestState::from_code(&self.state).map_err(|_| PrivacyError::Internal)?;
        Ok(PrivacyRequest {
            id: self.id,
            user_id: self.user_id.map(UserId),
            kind,
            state,
            details: self.details,
            fulfilled_by: self.fulfilled_by.map(UserId),
            fulfilled_at: self.fulfilled_at,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

#[async_trait]
impl PrivacyRequestRepository for SqlxPrivacyRequestRepository {
    async fn create(&self, r: &NewPrivacyRequest) -> Result<i64, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("privacy_request.create", e))?;
        #[derive(sqlx::FromRow)]
        struct IdRow {
            id: i64,
        }
        let row = sqlx::query_as::<_, IdRow>(
            r#"
            INSERT INTO privacy_request (user_id, kind, state, details)
            VALUES ($1, $2, 'OPEN', $3)
            RETURNING id
            "#,
        )
        .bind(r.user_id.0)
        .bind(r.kind.as_code())
        .bind(&r.details)
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_err("privacy_request.create", e))?;
        Ok(row.id)
    }

    async fn list(
        &self,
        state: Option<PrivacyRequestState>,
    ) -> Result<Vec<PrivacyRequest>, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("privacy_request.list", e))?;
        let rows: Vec<RequestRow> = match state {
            Some(s) => sqlx::query_as::<_, RequestRow>(
                r#"
                    SELECT id, user_id, kind, state, details, fulfilled_by, fulfilled_at,
                           created_at, updated_at
                    FROM privacy_request WHERE state = $1 ORDER BY created_at, id
                    "#,
            )
            .bind(s.as_code())
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| db_err("privacy_request.list", e))?,
            None => sqlx::query_as::<_, RequestRow>(
                r#"
                    SELECT id, user_id, kind, state, details, fulfilled_by, fulfilled_at,
                           created_at, updated_at
                    FROM privacy_request ORDER BY created_at, id
                    "#,
            )
            .fetch_all(&mut *conn)
            .await
            .map_err(|e| db_err("privacy_request.list", e))?,
        };
        rows.into_iter().map(RequestRow::into_request).collect()
    }

    async fn get(&self, id: i64) -> Result<Option<PrivacyRequest>, PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("privacy_request.get", e))?;
        let row = sqlx::query_as::<_, RequestRow>(
            r#"
            SELECT id, user_id, kind, state, details, fulfilled_by, fulfilled_at,
                   created_at, updated_at
            FROM privacy_request WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_err("privacy_request.get", e))?;
        match row {
            Some(r) => Ok(Some(r.into_request()?)),
            None => Ok(None),
        }
    }

    async fn fulfill(&self, id: i64, by: Option<UserId>) -> Result<(), PrivacyError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("privacy_request.fulfill", e))?;
        let by = by.map(|u| u.0);
        let res = sqlx::query(
            r#"
            UPDATE privacy_request
            SET state = 'COMPLETED', fulfilled_by = $2, fulfilled_at = now(), updated_at = now()
            WHERE id = $1 AND state IN ('OPEN', 'IN_PROGRESS')
            "#,
        )
        .bind(id)
        .bind(by)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_err("privacy_request.fulfill", e))?;
        if res.rows_affected() != 1 {
            return Err(PrivacyError::NotFound);
        }
        Ok(())
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"privacy_request.create"`.
fn db_err(context: &'static str, e: sqlx::Error) -> PrivacyError {
    crate::db_error::classify_and_log(context, e).into()
}
