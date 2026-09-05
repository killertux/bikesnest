//! Real-PostgreSQL integration tests for the community repositories.
//!
//! The write repos create their own transactions on pool connections (they
//! commit to the shared DB), so these tests use the **committed-fixture**
//! pattern: they create a fixture user + location, run the repo, assert against
//! the repos (which read committed rows), then clean up both. Each test gets a
//! unique user email so parallel tests never collide.

use bikesnest_application::{
    ContributionError, ContributionHistoryReader, FavoriteRepository, NewParkingLocation,
    NewProposal, NewVerification, ParkingContributionRepository, ParkingEdit, ReviewPhotosReader,
    ReviewRepository, VerificationRepository,
};
use bikesnest_domain::{
    AttributeResult, Cost, CurrencyCode, ExistenceResult, GeoPoint, Money, OpeningHours,
    ParkingType, PricingUnit, ReviewBody, SecurityFeature, SecurityState, StarRating, UserId,
};
use bikesnest_infrastructure::{
    Db, SqlxContributionHistoryReader, SqlxFavoriteRepository, SqlxParkingContributionRepository,
    SqlxReviewPhotosReader, SqlxReviewRepository, SqlxVerificationRepository,
};
use bikesnest_test_support::{UserBuilder, db_test, pool};

async fn db() -> Db {
    Db::from_pool(pool().await)
}

/// A unique, clean user per test. Uses the commit-fixture + explicit cleanup so
/// the write repos (running on pool connections) can honor the FK.
async fn fresh_user(tx: &mut bikesnest_test_support::TestTx, email: &str) -> UserId {
    // Clean any leftover from a prior run of the same name.
    sqlx::query(
        "DELETE FROM parking_location WHERE creator_id = (SELECT id FROM users WHERE email = $1)",
    )
    .bind(email)
    .execute(&pool().await)
    .await
    .unwrap();
    sqlx::query("DELETE FROM users WHERE email = $1")
        .bind(email)
        .execute(&pool().await)
        .await
        .unwrap();
    let user = UserBuilder::new()
        .with_email(email)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    user.id
}

async fn cleanup_user(email: &str) {
    sqlx::query(
        "DELETE FROM parking_location WHERE creator_id = (SELECT id FROM users WHERE email = $1)",
    )
    .bind(email)
    .execute(&pool().await)
    .await
    .unwrap();
    sqlx::query("DELETE FROM users WHERE email = $1")
        .bind(email)
        .execute(&pool().await)
        .await
        .unwrap();
}

async fn verify_user(user: UserId) {
    sqlx::query(
        "UPDATE users SET account_state = 'ACTIVE', email_verified_at = now() WHERE id = $1",
    )
    .bind(user.0)
    .execute(&pool().await)
    .await
    .unwrap();
}

fn new_location() -> NewParkingLocation {
    NewParkingLocation {
        name: "Estação Centro".to_string(),
        address: "Rua Teste, 10".to_string(),
        description: None,
        parking_type: ParkingType::Rack,
        cost: Cost::Free,
        point: GeoPoint::new(-25.4284, -49.2733).unwrap(),
        timezone: Some("America/Sao_Paulo".parse().unwrap()),
        hours: OpeningHours::Unknown,
        security: vec![SecurityFeature::new("well_lit", SecurityState::Yes)],
    }
}

#[db_test]
async fn create_writes_location_revision_and_reads_back(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-create@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);

    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();
    assert!(id > 0);

    let current = repo.get_for_edit(id).await.unwrap().unwrap();
    assert_eq!(current.version(), 1);
    assert_eq!(current.name(), "Estação Centro");

    let history = repo.revision_history(id, 50).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].version, 1);
    use bikesnest_domain::ChangeKind;
    assert_eq!(history[0].change_kind, ChangeKind::Create);

    cleanup_user(email).await;
}

#[db_test]
async fn optimistic_edit_wins_only_on_expected_version(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-edit@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let edit = ParkingEdit {
        name: "Estação Centro (renovada)".to_string(),
        address: "Av. Nova, 20".to_string(),
        description: Some("renovated".to_string()),
        parking_type: ParkingType::Secured,
        cost: Cost::Paid {
            price: Some(Money::new(
                200,
                CurrencyCode::parse("BRL").unwrap(),
                PricingUnit::Hour,
            )),
        },
        hours: OpeningHours::Unknown,
        security: vec![SecurityFeature::new("cctv", SecurityState::Yes)],
    };

    // Correct version → succeeds and bumps to 2.
    let new_version = repo
        .apply_edit(id, 1, &edit, user, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(new_version, 2);
    let current = repo.get_for_edit(id).await.unwrap().unwrap();
    assert_eq!(current.version(), 2);

    // Stale version → conflict, no change.
    let err = repo
        .apply_edit(id, 1, &edit, user, chrono::Utc::now())
        .await
        .unwrap_err();
    assert!(matches!(err, ContributionError::VersionConflict));
    let current = repo.get_for_edit(id).await.unwrap().unwrap();
    assert_eq!(current.version(), 2);

    // History now has two rows (create + edit).
    let history = repo.revision_history(id, 50).await.unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].version, 2);
    assert_eq!(history[1].version, 1);

    cleanup_user(email).await;
}

#[db_test]
async fn write_security_upserts_all_features_and_updates_states_in_place(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let email = "c-write-security@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);

    let mut input = new_location();
    input.security = vec![SecurityFeature::new("cctv", SecurityState::Yes)];
    let id = repo.create(&input, user, chrono::Utc::now()).await.unwrap();

    async fn rows(id: i64) -> Vec<(String, i16)> {
        sqlx::query_as(
            "SELECT feature_code, state FROM parking_security WHERE location_id = $1 ORDER BY feature_code",
        )
        .bind(id)
        .fetch_all(&pool().await)
        .await
        .unwrap()
    }

    let after_create = rows(id).await;
    assert_eq!(
        after_create.len(),
        8,
        "one row per SECURITY_FEATURE_CODES entry, regardless of how many the caller set"
    );
    let cctv = after_create.iter().find(|(c, _)| c == "cctv").unwrap();
    assert_eq!(cctv.1, 1, "cctv=Yes");
    let indoor = after_create.iter().find(|(c, _)| c == "indoor").unwrap();
    assert_eq!(indoor.1, 0, "unset features default to Unknown");

    // An edit flips some states; the row set must still be exactly 8 (an
    // upsert in place), not 16 (a stale DELETE-then-INSERT bug would double
    // them, and a missing upsert would leave the old states unchanged).
    let edit = ParkingEdit {
        name: input.name.clone(),
        address: input.address.clone(),
        description: None,
        parking_type: ParkingType::Rack,
        cost: Cost::Free,
        hours: OpeningHours::Unknown,
        security: vec![
            SecurityFeature::new("cctv", SecurityState::No),
            SecurityFeature::new("indoor", SecurityState::Yes),
        ],
    };
    repo.apply_edit(id, 1, &edit, user, chrono::Utc::now())
        .await
        .unwrap();

    let after_edit = rows(id).await;
    assert_eq!(after_edit.len(), 8, "still exactly 8 rows after the edit");
    let cctv = after_edit.iter().find(|(c, _)| c == "cctv").unwrap();
    assert_eq!(cctv.1, 2, "cctv flipped to No");
    let indoor = after_edit.iter().find(|(c, _)| c == "indoor").unwrap();
    assert_eq!(indoor.1, 1, "indoor flipped to Yes");
    let well_lit = after_edit.iter().find(|(c, _)| c == "well_lit").unwrap();
    assert_eq!(
        well_lit.1, 0,
        "features absent from the edit fall back to Unknown"
    );

    cleanup_user(email).await;
}

#[db_test]
async fn write_hours_replaces_ranges_on_edit(tx: &mut bikesnest_test_support::TestTx) {
    use bikesnest_domain::TimeRange;
    let email = "c-write-hours@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);

    let mut input = new_location();
    input.hours = OpeningHours::weekly(vec![
        (
            1,
            TimeRange::new(
                chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
                chrono::NaiveTime::from_hms_opt(18, 0, 0).unwrap(),
            ),
        ),
        (2, TimeRange::all_day()),
    ]);
    let id = repo.create(&input, user, chrono::Utc::now()).await.unwrap();

    async fn day_rows(id: i64) -> Vec<i16> {
        let mut days: Vec<(i16,)> = sqlx::query_as(
            "SELECT day_of_week FROM opening_hours WHERE location_id = $1 ORDER BY day_of_week",
        )
        .bind(id)
        .fetch_all(&pool().await)
        .await
        .unwrap();
        days.sort();
        days.into_iter().map(|(d,)| d).collect()
    }

    assert_eq!(day_rows(id).await, vec![1, 2]);

    // A completely different set of ranges must fully replace the old ones —
    // not merge with them (the old DELETE-then-insert-per-row semantics, kept
    // as DELETE + one multi-row `unnest` insert).
    let edit = ParkingEdit {
        name: input.name.clone(),
        address: input.address.clone(),
        description: None,
        parking_type: ParkingType::Rack,
        cost: Cost::Free,
        hours: OpeningHours::weekly(vec![(
            5,
            TimeRange::new(
                chrono::NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                chrono::NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
            ),
        )]),
        security: vec![],
    };
    repo.apply_edit(id, 1, &edit, user, chrono::Utc::now())
        .await
        .unwrap();
    assert_eq!(
        day_rows(id).await,
        vec![5],
        "old ranges (days 1, 2) are gone; only the new range (day 5) remains"
    );

    // Unknown hours clear every range.
    let clear = ParkingEdit {
        hours: OpeningHours::Unknown,
        ..edit
    };
    repo.apply_edit(id, 2, &clear, user, chrono::Utc::now())
        .await
        .unwrap();
    assert!(day_rows(id).await.is_empty(), "Unknown hours leave no rows");

    cleanup_user(email).await;
}

#[db_test]
async fn edit_is_refused_for_a_location_that_is_not_active(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let email = "c-edit-inactive@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();
    let edit = ParkingEdit {
        name: "should not land".to_string(),
        address: "nowhere".to_string(),
        description: None,
        parking_type: ParkingType::Rack,
        cost: Cost::Free,
        hours: OpeningHours::Unknown,
        security: vec![],
    };

    for state in ["INVALID", "REMOVED", "FLAGGED", "PENDING_REVIEW"] {
        sqlx::query("UPDATE parking_location SET moderation_state = $2 WHERE id = $1")
            .bind(id)
            .bind(state)
            .execute(&pool().await)
            .await
            .unwrap();

        // The version is correct, so only the moderation state can refuse it —
        // and it must not be reported as a version conflict.
        let err = repo
            .apply_edit(id, 1, &edit, user, chrono::Utc::now())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ContributionError::LocationNotActive),
            "{state}: expected LocationNotActive, got {err:?}"
        );

        let (version, name): (i64, String) =
            sqlx::query_as("SELECT version, name FROM parking_location WHERE id = $1")
                .bind(id)
                .fetch_one(&pool().await)
                .await
                .unwrap();
        assert_eq!(version, 1, "{state}: version untouched");
        assert_eq!(name, "Estação Centro", "{state}: row untouched");
        assert_eq!(
            repo.revision_history(id, 50).await.unwrap().len(),
            1,
            "{state}: no revision written"
        );
    }

    // A stale version on a non-ACTIVE row still reports the state, not the
    // version — the caller learns the reason it cannot edit at all.
    let err = repo
        .apply_edit(id, 99, &edit, user, chrono::Utc::now())
        .await
        .unwrap_err();
    assert!(matches!(err, ContributionError::LocationNotActive));

    cleanup_user(email).await;
}

#[db_test]
async fn edit_revision_snapshot_holds_the_row_after_state(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-edit-snapshot@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let edit = ParkingEdit {
        name: "Estação Centro (revisada)".to_string(),
        address: "Av. Nova, 20".to_string(),
        description: None,
        parking_type: ParkingType::Rack,
        cost: Cost::Free,
        hours: OpeningHours::Unknown,
        security: vec![],
    };
    repo.apply_edit(id, 1, &edit, user, chrono::Utc::now())
        .await
        .unwrap();

    // The snapshot's untouched fields (point / timezone / moderation state) must
    // come from the UPDATE's RETURNING, not from a read taken before the
    // transaction — so they equal the row as it stands now.
    let (snapshot,): (serde_json::Value,) = sqlx::query_as(
        "SELECT snapshot FROM parking_revision WHERE location_id = $1 ORDER BY version DESC LIMIT 1",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    let (lat, lon, timezone, state): (f64, f64, String, String) = sqlx::query_as(
        "SELECT lat, lon, timezone, moderation_state FROM parking_location WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();

    assert!(
        (snapshot["point"]["lat"].as_f64().unwrap() - lat).abs() < 1e-9,
        "snapshot lat {:?} vs row {lat}",
        snapshot["point"]["lat"]
    );
    assert!(
        (snapshot["point"]["lon"].as_f64().unwrap() - lon).abs() < 1e-9,
        "snapshot lon {:?} vs row {lon}",
        snapshot["point"]["lon"]
    );
    assert_eq!(snapshot["timezone"].as_str().unwrap(), timezone);
    assert_eq!(snapshot["moderation_state"].as_str().unwrap(), state);
    assert_eq!(snapshot["name"].as_str().unwrap(), edit.name);

    cleanup_user(email).await;
}

#[db_test]
async fn proposal_is_pending_with_no_live_change(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-proposal@test.dev";
    let user = fresh_user(tx, email).await;
    verify_user(user).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let p = NewProposal {
        location_id: id,
        proposer_id: user,
        base_version: 1,
        kind: bikesnest_domain::ProposalKind::MoveLocation,
        proposed: serde_json::json!({"lat": -25.0, "lon": -49.0, "reason": "moved"}),
    };
    let pid = repo.create_proposal(&p).await.unwrap();
    assert!(pid > 0);

    // No live change; location still at version 1.
    let current = repo.get_for_edit(id).await.unwrap().unwrap();
    assert_eq!(current.version(), 1);

    let row: (String,) = sqlx::query_as("SELECT status FROM parking_proposal WHERE id = $1")
        .bind(pid)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(row.0, "PENDING");

    cleanup_user(email).await;
}

#[db_test]
async fn review_upsert_recomputes_rating_and_appends_history(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let email = "c-review@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let reviews = SqlxReviewRepository::new(db().await);
    let body = ReviewBody::new("gostei muito da estrutura").unwrap();

    // Two different authors.
    let user2 = fresh_user(tx, "c-review-2@test.dev").await;

    reviews
        .upsert_review(id, user, StarRating::new(4).unwrap(), &body)
        .await
        .unwrap();
    reviews
        .upsert_review(id, user2, StarRating::new(5).unwrap(), &body)
        .await
        .unwrap();

    let active = reviews.list_active(id, None, 50).await.unwrap();
    assert_eq!(active.len(), 2);

    // Aggregate recomputed == direct COUNT/AVG.
    let db = db().await;
    let row: (Option<f64>, i32) = sqlx::query_as(
        "SELECT rating_avg::float8, rating_count FROM parking_location WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(row.1, 2);
    assert!((row.0.unwrap() - 4.5).abs() < 0.001, "avg = {:?}", row.0);

    // Editing author `user` appends a revision but keeps one row.
    let body2 = ReviewBody::new("updated").unwrap();
    let existed = reviews.find_own(id, user).await.unwrap().is_some();
    assert!(existed);
    reviews
        .upsert_review(id, user, StarRating::new(2).unwrap(), &body2)
        .await
        .unwrap();
    let active = reviews.list_active(id, None, 50).await.unwrap();
    assert_eq!(active.len(), 2);
    let own = reviews.find_own(id, user).await.unwrap().unwrap();
    assert_eq!(own.rating.value(), 2);
    let rev_count: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM review_revision WHERE review_id = $1")
            .bind(own.id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rev_count.0, 2);

    cleanup_user(email).await;
    cleanup_user("c-review-2@test.dev").await;
}

#[db_test]
async fn verification_still_exists_sets_last_verified_at(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-verify@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let ver = SqlxVerificationRepository::new(db().await);

    ver.record(
        &NewVerification::Existence {
            location_id: id,
            user_id: user,
            result: ExistenceResult::StillExists,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    // The *service* calls mark_verified_at for a positive existence signal;
    // the repo exposes it as a separate method. Test it here.
    ver.mark_verified_at(id, chrono::Utc::now()).await.unwrap();
    let verified: (bool,) =
        sqlx::query_as("SELECT last_verified_at IS NOT NULL FROM parking_location WHERE id = $1")
            .bind(id)
            .fetch_one(db().await.pool())
            .await
            .unwrap();
    assert!(verified.0);

    // A later no_longer_exists from another user → two latest-per-user signals.
    let user2 = fresh_user(tx, "c-verify-2@test.dev").await;
    ver.record(
        &NewVerification::Existence {
            location_id: id,
            user_id: user2,
            result: ExistenceResult::NoLongerExists,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let signals = ver.latest_existence_per_user(id).await.unwrap();
    assert_eq!(signals.len(), 2);

    // Attribute summary tallies per code.
    ver.record(
        &NewVerification::Attribute {
            location_id: id,
            user_id: user,
            code: "name".to_string(),
            result: AttributeResult::Incorrect,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let (summary, count_before_park) = ver.attribute_and_parked_summary(id).await.unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].incorrect, 1);
    assert_eq!(count_before_park, 0);

    // Parked-here count + expiry set.
    ver.record(
        &NewVerification::ParkedHere {
            location_id: id,
            user_id: user,
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    let (summary, count) = ver.attribute_and_parked_summary(id).await.unwrap();
    assert_eq!(count, 1);
    // The fold still returns the same attribute tally alongside the count.
    assert_eq!(summary.len(), 1);
    let exp: (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("SELECT expires_at FROM verification WHERE kind = 'parked_here' LIMIT 1")
            .fetch_one(db().await.pool())
            .await
            .unwrap();
    assert!(exp.0.is_some());

    cleanup_user(email).await;
    cleanup_user("c-verify-2@test.dev").await;
}

#[db_test]
async fn review_upsert_survives_a_repeated_first_review(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-review-upsert@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let reviews = SqlxReviewRepository::new(db().await);
    let first = ReviewBody::new("primeira versão").unwrap();
    let second = ReviewBody::new("segunda versão").unwrap();

    // Two "first reviews" back to back: the old read-then-write pair let both
    // take the insert branch, and the second hit the unique index. The single
    // upsert turns the loser into an update instead.
    assert!(
        !reviews
            .upsert_review(id, user, StarRating::new(4).unwrap(), &first)
            .await
            .unwrap(),
        "the first write creates the review"
    );
    assert!(
        reviews
            .upsert_review(id, user, StarRating::new(2).unwrap(), &second)
            .await
            .unwrap(),
        "the second write updates it"
    );

    let db = db().await;
    let (rows,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM review WHERE location_id = $1 AND author_id = $2")
            .bind(id)
            .bind(user.0)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(rows, 1, "one review row per author per location");

    let own = reviews.find_own(id, user).await.unwrap().unwrap();
    assert_eq!(own.rating.value(), 2);
    assert_eq!(own.body.as_str(), "segunda versão");

    // review_revision holds every published version, newest last.
    let history: Vec<(i16, String)> =
        sqlx::query_as("SELECT rating, body FROM review_revision WHERE review_id = $1 ORDER BY id")
            .bind(own.id)
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0], (4, "primeira versão".to_string()));
    assert_eq!(
        history[1],
        (2, "segunda versão".to_string()),
        "the newest version is the last row"
    );

    // The aggregate reflects the update, not the original rating.
    let (avg, count): (Option<f64>, i32) = sqlx::query_as(
        "SELECT rating_avg::float8, rating_count FROM parking_location WHERE id = $1",
    )
    .bind(id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(count, 1);
    assert!((avg.unwrap() - 2.0).abs() < 0.001, "avg = {avg:?}");

    cleanup_user(email).await;
}

#[db_test]
async fn review_edit_does_not_unhide_a_hidden_review(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-review-hidden@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();
    let reviews = SqlxReviewRepository::new(db().await);
    let body = ReviewBody::new("texto original").unwrap();
    reviews
        .upsert_review(id, user, StarRating::new(5).unwrap(), &body)
        .await
        .unwrap();
    let own = reviews.find_own(id, user).await.unwrap().unwrap();
    sqlx::query("UPDATE review SET moderation_state = 'HIDDEN' WHERE id = $1")
        .bind(own.id)
        .execute(&pool().await)
        .await
        .unwrap();

    reviews
        .upsert_review(
            id,
            user,
            StarRating::new(1).unwrap(),
            &ReviewBody::new("texto editado").unwrap(),
        )
        .await
        .unwrap();

    let (state,): (String,) = sqlx::query_as("SELECT moderation_state FROM review WHERE id = $1")
        .bind(own.id)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(
        state, "HIDDEN",
        "an author edit never restores a hidden review"
    );
    assert!(
        reviews.list_active(id, None, 50).await.unwrap().is_empty(),
        "a hidden review stays out of the public list"
    );

    cleanup_user(email).await;
}

#[db_test]
async fn for_reviews_groups_by_review_and_orders_by_position(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let email = "c-review-photos@test.dev";
    let user = fresh_user(tx, email).await;
    let email2 = "c-review-photos-2@test.dev";
    let user2 = fresh_user(tx, email2).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let reviews = SqlxReviewRepository::new(db().await);
    reviews
        .upsert_review(
            id,
            user,
            StarRating::new(5).unwrap(),
            &ReviewBody::new("gostei").unwrap(),
        )
        .await
        .unwrap();
    reviews
        .upsert_review(
            id,
            user2,
            StarRating::new(4).unwrap(),
            &ReviewBody::new("bom tambem").unwrap(),
        )
        .await
        .unwrap();
    let r1 = reviews.find_own(id, user).await.unwrap().unwrap().id;
    let r2 = reviews.find_own(id, user2).await.unwrap().unwrap().id;

    // Three APPROVED photos per review, inserted in non-sequential `position`
    // order (2, 0, 1) — the reader must sort by `position`, not echo
    // insertion/id order.
    for (review_id, label) in [(r1, "r1"), (r2, "r2")] {
        for (pos, suffix) in [(2, "c"), (0, "a"), (1, "b")] {
            sqlx::query(
                "INSERT INTO review_photo (review_id, storage_key, moderation_state, position) \
                 VALUES ($1, $2, 'APPROVED', $3)",
            )
            .bind(review_id)
            .bind(format!("{label}/{suffix}.jpg"))
            .bind(pos)
            .execute(&pool().await)
            .await
            .unwrap();
        }
    }
    // A PENDING_REVIEW photo on r1 must never appear (only APPROVED renders).
    sqlx::query(
        "INSERT INTO review_photo (review_id, storage_key, moderation_state, position) \
         VALUES ($1, 'r1/pending.jpg', 'PENDING_REVIEW', 3)",
    )
    .bind(r1)
    .execute(&pool().await)
    .await
    .unwrap();

    let reader = SqlxReviewPhotosReader::new(db().await);
    let grouped = reader.for_reviews(&[r1, r2]).await.unwrap();

    assert_eq!(grouped.len(), 2, "both reviews have an entry");
    let r1_keys: Vec<&str> = grouped[&r1].iter().map(|p| p.key.as_str()).collect();
    assert_eq!(
        r1_keys,
        vec!["r1/a.jpg", "r1/b.jpg", "r1/c.jpg"],
        "r1 ordered by position, pending photo excluded"
    );
    let r2_keys: Vec<&str> = grouped[&r2].iter().map(|p| p.key.as_str()).collect();
    assert_eq!(
        r2_keys,
        vec!["r2/a.jpg", "r2/b.jpg", "r2/c.jpg"],
        "r2 ordered by position independently of r1"
    );

    cleanup_user(email).await;
    cleanup_user(email2).await;
}

#[db_test]
async fn favorite_toggle_reports_the_state_it_wrote(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-fav-toggle@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();
    let fav = SqlxFavoriteRepository::new(db().await);

    async fn rows(user: UserId, id: i64) -> i64 {
        let (n,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM favorite WHERE user_id = $1 AND location_id = $2")
                .bind(user.0)
                .bind(id)
                .fetch_one(&pool().await)
                .await
                .unwrap();
        n
    }

    // added → removed → added, with the row count agreeing after each toggle.
    assert!(fav.toggle(user, id).await.unwrap(), "first toggle adds");
    assert_eq!(rows(user, id).await, 1);
    assert!(
        !fav.toggle(user, id).await.unwrap(),
        "second toggle removes"
    );
    assert_eq!(rows(user, id).await, 0);
    assert!(
        fav.toggle(user, id).await.unwrap(),
        "third toggle adds again"
    );
    assert_eq!(rows(user, id).await, 1);
    assert!(fav.is_favorited(user, id).await.unwrap());

    cleanup_user(email).await;
}

#[db_test]
async fn favorite_toggle_is_idempotent(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-fav@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let fav = SqlxFavoriteRepository::new(db().await);
    assert!(fav.toggle(user, id).await.unwrap()); // now favorited
    assert!(fav.is_favorited(user, id).await.unwrap());
    assert!(!fav.toggle(user, id).await.unwrap()); // now unfavorited
    assert!(!fav.is_favorited(user, id).await.unwrap());
    assert_eq!(fav.list(user, None, 50).await.unwrap().len(), 0);

    fav.toggle(user, id).await.unwrap();
    let listed = fav.list(user, None, 50).await.unwrap();
    assert_eq!(
        listed.iter().map(|f| f.location_id).collect::<Vec<_>>(),
        vec![id]
    );

    cleanup_user(email).await;
}

#[db_test]
async fn favorite_list_orders_by_recency_and_pages_are_disjoint(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let email = "c-fav-recency@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let fav = SqlxFavoriteRepository::new(db().await);

    // Three locations, favorited in order oldest -> newest, with explicit
    // `created_at` values so recency order is deterministic (a real `toggle`
    // could otherwise land within the same millisecond).
    let mut ids = Vec::new();
    for _ in 0..3 {
        let id = repo
            .create(&new_location(), user, chrono::Utc::now())
            .await
            .unwrap();
        fav.toggle(user, id).await.unwrap();
        ids.push(id);
    }
    let base = chrono::Utc::now() - chrono::Duration::hours(1);
    for (i, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE favorite SET created_at = $1 WHERE user_id = $2 AND location_id = $3")
            .bind(base + chrono::Duration::seconds(i as i64))
            .bind(user.0)
            .bind(id)
            .execute(&pool().await)
            .await
            .unwrap();
    }
    let (oldest, middle, newest) = (ids[0], ids[1], ids[2]);

    // Page 1 (limit 2): the two most recently favorited, newest first.
    let page1 = fav.list(user, None, 2).await.unwrap();
    let page1_ids: Vec<i64> = page1.iter().map(|f| f.location_id).collect();
    assert_eq!(page1_ids, vec![newest, middle], "newest favorited first");

    // Page 2, cursored from the last item on page 1: the remaining (oldest)
    // one, disjoint from page 1.
    let cursor = page1.last().unwrap();
    let page2 = fav
        .list(user, Some((cursor.created_at, cursor.location_id)), 2)
        .await
        .unwrap();
    let page2_ids: Vec<i64> = page2.iter().map(|f| f.location_id).collect();
    assert_eq!(page2_ids, vec![oldest]);
    assert!(
        page2_ids.iter().all(|id| !page1_ids.contains(id)),
        "pages are disjoint"
    );

    // A newly favorited location (real `now()`, after all three backdated
    // rows) appears first.
    let fresh_id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();
    fav.toggle(user, fresh_id).await.unwrap();
    let top = fav.list(user, None, 1).await.unwrap();
    assert_eq!(
        top.first().map(|f| f.location_id),
        Some(fresh_id),
        "a newly favorited location appears first"
    );

    cleanup_user(email).await;
}

#[db_test]
async fn history_reads_contributions_across_sources(tx: &mut bikesnest_test_support::TestTx) {
    let email = "c-history@test.dev";
    let user = fresh_user(tx, email).await;
    let repo = SqlxParkingContributionRepository::new(db().await);
    let id = repo
        .create(&new_location(), user, chrono::Utc::now())
        .await
        .unwrap();

    let favorite = SqlxFavoriteRepository::new(db().await);
    favorite.toggle(user, id).await.unwrap();

    let history = SqlxContributionHistoryReader::new(db().await);
    let items = history.history(user, None, 50).await.unwrap();
    let kinds: Vec<&str> = items.iter().map(|i| i.kind.as_str()).collect();
    assert!(kinds.contains(&"added"));
    assert!(kinds.contains(&"favorited"));
    assert!(items.len() >= 2);

    cleanup_user(email).await;
}
