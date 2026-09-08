//! SQL-backed favorite repository.
//!
//! Favorites are per-user, idempotent and private.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{ContributionError, FavoriteItem, FavoriteRepository};
use bikesnest_domain::UserId;
use chrono::{DateTime, Utc};

pub struct SqlxFavoriteRepository {
    db: Db,
}

impl SqlxFavoriteRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl FavoriteRepository for SqlxFavoriteRepository {
    async fn toggle(&self, user: UserId, location_id: i64) -> Result<bool, ContributionError> {
        // One statement, so a double-click cannot read the state on one pool
        // connection and write it on another (which reported the wrong result
        // and could leave the button out of step with the row).
        //
        // Both arms see the same snapshot: the INSERT never observes the
        // DELETE's effect, so `NOT EXISTS (SELECT 1 FROM del)` — not the table
        // — is what decides whether it runs. A returned row means "added";
        // no row means the DELETE removed the existing favorite.
        let row: Option<(bool,)> = sqlx::query_as(
            r#"
            WITH del AS (
                DELETE FROM favorite WHERE user_id = $1 AND location_id = $2 RETURNING 1
            )
            INSERT INTO favorite (user_id, location_id)
            SELECT $1, $2 WHERE NOT EXISTS (SELECT 1 FROM del)
            RETURNING true
            "#,
        )
        .bind(user.0)
        .bind(location_id)
        .fetch_optional(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("favorite.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("favorite.toggle", e))?;
        Ok(row.is_some())
    }

    async fn is_favorited(
        &self,
        user: UserId,
        location_id: i64,
    ) -> Result<bool, ContributionError> {
        let row: (bool,) = sqlx::query_as(
            "SELECT EXISTS(SELECT 1 FROM favorite WHERE user_id = $1 AND location_id = $2)",
        )
        .bind(user.0)
        .bind(location_id)
        .fetch_one(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| db_err("favorite.acquire", e))?,
        )
        .await
        .map_err(|e| db_err("favorite.is_favorited", e))?;
        Ok(row.0)
    }

    async fn list(
        &self,
        user: UserId,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<FavoriteItem>, ContributionError> {
        let limit = limit.clamp(1, 200);
        #[derive(sqlx::FromRow)]
        struct Row {
            location_id: i64,
            created_at: DateTime<Utc>,
        }
        // Keyset on the full sort key `(created_at, location_id)` — same
        // shape as `photo.list_pending` — so "most recently favorited first"
        // is preserved exactly (favorite has no row id of its own; its PK is
        // `(user_id, location_id)`, so `location_id` alone cannot serve as a
        // recency cursor).
        let (after_at, after_id) = match after {
            Some((at, id)) => (Some(at), Some(id)),
            None => (None, None),
        };
        let rows = sqlx::query_as::<_, Row>(
            r#"
            SELECT location_id, created_at FROM favorite
            WHERE user_id = $1
              AND ($2::timestamptz IS NULL OR (created_at, location_id) < ($2::timestamptz, $3::bigint))
            ORDER BY created_at DESC, location_id DESC
            LIMIT $4::bigint
            "#,
        )
        .bind(user.0)
        .bind(after_at)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&mut *self.db.acquire().await.map_err(|e| db_err("favorite.acquire", e))?)
        .await
        .map_err(|e| db_err("favorite.list", e))?;
        Ok(rows
            .into_iter()
            .map(|r| FavoriteItem {
                location_id: r.location_id,
                created_at: r.created_at,
            })
            .collect())
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"favorite.toggle"`.
fn db_err(context: &'static str, e: sqlx::Error) -> ContributionError {
    crate::db_error::classify_and_log(context, e).into()
}
