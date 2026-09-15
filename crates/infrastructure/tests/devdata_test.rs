//! Seeder integration test (`devdata`/`parking::seed_mock`): every seeded
//! photo derivative is really retrievable, `rating_avg`/`rating_count` never
//! drift from the `review` rows that back them, and enough locations sit
//! within 1 km of the fake-geocoder centroid to reach a second results page.

use bikesnest_application::ObjectStorage;
use bikesnest_domain::PhotoLimits;
use bikesnest_infrastructure::{LocalImageProcessor, parking::seed_mock};
use bikesnest_test_support::{TestObjectStorage, db_test};

// One rollback-only scope exercises both the initial seed and idempotent replay.
#[db_test]
async fn seed_mock_backs_photos_ratings_geo_spread_and_is_idempotent(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let storage = TestObjectStorage::new();
    let processor = LocalImageProcessor::new(PhotoLimits::default());

    let seeded = seed_mock(&db, &storage, &processor)
        .await
        .expect("seed_mock should succeed against a fresh test double");
    assert!(
        seeded >= 25,
        "expected at least 25 seeded locations, got {seeded}"
    );

    // --- (a) every referenced photo key exists in the double ---------------
    let photos: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT storage_key, thumbnail_key FROM parking_photo WHERE seed_key IS NOT NULL",
    )
    .fetch_all(&mut *db.acquire().await.unwrap())
    .await
    .expect("query seeded photos");
    assert!(!photos.is_empty(), "expected at least one seeded photo");
    for (storage_key, thumbnail_key) in &photos {
        assert!(
            storage.exists(storage_key).await.unwrap(),
            "storage_key {storage_key} does not exist in the object-storage double"
        );
        let thumb = thumbnail_key
            .as_deref()
            .unwrap_or_else(|| panic!("photo {storage_key} has no thumbnail_key"));
        assert!(
            storage.exists(thumb).await.unwrap(),
            "thumbnail_key {thumb} does not exist in the object-storage double"
        );
    }

    // --- (b) rating_count/rating_avg match the ACTIVE reviews behind them --
    let locations: Vec<(i64, Option<f64>, i32)> = sqlx::query_as(
        "SELECT id, rating_avg::float8, rating_count FROM parking_location WHERE seed_key IS NOT NULL",
    )
    .fetch_all(&mut *db.acquire().await.unwrap())
    .await
    .expect("query seeded locations");
    assert!(
        locations.len() >= 25,
        "expected at least 25 seeded locations, got {}",
        locations.len()
    );
    for (id, rating_avg, rating_count) in &locations {
        let reviews: Vec<(i16,)> = sqlx::query_as(
            "SELECT rating FROM review WHERE location_id = $1 AND moderation_state = 'ACTIVE'",
        )
        .bind(id)
        .fetch_all(&mut *db.acquire().await.unwrap())
        .await
        .expect("query reviews for location");

        assert_eq!(
            reviews.len() as i32,
            *rating_count,
            "location {id}: rating_count disagrees with its ACTIVE reviews"
        );
        match rating_avg {
            None => assert!(
                reviews.is_empty(),
                "location {id}: rating_avg is NULL but {} ACTIVE reviews exist",
                reviews.len()
            ),
            Some(avg) => {
                assert!(
                    !reviews.is_empty(),
                    "location {id}: rating_avg is {avg} but there are zero ACTIVE reviews"
                );
                let mean =
                    reviews.iter().map(|(r,)| f64::from(*r)).sum::<f64>() / reviews.len() as f64;
                assert!(
                    (avg - mean).abs() < 0.01,
                    "location {id}: rating_avg {avg} != review mean {mean}"
                );
            }
        }
    }

    // --- every seeded review has its published version in review_revision --
    // `review_revision` holds one row per published version, and the export's
    // edit history reads it. The seeder used to write reviews without one, so a
    // seeded reviewer's personal-data export came back with an empty history
    // and the export was never exercised against real history in development.
    let (reviews_without_history,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM review r
        WHERE r.location_id IN (SELECT id FROM parking_location WHERE seed_key IS NOT NULL)
          AND NOT EXISTS (SELECT 1 FROM review_revision v WHERE v.review_id = r.id)
        "#,
    )
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .expect("count seeded reviews without a revision");
    assert_eq!(
        reviews_without_history, 0,
        "every seeded review must have at least one review_revision row"
    );

    // --- (c) at least 25 ACTIVE locations within 1 km of the centroid ------
    // Same point the `FakeGeocoder` resolves "Rua XV de Novembro" to
    // (`crate::devdata::LANDMARKS`), so the default search actually reaches a
    // second results page.
    let (within,): (i64,) = sqlx::query_as(
        r#"
        SELECT COUNT(*) FROM parking_location
        WHERE seed_key IS NOT NULL AND moderation_state = 'ACTIVE'
          AND ST_DWithin(
                location,
                ST_SetSRID(ST_MakePoint(-49.2733, -25.4284), 4326)::geography,
                1000
              )
        "#,
    )
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .expect("count locations within 1km");
    assert!(
        within >= 25,
        "only {within} ACTIVE locations within 1 km of the centroid"
    );

    // --- idempotency: re-running must not duplicate anything ---------------
    let reseeded = seed_mock(&db, &storage, &processor)
        .await
        .expect("re-seeding should also succeed");
    assert_eq!(
        reseeded, seeded,
        "re-seeding should not change the row count"
    );

    let (location_count,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM parking_location WHERE seed_key IS NOT NULL")
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .expect("count seeded locations");
    assert_eq!(
        location_count, seeded as i64,
        "re-seeding must not duplicate locations"
    );

    // Community reviewers are found-or-created by email (no seed_key column
    // on `users`), so re-seeding must not duplicate those accounts either.
    let (authors,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM users WHERE lower(email) LIKE '%@seed.bikesnest.dev'")
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .expect("count seeded review authors");
    assert_eq!(
        authors,
        bikesnest_infrastructure::devdata::REVIEW_AUTHORS.len() as i64,
        "re-seeding must not duplicate community reviewer accounts"
    );
}
