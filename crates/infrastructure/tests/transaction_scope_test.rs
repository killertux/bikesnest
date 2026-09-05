//! The outer transaction owns cleanup, including repository commits and panic.
use bikesnest_application::{AccountRepository, ReviewRepository};
use bikesnest_domain::{ReviewBody, StarRating};
use bikesnest_infrastructure::{SqlxAccountRepository, SqlxReviewRepository};
use bikesnest_test_support::{ParkingBuilder, UserBuilder, pool, run_db_test};

#[test]
fn repository_commits_release_savepoints_and_outer_scope_rolls_back() {
    const EMAIL: &str = "savepoint-repository@test.dev";
    let mut saved_db = None;
    run_db_test(async |tx| {
        let db = tx.db().await;
        let mut conn = db.acquire().await.unwrap();
        let user = UserBuilder::new()
            .with_email(EMAIL)
            .create(&mut *conn)
            .await
            .unwrap();
        let parking = ParkingBuilder::new().create(&mut conn).await.unwrap();
        drop(conn);
        let accounts = SqlxAccountRepository::new(db.clone());
        accounts
            .set_public_contribution_name(user.id, true)
            .await
            .unwrap();
        assert!(accounts.public_contribution_name(user.id).await.unwrap());

        let reviews = SqlxReviewRepository::new(db.clone());
        let rating = StarRating::new(4).unwrap();
        let body = ReviewBody::new("Safe repository transaction").unwrap();
        // This FK failure rolls back only the repository savepoint. A subsequent
        // operation succeeds and still sees the uncommitted fixture user.
        assert!(
            reviews
                .upsert_review(-1, user.id, rating, &body)
                .await
                .is_err()
        );
        reviews
            .upsert_review(parking.id(), user.id, rating, &body)
            .await
            .unwrap();
        assert_eq!(
            reviews
                .list_active(parking.id(), None, 10)
                .await
                .unwrap()
                .len(),
            1
        );
        let visible: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email=$1)")
            .bind(EMAIL)
            .fetch_one(&pool().await)
            .await
            .unwrap();
        assert!(
            !visible,
            "repository commit must not escape the outer transaction"
        );
        saved_db = Some(db);
    });
    run_db_test(async |_tx| {
        assert!(
            saved_db.as_ref().unwrap().acquire().await.is_err(),
            "scope end invalidates surviving Db clones"
        );
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email=$1)")
            .bind(EMAIL)
            .fetch_one(&pool().await)
            .await
            .unwrap();
        assert!(
            !exists,
            "outer scope must remove fixtures without manual deletes"
        );
    });
}

#[test]
fn panicking_test_rolls_back_and_invalidates_surviving_db_clones() {
    const EMAIL: &str = "savepoint-panic@test.dev";
    let mut saved_db = None;
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_db_test(async |tx| {
            let db = tx.db().await;
            let mut conn = db.acquire().await.unwrap();
            UserBuilder::new()
                .with_email(EMAIL)
                .create(&mut *conn)
                .await
                .unwrap();
            drop(conn);
            saved_db = Some(db);
            panic!("intentional test panic");
        });
    }));
    assert!(panic.is_err());
    run_db_test(async |_tx| {
        assert!(saved_db.as_ref().unwrap().acquire().await.is_err());
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email=$1)")
            .bind(EMAIL)
            .fetch_one(&pool().await)
            .await
            .unwrap();
        assert!(!exists);
    });
}
