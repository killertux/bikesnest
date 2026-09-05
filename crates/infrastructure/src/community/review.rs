//! SQL-backed review repository.
//!
//! One active review per user per location. `upsert_review` is a single
//! transaction: one `INSERT … ON CONFLICT DO UPDATE` for the row, append a
//! `review_revision` holding the values just published, and recompute the
//! location rating aggregate from `ACTIVE` reviews.

use crate::Db;
use async_trait::async_trait;
use bikesnest_application::{ContributionError, Review, ReviewRepository};
use bikesnest_domain::{ReviewBody, StarRating, UserId};

pub struct SqlxReviewRepository {
    db: Db,
}

impl SqlxReviewRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl ReviewRepository for SqlxReviewRepository {
    async fn upsert_review(
        &self,
        location_id: i64,
        author: UserId,
        rating: StarRating,
        body: &ReviewBody,
    ) -> Result<bool, ContributionError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("review.upsert_review", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("review.upsert_review", e))?;

        let public_author: bool = sqlx::query_scalar(
            "SELECT public_contribution_name FROM users WHERE id = $1 FOR UPDATE",
        )
        .bind(author.0)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_err("review.capture_attribution", e))?;

        // One statement, so two concurrent *first* reviews by the same author
        // cannot both take an insert branch: the loser upserts instead of
        // hitting the `UNIQUE (location_id, author_id)` index. `xmax <> 0`
        // distinguishes the conflict path from the plain insert.
        // `moderation_state` is deliberately left out of the DO UPDATE set: an
        // author editing a review a moderator hid must not un-hide it.
        let (review_id, was_update): (i64, bool) = sqlx::query_as(
            r#"
            INSERT INTO review (location_id, author_id, rating, body, public_author)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (location_id, author_id) DO UPDATE
                SET rating = EXCLUDED.rating,
                    body = EXCLUDED.body,
                    updated_at = now()
            RETURNING id, (xmax <> 0) AS was_update
            "#,
        )
        .bind(location_id)
        .bind(author.0)
        .bind(rating.as_i16())
        .bind(body.as_str())
        .bind(public_author)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| db_err("review.upsert_review", e))?;

        // `review_revision` holds every *published* version, newest last — the
        // values just written, on the create path and the edit path alike. (The
        // 0008 comment "initial + each edit" describes exactly this; the old
        // code wrote the pre-edit values on edits, so the newest version never
        // reached the table and the first one was stored twice.)
        sqlx::query("INSERT INTO review_revision (review_id, rating, body) VALUES ($1, $2, $3)")
            .bind(review_id)
            .bind(rating.as_i16())
            .bind(body.as_str())
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("review.upsert_review", e))?;

        // Recompute the denormalized aggregate in the same transaction (no drift).
        sqlx::query(
            r#"
            UPDATE parking_location
            SET rating_avg = (
                    SELECT AVG(rating)::numeric(3,2) FROM review
                    WHERE location_id = $1 AND moderation_state = 'ACTIVE'
                ),
                rating_count = (
                    SELECT COUNT(*)::integer FROM review
                    WHERE location_id = $1 AND moderation_state = 'ACTIVE'
                )
            WHERE id = $1
            "#,
        )
        .bind(location_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("review.upsert_review", e))?;

        tx.commit()
            .await
            .map_err(|e| db_err("review.upsert_review", e))?;
        Ok(was_update)
    }

    async fn find_own(
        &self,
        location_id: i64,
        author: UserId,
    ) -> Result<Option<Review>, ContributionError> {
        let row = sqlx::query_as::<_, ReviewRow>(
            r#"
            SELECT id, location_id, author_id, rating, body, created_at, updated_at,
                CASE WHEN public_author THEN (SELECT NULLIF(BTRIM(display_name), '') FROM users WHERE users.id = review.author_id AND public_contribution_name AND account_state = 'ACTIVE') END AS public_author_name
            FROM review
            WHERE location_id = $1 AND author_id = $2
            "#,
        )
        .bind(location_id)
        .bind(author.0)
        .fetch_optional(&mut *self.db.acquire().await.map_err(|e| db_err("review.acquire", e))?)
        .await
        .map_err(|e| db_err("review.find_own", e))?;
        row.map(review_from_row).transpose()
    }

    async fn list_active(
        &self,
        location_id: i64,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Review>, ContributionError> {
        let limit = limit.clamp(1, 200);
        // Cursors on `id` alone (not the full `(created_at, id)` sort key):
        // `id` is an IDENTITY column assigned in the same insert as
        // `created_at`'s `now()` default, so the two stay co-monotonic for
        // this table's write pattern (one row per upsert, no backfills) —
        // the `review_active_location_idx` (location_id, created_at DESC, id
        // DESC) still drives the ORDER BY, the cursor just narrows on `id`.
        let rows = match after_id {
            Some(after) => {
                sqlx::query_as::<_, ReviewRow>(
                    r#"
                    SELECT id, location_id, author_id, rating, body, created_at, updated_at,
                        CASE WHEN public_author THEN (SELECT NULLIF(BTRIM(display_name), '') FROM users WHERE users.id = review.author_id AND public_contribution_name AND account_state = 'ACTIVE') END AS public_author_name
                    FROM review
                    WHERE location_id = $1 AND moderation_state = 'ACTIVE' AND id < $2
                    ORDER BY created_at DESC, id DESC
                    LIMIT $3
                    "#,
                )
                .bind(location_id)
                .bind(after)
                .bind(limit)
                .fetch_all(&mut *self.db.acquire().await.map_err(|e| db_err("review.acquire", e))?)
                .await
            }
            None => {
                sqlx::query_as::<_, ReviewRow>(
                    r#"
                    SELECT id, location_id, author_id, rating, body, created_at, updated_at,
                        CASE WHEN public_author THEN (SELECT NULLIF(BTRIM(display_name), '') FROM users WHERE users.id = review.author_id AND public_contribution_name AND account_state = 'ACTIVE') END AS public_author_name
                    FROM review
                    WHERE location_id = $1 AND moderation_state = 'ACTIVE'
                    ORDER BY created_at DESC, id DESC
                    LIMIT $2
                    "#,
                )
                .bind(location_id)
                .bind(limit)
                .fetch_all(&mut *self.db.acquire().await.map_err(|e| db_err("review.acquire", e))?)
                .await
            }
        }
        .map_err(|e| db_err("review.list_active", e))?;
        rows.into_iter().map(review_from_row).collect()
    }
}

#[derive(sqlx::FromRow)]
struct ReviewRow {
    id: i64,
    location_id: i64,
    author_id: Option<i64>,
    public_author_name: Option<String>,
    rating: i16,
    body: String,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

fn review_from_row(r: ReviewRow) -> Result<Review, ContributionError> {
    Ok(Review {
        id: r.id,
        location_id: r.location_id,
        author: r.author_id.map(UserId),
        public_author_name: r.public_author_name,
        rating: StarRating::from_smallint(r.rating)
            .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
        body: ReviewBody::new(&r.body)
            .map_err(|e| ContributionError::InvalidField(e.to_string()))?,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

/// Classify + log the sqlx error (SQLSTATE, constraint), then map it onto
/// the feature error. `context` names the operation, e.g. `"review.upsert_review"`.
fn db_err(context: &'static str, e: sqlx::Error) -> ContributionError {
    crate::db_error::classify_and_log(context, e).into()
}
