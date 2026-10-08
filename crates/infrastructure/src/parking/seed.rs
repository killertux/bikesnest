//! Mock parking seeder.
//!
//! Development/demo affordance only: production starts with an empty dataset.
//! `parking_location`/`parking_photo` rows are tagged with `seed_key` so
//! re-runs are idempotent (delete + insert in one transaction) and easy to
//! identify for cleanup. `users` has no `seed_key` column, so the community
//! reviewers the seeder authors reviews as are found-or-created by email
//! instead (re-seeding never duplicates them).

use crate::Db;
use crate::devdata::{REVIEW_AUTHORS, mock_parkings, review_body_for, star_ratings_for};
use bikesnest_application::{ImageProcessor, ObjectStorage, PhotoError, PutObject, StorageError};
use std::collections::HashSet;

const SEED_KEY: &str = "mock-cwb-2026-01";

/// Directory holding the bundled bike photos the seeder pushes into object
/// storage (relative to this crate).
const IMG_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/static/img");

#[derive(Debug, thiserror::Error)]
pub enum SeedError {
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("photo processing error: {0}")]
    Photo(#[from] PhotoError),
    #[error("storage error: {0}")]
    Storage(#[from] StorageError),
    #[error("seed photo {0}: {1}")]
    Io(String, std::io::Error),
    #[error("seeded object missing after upload: {0}")]
    MissingObject(String),
}

/// Seed the mock dataset: locations, hours, security, backing reviews (a
/// rating never appears without real `review` rows behind it), and photos —
/// each processed into full + thumbnail derivatives exactly like a real
/// upload and pushed through the object-storage port. `storage` and
/// `processor` are the same ports the live photo pipeline uses (S3 in
/// production; the same call works unchanged against a test double).
pub async fn seed_mock(
    db: &Db,
    storage: &dyn ObjectStorage,
    processor: &dyn ImageProcessor,
) -> Result<usize, SeedError> {
    let mut conn = db.acquire().await?;
    let mut tx = conn.begin().await?;
    // Remove every previously seeded row (any dev dataset, including an older
    // key) so re-seeding is fully idempotent and never leaves stale rows.
    // `parking_photo`/`review` rows cascade with their location; storage
    // objects use stable per-image keys, so re-seeding overwrites them in
    // place.
    sqlx::query("DELETE FROM parking_location WHERE seed_key IS NOT NULL")
        .execute(&mut *tx)
        .await?;

    // Community reviewers: find-or-create by email (idempotent — see the
    // module doc for why `users` can't be tagged/deleted like the other seed
    // tables).
    let mut author_ids: Vec<i64> = Vec::with_capacity(REVIEW_AUTHORS.len());
    for (email, display_name) in REVIEW_AUTHORS {
        let existing: Option<(i64,)> =
            sqlx::query_as("SELECT id FROM users WHERE lower(email) = lower($1)")
                .bind(email)
                .fetch_optional(&mut *tx)
                .await?;
        let id = match existing {
            Some((id,)) => id,
            None => {
                let row: (i64,) = sqlx::query_as(
                    "INSERT INTO users (email, display_name) VALUES ($1, $2) RETURNING id",
                )
                .bind(email)
                .bind(display_name)
                .fetch_one(&mut *tx)
                .await?;
                row.0
            }
        };
        author_ids.push(id);
    }

    let now = chrono::Utc::now();
    let tz = "America/Sao_Paulo";
    let mut count = 0usize;
    // Every object key the seeder pushes this run, so we can confirm each one
    // is really retrievable before declaring success.
    let mut pushed_keys: HashSet<String> = HashSet::new();

    for (loc_idx, mock) in mock_parkings().into_iter().enumerate() {
        let (cost_kind, price_cents, price_currency, price_unit) = match &mock.cost {
            bikesnest_domain::Cost::Free => ("free", None, None, None),
            bikesnest_domain::Cost::Unknown => ("unknown", None, None, None),
            bikesnest_domain::Cost::Paid { price: Some(p) } => (
                "paid",
                Some(p.cents()),
                Some(p.currency().as_str().to_string()),
                Some(p.unit().as_code().to_string()),
            ),
            bikesnest_domain::Cost::Paid { price: None } => ("paid", None, None, None),
        };
        let last_verified_at = mock
            .verified_days_ago
            .map(|d| now - chrono::Duration::days(d));

        // rating_avg/rating_count are intentionally omitted here (both
        // default to NULL/0): they're recomputed below from the actual
        // `review` rows, never hand-set.
        let row: (i64,) = sqlx::query_as(
            r#"
            INSERT INTO parking_location
                (name, address, description, parking_type, cost_kind,
                 price_cents, price_currency, price_unit,
                 location, timezone, hours_unknown,
                 created_at, updated_at, last_verified_at,
                 moderation_state, seed_key)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8,
                    ST_SetSRID(ST_MakePoint($9, $10), 4326)::geography, $11, $12,
                    $13, $13, $14,
                    'ACTIVE', $15)
            RETURNING id
            "#,
        )
        .bind(mock.name)
        .bind(mock.address)
        .bind(mock.description)
        .bind(mock.parking_type.as_code())
        .bind(cost_kind)
        .bind(price_cents)
        .bind(price_currency)
        .bind(price_unit)
        .bind(mock.lon)
        .bind(mock.lat)
        .bind(tz)
        .bind(mock.hours_unknown)
        .bind(now)
        .bind(last_verified_at)
        .bind(SEED_KEY)
        .fetch_one(&mut *tx)
        .await?;
        let id = row.0;

        for (day, opens, closes, all_day) in &mock.hours {
            sqlx::query(
                r#"
                INSERT INTO opening_hours (location_id, day_of_week, opens_at, closes_at, all_day)
                VALUES ($1, $2, $3, $4, $5)
                "#,
            )
            .bind(id)
            .bind(i16::from(*day))
            .bind(chrono::NaiveTime::from_hms_opt(opens.0, opens.1, 0).expect("seed hour"))
            .bind(chrono::NaiveTime::from_hms_opt(closes.0, closes.1, 0).expect("seed hour"))
            .bind(*all_day)
            .execute(&mut *tx)
            .await?;
        }

        for (code, state) in &mock.security {
            sqlx::query(
                "INSERT INTO parking_security (location_id, feature_code, state) VALUES ($1, $2, $3)",
            )
            .bind(id)
            .bind(code)
            .bind(*state)
            .execute(&mut *tx)
            .await?;
        }

        // Recorded-but-unknown security attributes (: unknown is explicit).
        let recorded: Vec<&str> = mock.security.iter().map(|(c, _)| *c).collect();
        for feature in bikesnest_domain::SECURITY_FEATURE_CODES.iter().copied() {
            if !recorded.contains(&feature) {
                sqlx::query(
                    "INSERT INTO parking_security (location_id, feature_code, state) VALUES ($1, $2, 0)",
                )
                .bind(id)
                .bind(feature)
                .execute(&mut *tx)
                .await?;
            }
        }

        // Reviews: synthesize `rating_count` real ACTIVE reviews
        // approximating `rating_avg`, authored by distinct seeded reviewers
        // (rotated per location so the same few people don't review
        // everything), then recompute the denormalized aggregate from them.
        if mock.rating_count > 0 {
            let target_avg = mock.rating_avg.unwrap_or(4.0);
            let stars = star_ratings_for(target_avg, mock.rating_count);
            let start = (loc_idx * 7) % author_ids.len();
            for (i, star) in stars.into_iter().enumerate() {
                let author_id = author_ids[(start + i) % author_ids.len()];
                let body = review_body_for(star, i);
                let (review_id,): (i64,) = sqlx::query_as(
                    "INSERT INTO review (location_id, author_id, rating, body) \
                     VALUES ($1, $2, $3, $4) RETURNING id",
                )
                .bind(id)
                .bind(author_id)
                .bind(i16::from(star))
                .bind(body)
                .fetch_one(&mut *tx)
                .await?;
                // `review_revision` holds one row per *published* version, and
                // the seeder publishes one — exactly what
                // `SqlxReviewRepository::upsert_review` writes on the create
                // path. Without it a seeded reviewer's personal-data export
                // came back with an empty edit history, so the export was
                // never exercised against real history in development.
                sqlx::query(
                    "INSERT INTO review_revision (review_id, rating, body) VALUES ($1, $2, $3)",
                )
                .bind(review_id)
                .bind(i16::from(star))
                .bind(body)
                .execute(&mut *tx)
                .await?;
            }
        }
        // Recompute the denormalized aggregate from actual ACTIVE reviews —
        // the same query `SqlxReviewRepository::upsert_review` uses — so a
        // location can never end up with a rating_count and no reviews behind
        // it, including the locations with no reviews at all
        // (this just confirms rating_avg/rating_count stay NULL/0 for them).
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
        .bind(id)
        .execute(&mut *tx)
        .await?;

        // Photo: process the bundled JPEG into a full derivative
        // + thumbnail exactly as a real upload would, then push both through
        // the object-storage port under stable keys (re-seeding overwrites
        // them in place) and record both in `parking_photo`.
        if let Some(basename) = mock.photo {
            let bytes = tokio::fs::read(format!("{IMG_DIR}/{basename}"))
                .await
                .map_err(|e| SeedError::Io(basename.to_string(), e))?;
            let processed = processor.process(&bytes).await?;

            let stem = basename.strip_suffix(".jpg").unwrap_or(basename);
            let full_key = format!("seed/curitiba/{basename}");
            let thumb_key = format!("seed/curitiba/{stem}-thumb.jpg");

            storage
                .put(PutObject {
                    key: full_key.clone(),
                    bytes: &processed.full,
                    content_type: processed.content_type.to_string(),
                })
                .await?;
            storage
                .put(PutObject {
                    key: thumb_key.clone(),
                    bytes: &processed.thumb,
                    content_type: processed.content_type.to_string(),
                })
                .await?;
            pushed_keys.insert(full_key.clone());
            pushed_keys.insert(thumb_key.clone());

            sqlx::query(
                r#"
                INSERT INTO parking_photo
                    (location_id, storage_key, thumbnail_key, content_type, alt,
                     width, height, processed_at, position, moderation_state, seed_key)
                VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 0, 'APPROVED', $9)
                "#,
            )
            .bind(id)
            .bind(&full_key)
            .bind(&thumb_key)
            .bind(processed.content_type)
            .bind(mock.name)
            .bind(processed.dimensions.width as i32)
            .bind(processed.dimensions.height as i32)
            .bind(now)
            .bind(SEED_KEY)
            .execute(&mut *tx)
            .await?;
        }

        count += 1;
    }

    // Verify every object the seeder just pushed is really retrievable before
    // declaring success — a `put` that silently no-ops must fail
    // the seed loudly instead of leaving a broken photo in the demo.
    for key in &pushed_keys {
        if !storage.exists(key).await? {
            return Err(SeedError::MissingObject(key.clone()));
        }
    }

    tx.commit().await?;
    Ok(count)
}
