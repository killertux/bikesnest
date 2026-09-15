//! SQL-backed photo repository: the queue insert,
//! moderation flips and the pending-photo read model — now dispatching across
//! both `parking_photo` and `review_photo` through [`bikesnest_application::PhotoKind`].

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{
    NewPendingPhoto, PendingPhoto, PhotoError, PhotoForModeration, PhotoKind, PhotoRepository,
    PhotoTarget, RejectedPhoto,
};
use bikesnest_domain::{PhotoModerationState, UserId};
use sqlx::FromRow;

pub struct SqlxPhotoRepository {
    db: Db,
}

impl SqlxPhotoRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(FromRow)]
struct IdRow {
    id: i64,
}

#[derive(FromRow)]
struct PendingRow {
    id: i64,
    kind: String,
    parent_id: i64,
    parent_name: String,
    storage_key: String,
    thumbnail_key: Option<String>,
    alt: Option<String>,
    width: Option<i32>,
    height: Option<i32>,
    uploader_id: Option<i64>,
    created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(FromRow)]
struct ModRow {
    id: i64,
    parent_id: i64,
    moderation_state: String,
    storage_key: String,
    thumbnail_key: Option<String>,
}

#[derive(FromRow)]
struct RejectedRow {
    storage_key: String,
    thumbnail_key: Option<String>,
}

fn parent_col(kind: PhotoKind) -> &'static str {
    match kind {
        PhotoKind::Parking => "location_id",
        PhotoKind::Review => "review_id",
    }
}

#[async_trait]
impl PhotoRepository for SqlxPhotoRepository {
    /// One insert, with the derivative keys already known: [`PhotoService`]
    /// mints them from a random id before it writes the objects, so there is no
    /// "insert with `storage_key = ''` to get an id, then patch it" step (and
    /// no window in which a row points at nothing — a `CHECK (storage_key <>
    /// '')` in migration 0019 now makes that unrepresentable).
    async fn insert_pending(&self, p: &NewPendingPhoto) -> Result<i64, PhotoError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("photo.insert_pending", e))?;
        let (table, parent_col) = (p.target.kind().table(), parent_col(p.target.kind()));
        let id = if table == "parking_photo" {
            let row = sqlx::query_as::<_, IdRow>(&format!(
                "INSERT INTO parking_photo ({parent_col}, storage_key, thumbnail_key, content_type, alt, \
                 width, height, processed_at, position, moderation_state, uploader_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0, 'PENDING_REVIEW', $9) RETURNING id"
            ))
            .bind(p.target.parent_id())
            .bind(&p.storage_key)
            .bind(&p.thumbnail_key)
            .bind(&p.content_type)
            .bind(p.alt.as_deref())
            .bind(p.dimensions.width as i32)
            .bind(p.dimensions.height as i32)
            .bind(p.processed_at)
            .bind(p.uploader_id.0)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| db_err("photo.insert_pending", e))?;
            row.id
        } else {
            // `review_photo` has no content_type/alt columns (review photos have
            // no accessible caption yet) — insert only the shared columns.
            let row = sqlx::query_as::<_, IdRow>(&format!(
                "INSERT INTO review_photo ({parent_col}, storage_key, thumbnail_key, \
                 width, height, processed_at, position, moderation_state, uploader_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, 0, 'PENDING_REVIEW', $7) RETURNING id"
            ))
            .bind(p.target.parent_id())
            .bind(&p.storage_key)
            .bind(&p.thumbnail_key)
            .bind(p.dimensions.width as i32)
            .bind(p.dimensions.height as i32)
            .bind(p.processed_at)
            .bind(p.uploader_id.0)
            .fetch_one(&mut *conn)
            .await
            .map_err(|e| db_err("photo.insert_pending", e))?;
            row.id
        };
        Ok(id)
    }

    async fn max_position(&self, target: PhotoTarget) -> Result<i32, PhotoError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("photo.max_position", e))?;
        let (table, parent_col) = (target.kind().table(), parent_col(target.kind()));
        let row = sqlx::query_as::<_, (Option<i32>,)>(&format!(
            "SELECT COALESCE(MAX(position), 0) AS position FROM {table} WHERE {parent_col} = $1"
        ))
        .bind(target.parent_id())
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_err("photo.max_position", e))?;
        Ok(row.0.unwrap_or(0))
    }

    async fn delete(&self, kind: PhotoKind, id: i64) -> Result<(), PhotoError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("photo.delete", e))?;
        sqlx::query(&format!("DELETE FROM {} WHERE id = $1", kind.table()))
            .bind(id)
            .execute(&mut *conn)
            .await
            .map_err(|e| db_err("photo.delete", e))?;
        Ok(())
    }

    async fn approve(
        &self,
        kind: PhotoKind,
        id: i64,
        moderator: UserId,
        position: i32,
    ) -> Result<(), PhotoError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("photo.approve", e))?;
        let rows = sqlx::query(&format!(
            "UPDATE {} SET moderation_state = 'APPROVED', position = $3, reviewed_by = $2, reviewed_at = now() \
             WHERE id = $1 AND moderation_state = 'PENDING_REVIEW'",
            kind.table()
        ))
        .bind(id)
        .bind(moderator.0)
        .bind(position)
        .execute(&mut *conn)
        .await
        .map_err(|e| db_err("photo.approve", e))?;
        if rows.rows_affected() != 1 {
            return Err(PhotoError::NotPending);
        }
        Ok(())
    }

    async fn reject(
        &self,
        kind: PhotoKind,
        id: i64,
        moderator: UserId,
        reason: &str,
    ) -> Result<RejectedPhoto, PhotoError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("photo.reject", e))?;
        let row = sqlx::query_as::<_, RejectedRow>(&format!(
            "UPDATE {} SET moderation_state = 'REJECTED', rejection_reason = $3, reviewed_by = $2, reviewed_at = now() \
             WHERE id = $1 AND moderation_state = 'PENDING_REVIEW' RETURNING storage_key, thumbnail_key",
            kind.table()
        ))
        .bind(id)
        .bind(moderator.0)
        .bind(reason)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_err("photo.reject", e))?;
        let Some(row) = row else {
            return Err(PhotoError::NotPending);
        };
        Ok(RejectedPhoto {
            storage_key: row.storage_key,
            thumbnail_key: row.thumbnail_key,
        })
    }

    /// Oldest first, keyset-paginated on `(created_at, id)`. Unlike the
    /// single-table listers, `id` alone can't be the cursor here: the two
    /// UNIONed tables (`parking_photo`, `review_photo`) have independent
    /// `id` sequences, so the pair — compared as a row constructor — is
    /// wrapped around the union rather than pushed into either branch.
    async fn list_pending(
        &self,
        after: Option<(chrono::DateTime<chrono::Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<PendingPhoto>, PhotoError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("photo.list_pending", e))?;
        let limit = limit.clamp(1, 200);
        let (after_at, after_id) = match after {
            Some((at, id)) => (Some(at), Some(id)),
            None => (None, None),
        };
        let rows = sqlx::query_as::<_, PendingRow>(
            r#"
            SELECT * FROM (
                SELECT p.id, 'parking' AS kind, p.location_id AS parent_id, l.name AS parent_name,
                       p.storage_key, p.thumbnail_key, p.alt, p.width, p.height, p.uploader_id, p.created_at
                FROM parking_photo p
                JOIN parking_location l ON l.id = p.location_id
                WHERE p.moderation_state = 'PENDING_REVIEW'
                UNION ALL
                SELECT p.id, 'review' AS kind, r.location_id AS parent_id, l.name AS parent_name,
                       p.storage_key, p.thumbnail_key, NULL::text AS alt, p.width, p.height, p.uploader_id, p.created_at
                FROM review_photo p
                JOIN review r ON r.id = p.review_id
                JOIN parking_location l ON l.id = r.location_id
                WHERE p.moderation_state = 'PENDING_REVIEW'
            ) pending
            WHERE $1::timestamptz IS NULL OR (created_at, id) > ($1::timestamptz, $2::bigint)
            ORDER BY created_at, id
            LIMIT $3::bigint
            "#,
        )
        .bind(after_at)
        .bind(after_id)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| db_err("photo.list_pending", e))?;

        Ok(rows
            .into_iter()
            .map(|r| PendingPhoto {
                id: r.id,
                kind: PhotoKind::from_code(&r.kind).unwrap_or(PhotoKind::Parking),
                parent_id: r.parent_id,
                parent_name: r.parent_name,
                storage_key: r.storage_key,
                thumbnail_key: r.thumbnail_key,
                alt: r.alt,
                width: r.width,
                height: r.height,
                uploader_id: r.uploader_id.map(UserId),
                created_at: r.created_at,
            })
            .collect())
    }

    async fn get_for_moderation(
        &self,
        kind: PhotoKind,
        id: i64,
    ) -> Result<Option<PhotoForModeration>, PhotoError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("photo.get_for_moderation", e))?;
        let (table, parent_col) = (kind.table(), parent_col(kind));
        let row = sqlx::query_as::<_, ModRow>(&format!(
            "SELECT id, {parent_col} AS parent_id, moderation_state, storage_key, thumbnail_key FROM {table} WHERE id = $1"
        ))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|e| db_err("photo.get_for_moderation", e))?;

        Ok(row.map(|r| PhotoForModeration {
            id: r.id,
            kind,
            parent_id: r.parent_id,
            state: PhotoModerationState::from_code(&r.moderation_state)
                .unwrap_or(PhotoModerationState::PendingReview),
            storage_key: r.storage_key,
            thumbnail_key: r.thumbnail_key,
        }))
    }
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"photo.insert"`.
fn db_err(context: &'static str, e: sqlx::Error) -> PhotoError {
    crate::db_error::classify_and_log(context, e).into()
}
