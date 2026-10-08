//! SQL-backed reader for the sitemap: the ids of every publicly listed
//! location.

use async_trait::async_trait;
use bikesnest_application::{ReaderError, SitemapReader};

use crate::Db;
use crate::parking::search::reader_err;

pub struct SqlxSitemapReader {
    db: Db,
}

impl SqlxSitemapReader {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl SitemapReader for SqlxSitemapReader {
    /// Ordered by id so the emitted sitemap is stable between requests.
    async fn active_parking_ids(&self) -> Result<Vec<i64>, ReaderError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| reader_err("sitemap.active_parking_ids", e))?;
        sqlx::query_scalar(
            "SELECT id FROM parking_location WHERE moderation_state = 'ACTIVE' ORDER BY id",
        )
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| reader_err("sitemap.active_parking_ids", e))
    }
}
