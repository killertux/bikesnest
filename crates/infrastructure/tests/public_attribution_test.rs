//! Public names are optional, non-retroactive and permanently revocable.
use bikesnest_application::{AccountRepository, ExportRepository, ReviewRepository};
use bikesnest_domain::{ReviewBody, StarRating};
use bikesnest_infrastructure::{SqlxAccountRepository, SqlxExportRepository, SqlxReviewRepository};
use bikesnest_test_support::{ParkingBuilder, UserBuilder, db_test};

#[db_test]
async fn public_names_apply_only_to_new_reviews_and_revocation_is_permanent(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let user = UserBuilder::new()
        .with_email("public-attribution@test.dev")
        .with_name("Private Cyclist")
        .create(&mut *conn)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE users SET account_state = 'ACTIVE', email_verified_at = now() WHERE id = $1",
    )
    .bind(user.id.0)
    .execute(&mut *conn)
    .await
    .unwrap();
    let default_named = ParkingBuilder::new().create(&mut conn).await.unwrap().id();
    let first = ParkingBuilder::new().create(&mut conn).await.unwrap().id();
    let second = ParkingBuilder::new().create(&mut conn).await.unwrap().id();
    let third = ParkingBuilder::new().create(&mut conn).await.unwrap().id();
    drop(conn); // Release the lease before a repository acquires this connection.
    let accounts = SqlxAccountRepository::new(db.clone());
    let reviews = SqlxReviewRepository::new(db.clone());
    let body = ReviewBody::new("A useful parking space").unwrap();
    let rating = StarRating::new(4).unwrap();

    assert!(accounts.public_contribution_name(user.id).await.unwrap());
    reviews
        .upsert_review(default_named, user.id, rating, &body)
        .await
        .unwrap();
    assert_eq!(
        reviews.list_active(default_named, None, 10).await.unwrap()[0]
            .public_author_name
            .as_deref(),
        Some("Private Cyclist")
    );
    accounts
        .set_public_contribution_name(user.id, false)
        .await
        .unwrap();
    assert!(
        reviews.list_active(default_named, None, 10).await.unwrap()[0]
            .public_author_name
            .is_none()
    );
    reviews
        .upsert_review(first, user.id, rating, &body)
        .await
        .unwrap();
    assert!(
        reviews.list_active(first, None, 10).await.unwrap()[0]
            .public_author_name
            .is_none()
    );

    accounts
        .set_public_contribution_name(user.id, true)
        .await
        .unwrap();
    // Editing an old review is not consent to reveal its old anonymous author.
    reviews
        .upsert_review(first, user.id, rating, &body)
        .await
        .unwrap();
    assert!(
        reviews.list_active(first, None, 10).await.unwrap()[0]
            .public_author_name
            .is_none()
    );
    reviews
        .upsert_review(second, user.id, rating, &body)
        .await
        .unwrap();
    assert_eq!(
        reviews.list_active(second, None, 10).await.unwrap()[0]
            .public_author_name
            .as_deref(),
        Some("Private Cyclist")
    );

    let exports = SqlxExportRepository::new(db.clone());
    let export = exports.assemble_payload(user.id).await.unwrap();
    assert!(export.account.public_contribution_name);
    assert!(export.account.public_contribution_name_updated_at.is_some());
    assert!(
        export
            .reviews
            .iter()
            .find(|r| r.location_id == second)
            .unwrap()
            .public_author
    );

    accounts
        .set_public_contribution_name(user.id, false)
        .await
        .unwrap();
    accounts
        .set_public_contribution_name(user.id, true)
        .await
        .unwrap();
    assert!(
        reviews.list_active(second, None, 10).await.unwrap()[0]
            .public_author_name
            .is_none()
    );
    reviews
        .upsert_review(third, user.id, rating, &body)
        .await
        .unwrap();
    assert!(
        reviews.list_active(third, None, 10).await.unwrap()[0]
            .public_author_name
            .is_some()
    );

    sqlx::query("UPDATE users SET account_state = 'SUSPENDED' WHERE id = $1")
        .bind(user.id.0)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert!(
        reviews.list_active(third, None, 10).await.unwrap()[0]
            .public_author_name
            .is_none()
    );
    assert!(
        accounts
            .set_public_contribution_name(user.id, true)
            .await
            .is_err()
    );
}
