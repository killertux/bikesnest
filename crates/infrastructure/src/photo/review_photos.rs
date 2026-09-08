//! SQL-backed reader for reviews' approved photos (D3). Only `APPROVED`
//! review photos render on the review card; hidden/rejected/pending are excluded.

use crate::Db;
use crate::parking::search::reader_err;
use async_trait::async_trait;
use bikesnest_application::{ReaderError, ReviewPhotosReader, StoredPhoto};
use std::collections::HashMap;

pub struct SqlxReviewPhotosReader {
    db: Db,
}

impl SqlxReviewPhotosReader {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    review_id: i64,
    storage_key: String,
    thumbnail_key: Option<String>,
}

#[async_trait]
impl ReviewPhotosReader for SqlxReviewPhotosReader {
    async fn for_reviews(
        &self,
        review_ids: &[i64],
    ) -> Result<HashMap<i64, Vec<StoredPhoto>>, ReaderError> {
        let mut by_review: HashMap<i64, Vec<StoredPhoto>> = HashMap::new();
        if review_ids.is_empty() {
            return Ok(by_review);
        }
        let rows = sqlx::query_as::<_, Row>(
            r#"
            SELECT review_id, storage_key, thumbnail_key
            FROM review_photo
            WHERE review_id = ANY($1) AND moderation_state = 'APPROVED'
            ORDER BY review_id, position, id
            "#,
        )
        .bind(review_ids)
        .fetch_all(
            &mut *self
                .db
                .acquire()
                .await
                .map_err(|e| crate::parking::search::reader_err("review_photos.acquire", e))?,
        )
        .await
        .map_err(|e| reader_err("review_photos.for_reviews", e))?;

        for r in rows {
            by_review.entry(r.review_id).or_default().push(StoredPhoto {
                key: r.storage_key,
                thumbnail_key: r.thumbnail_key,
                content_type: "image/jpeg".to_string(),
                alt: None,
            });
        }
        Ok(by_review)
    }
}
