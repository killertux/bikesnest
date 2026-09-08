//! SQL-backed reader for a location's approved photos (P3 gallery / P2 card).

use crate::Db;
use crate::parking::search::reader_err;
use async_trait::async_trait;
use bikesnest_application::{ParkingPhotoReader, ReaderError, StoredPhoto};

pub struct SqlxParkingPhotoReader {
    db: Db,
}

impl SqlxParkingPhotoReader {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct PhotoRow {
    storage_key: String,
    thumbnail_key: Option<String>,
    content_type: String,
    alt: Option<String>,
}

#[async_trait]
impl ParkingPhotoReader for SqlxParkingPhotoReader {
    async fn pending_count(&self, location_id: i64) -> Result<i64, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("parking_photos.pending", e))?;
        let (count,): (i64,) = sqlx::query_as("SELECT count(*) FROM parking_photo WHERE location_id=$1 AND moderation_state='PENDING_REVIEW'")
            .bind(location_id).fetch_one(&mut *conn).await.map_err(|e|reader_err("parking_photos.pending",e))?;
        Ok(count)
    }
    async fn photos(&self, location_id: i64) -> Result<Vec<StoredPhoto>, ReaderError> {
        let rows = sqlx::query_as::<_, PhotoRow>(
            r#"
            SELECT storage_key, thumbnail_key, content_type, alt
            FROM parking_photo
            WHERE location_id = $1 AND moderation_state = 'APPROVED'
            ORDER BY position, id
            "#,
        )
        .bind(location_id)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| reader_err("parking_photos.acquire", e))?,
        )
        .await
        .map_err(|e| reader_err("parking_photos.photos", e))?;

        Ok(rows
            .into_iter()
            .map(|r| StoredPhoto {
                key: r.storage_key,
                thumbnail_key: r.thumbnail_key,
                content_type: r.content_type,
                alt: r.alt,
            })
            .collect())
    }
}
