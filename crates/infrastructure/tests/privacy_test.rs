//! Privacy infrastructure tests: the export
//! payload (no secrets), the single-use download token, the anonymize-in-place
//! transaction, the retention purge statements, and the policy reader.
//!
//! Uses the committed-fixture pattern: fixtures are inserted through the test
//! transaction and committed with `commit_fixture` so the repos (which read /
//! write through shared pool connections) can see them.

use bikesnest_application::{
    AnonymizationRepository, AuditEvent, AuditLog, ExportAccount, ExportPayload, ExportRepository,
    NewExport, PolicyReader, PrivacyError, RetentionRepository,
};
use bikesnest_domain::{PolicyKind, RetentionPolicy, UserId};
use bikesnest_infrastructure::{
    AUDIT_METADATA_KEYS, Db, SqlxAnonymizationRepository, SqlxAuditLog, SqlxExportRepository,
    SqlxPolicyReader, SqlxRetentionRepository,
};
use bikesnest_test_support::{ParkingBuilder, TestObjectStorage, UserBuilder, db_test, pool};
use chrono::{DateTime, Duration, Utc};

async fn db() -> Db {
    Db::from_pool(pool().await)
}

fn empty_payload(user_id: i64) -> ExportPayload {
    ExportPayload::new(
        ExportAccount {
            user_id,
            email: "a@example.com".to_string(),
            display_name: None,
            public_contribution_name: false,
            public_contribution_name_updated_at: None,
            account_state: "ACTIVE".to_string(),
            email_verified_at: None,
            created_at: Utc::now(),
            roles: vec![],
        },
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        vec![],
        Utc::now(),
    )
}

/// A fixture identifier no other process or run can collide with. The suite
/// shares one database and a failing test can leave rows behind, so a literal
/// like `"m6ret@example.com"` poisons every later run.
fn unique_tag(label: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{label}-{}-{n}", std::process::id())
}

fn af(t: impl AsRef<str>) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(t.as_ref())
        .unwrap()
        .with_timezone(&Utc)
}

#[db_test]
async fn export_payload_excludes_credential_hash(tx: &mut bikesnest_test_support::TestTx) {
    let user = UserBuilder::new()
        .with_email("m6exp@example.com")
        .create(tx.executor())
        .await
        .unwrap();
    let uid = user.id.0;
    sqlx::query(
        "INSERT INTO authentication_identities (user_id, provider, provider_subject, credential_hash) \
         VALUES ($1, 'password', $2, 'supersecret-hash')",
    )
    .bind(uid)
    .bind("m6exp@example.com")
    .execute(tx.executor())
    .await
    .unwrap();
    tx.commit_fixture().await;

    let repo = SqlxExportRepository::new(db().await);
    let payload = repo.assemble_payload(UserId(uid)).await.unwrap();
    assert_eq!(payload.schema_version, 1);
    assert_eq!(payload.authentication.len(), 1);
    // credential_hash is never selected into the payload.
    let json = serde_json::to_string(&payload).unwrap();
    assert!(!json.contains("supersecret-hash"));

    // Clean up the committed fixture user.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn export_consume_download_is_single_use_and_distinguishes_errors(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let user = UserBuilder::new()
        .with_email("m6exp2@example.com")
        .create(tx.executor())
        .await
        .unwrap();
    let uid = user.id.0;
    tx.commit_fixture().await;

    let repo = SqlxExportRepository::new(db().await);
    let token = [7u8; 32];
    let now = Utc::now();
    let id = repo
        .create(&NewExport {
            user_id: UserId(uid),
            token,
            payload: empty_payload(uid),
            expires_at: now + Duration::hours(24),
        })
        .await
        .unwrap();

    // First download succeeds.
    repo.consume_download(id, &token, now).await.unwrap();

    // Second download = AlreadyDownloaded.
    let e2 = repo.consume_download(id, &token, now).await.unwrap_err();
    assert!(matches!(e2, PrivacyError::AlreadyDownloaded));

    // A different token (not yet consumed) reports InvalidToken.
    let id2 = repo
        .create(&NewExport {
            user_id: UserId(uid),
            token: [8u8; 32],
            payload: empty_payload(uid),
            expires_at: now + Duration::hours(24),
        })
        .await
        .unwrap();
    let e3 = repo
        .consume_download(id2, &[9u8; 32], now)
        .await
        .unwrap_err();
    assert!(matches!(e3, PrivacyError::InvalidToken));

    // An expired export reports Expired.
    let id3 = repo
        .create(&NewExport {
            user_id: UserId(uid),
            token: [10u8; 32],
            payload: empty_payload(uid),
            expires_at: now - Duration::hours(1),
        })
        .await
        .unwrap();
    let e4 = repo
        .consume_download(id3, &[10u8; 32], now)
        .await
        .unwrap_err();
    assert!(matches!(e4, PrivacyError::Expired));

    // Clean up the committed fixture user (cascades to exports).
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn anonymize_scrubs_identity_and_nulls_attribution(tx: &mut bikesnest_test_support::TestTx) {
    // A location to hang content off of.
    let loc = ParkingBuilder::new()
        .with_name("M6 Anon")
        .create(tx.executor())
        .await
        .unwrap();
    let loc_id = loc.id();
    let user = UserBuilder::new()
        .with_email("m6anon@example.com")
        .with_name("Ada")
        .create(tx.executor())
        .await
        .unwrap();
    let uid = user.id.0;

    // Identity + session (private, deleted).
    sqlx::query(
        "INSERT INTO authentication_identities (user_id, provider, provider_subject, credential_hash) \
         VALUES ($1, 'password', $2, 'hash')",
    )
    .bind(uid).bind("m6anon@example.com").execute(tx.executor()).await.unwrap();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, csrf_token, created_at, last_seen_at, expires_at) \
         VALUES ('tok', $1, 'csrf', now(), now(), now() + interval '30 days')",
    )
    .bind(uid).execute(tx.executor()).await.unwrap();
    sqlx::query("INSERT INTO favorite (user_id, location_id, created_at) VALUES ($1, $2, now())")
        .bind(uid)
        .bind(loc_id)
        .execute(tx.executor())
        .await
        .unwrap();

    // Parked-here (private, deleted).
    sqlx::query(
        "INSERT INTO verification (location_id, user_id, kind, result, created_at, expires_at) \
         VALUES ($1, $2, 'parked_here', 'still_exists', now(), now() + interval '90 days')",
    )
    .bind(loc_id)
    .bind(uid)
    .execute(tx.executor())
    .await
    .unwrap();
    // Existence verification (community content, retained but unattributed).
    sqlx::query(
        "INSERT INTO verification (location_id, user_id, kind, result, created_at) \
         VALUES ($1, $2, 'existence', 'still_exists', now())",
    )
    .bind(loc_id)
    .bind(uid)
    .execute(tx.executor())
    .await
    .unwrap();
    // Review (retained, author NULL).
    sqlx::query(
        "INSERT INTO review (location_id, author_id, rating, body, moderation_state, created_at, updated_at) \
         VALUES ($1, $2, 5, 'Great', 'ACTIVE', now(), now())",
    )
    .bind(loc_id).bind(uid).execute(tx.executor()).await.unwrap();
    // Proposal (retained, proposer NULL).
    sqlx::query(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status, created_at) \
         VALUES ($1, $2, 1, 'change_existence', '{\"existence\":\"exists\"}', 'PENDING', now())",
    )
    .bind(loc_id).bind(uid).execute(tx.executor()).await.unwrap();
    // Report (retained, reporter NULL).
    sqlx::query(
        "INSERT INTO report (reporter_id, target_type, target_id, reason, state, created_at, updated_at) \
         VALUES ($1, 'parking', $2, 'spam', 'OPEN', now(), now())",
    )
    .bind(uid).bind(loc_id).execute(tx.executor()).await.unwrap();
    // Photos (retained, uploader NULL).
    sqlx::query(
        "INSERT INTO parking_photo (location_id, storage_key, content_type, moderation_state, created_at, uploader_id) \
         VALUES ($1, 'seed/a.jpg', 'image/jpeg', 'APPROVED', now(), $2)",
    )
    .bind(loc_id).bind(uid).execute(tx.executor()).await.unwrap();
    // A privacy request (kept, user_id nulled).
    sqlx::query(
        "INSERT INTO privacy_request (user_id, kind, state, details) VALUES ($1, 'deletion', 'OPEN', '{}')",
    )
    .bind(uid).execute(tx.executor()).await.unwrap();

    tx.commit_fixture().await;

    let now = Utc::now();
    let repo = SqlxAnonymizationRepository::new(db().await);
    let report = repo.anonymize(UserId(uid), now).await.unwrap();

    let pool = pool().await;
    // user scrubbed.
    let (email, state, deleted_at) = sqlx::query_as::<_, (String, String, Option<DateTime<Utc>>)>(
        "SELECT email, account_state, deleted_at FROM users WHERE id = $1",
    )
    .bind(uid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(email, format!("deleted+{uid}@bikesnest.invalid"));
    assert_eq!(state, "DELETED");
    assert!(deleted_at.is_some());

    // private activity gone.
    let identities: i64 =
        sqlx::query_scalar("SELECT count(*) FROM authentication_identities WHERE user_id = $1")
            .bind(uid)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(identities, 0);
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let favs: i64 = sqlx::query_scalar("SELECT count(*) FROM favorite WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(favs, 0);
    let parked: i64 = sqlx::query_scalar("SELECT count(*) FROM verification WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(parked, 0);

    // community content retained + unattributed.
    let review_author: Option<i64> =
        sqlx::query_scalar("SELECT author_id FROM review WHERE location_id = $1")
            .bind(loc_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(review_author.is_none());
    let existence_user: Option<i64> = sqlx::query_scalar(
        "SELECT user_id FROM verification WHERE location_id = $1 AND kind = 'existence'",
    )
    .bind(loc_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(existence_user.is_none());
    let prop: Option<i64> =
        sqlx::query_scalar("SELECT proposer_id FROM parking_proposal WHERE location_id = $1")
            .bind(loc_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(prop.is_none());
    let rep: Option<i64> = sqlx::query_scalar(
        "SELECT reporter_id FROM report WHERE target_id = $1 AND target_type = 'parking'",
    )
    .bind(loc_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(rep.is_none());
    let photo: Option<i64> =
        sqlx::query_scalar("SELECT uploader_id FROM parking_photo WHERE location_id = $1")
            .bind(loc_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(photo.is_none());
    let req_user: Option<i64> =
        sqlx::query_scalar("SELECT user_id FROM privacy_request WHERE details = '{}'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(req_user.is_none());

    // Report counts.
    assert_eq!(report.identities, 1);
    assert_eq!(report.sessions, 1);
    assert_eq!(report.favorites, 1);
    assert_eq!(report.parked_here, 1);
    assert_eq!(report.reviews_anonymized, 1);
    assert_eq!(report.verifications_anonymized, 1);
    assert_eq!(report.proposals_anonymized, 1);
    assert_eq!(report.reports_anonymized, 1);
    assert_eq!(report.parking_photos_anonymized, 1);
    assert_eq!(report.privacy_requests_anonymized, 1);

    // The anonymized shell is the only remaining row referencing the user; remove it.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
}

#[db_test]
async fn retention_purges_only_expired(tx: &mut bikesnest_test_support::TestTx) {
    // Everything here is scoped to this fixture's own rows. `purge_expired_*`
    // is a table-wide DELETE and the suite shares one database, so asserting
    // its *global* return count made the test depend on what every other test
    // happened to have left lying around — and its cleanup only ran on the
    // happy path, so one failure poisoned every later run.
    let email = unique_tag("m6ret") + "@example.com";
    let expired_token = unique_tag("m6ret-expired");
    let valid_token = unique_tag("m6ret-valid");
    let pool = pool().await;
    // Unconditional pre-cleanup: a previous run that failed mid-test leaves the
    // account behind, and the insert below would collide with it.
    drop_users(&pool, &[&email], &[]).await;

    let user = UserBuilder::new()
        .with_email(&email)
        .create(tx.executor())
        .await
        .unwrap();
    let uid = user.id.0;
    let now = Utc::now();
    // One expired + one still-valid password-reset token for this user.
    for (token, expires_at) in [
        (&expired_token, now - Duration::hours(2)),
        (&valid_token, now + Duration::hours(2)),
    ] {
        sqlx::query(
            "INSERT INTO password_reset_tokens (token_hash, user_id, created_at, expires_at) \
             VALUES ($1, $2, now(), $3)",
        )
        .bind(token)
        .bind(uid)
        .bind(expires_at)
        .execute(tx.executor())
        .await
        .unwrap();
    }
    tx.commit_fixture().await;

    let repo = SqlxRetentionRepository::new(
        db().await,
        RetentionPolicy::default(),
        std::sync::Arc::new(TestObjectStorage::new()),
    );
    let purged = repo.purge_expired_password_reset_tokens(now).await.unwrap();
    assert!(
        purged >= 1,
        "the expired token must be counted among those purged"
    );

    let survivors: Vec<String> =
        sqlx::query_scalar("SELECT token_hash FROM password_reset_tokens WHERE user_id = $1")
            .bind(uid)
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(
        survivors,
        vec![valid_token.clone()],
        "the expired token must be gone and the valid one untouched"
    );

    drop_users(&pool, &[&email], &[uid]).await;
}

#[db_test]
async fn policy_reader_current_and_history(tx: &mut bikesnest_test_support::TestTx) {
    let reader = SqlxPolicyReader::new(db().await);
    let old = "m6-test-old";
    let new = "m6-test-new";

    // Insert an old + a new privacy version with far-future effective dates so
    // the reader's `current` (newest effective, not superseded) is deterministic
    // regardless of any rows already in the shared dev DB.
    sqlx::query(
        "INSERT INTO policy_version (kind, version, effective_at, content) VALUES ('privacy', $1, $2, 'old')",
    )
    .bind(old)
    .bind(af("2030-01-01T00:00:00Z"))
    .execute(tx.executor())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO policy_version (kind, version, effective_at, content) VALUES ('privacy', $1, $2, 'new')",
    )
    .bind(new)
    .bind(af("2031-01-01T00:00:00Z"))
    .execute(tx.executor())
    .await
    .unwrap();
    tx.commit_fixture().await;

    let current = reader
        .current(PolicyKind::Privacy, "pt-BR")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.locale, "pt-BR");
    assert_eq!(current.version, "m6-test-new");
    assert!(current.superseded_at.is_none());

    let history = reader.history(PolicyKind::Privacy, "pt-BR").await.unwrap();
    assert!(history.iter().any(|d| d.version == old));
    assert!(history.iter().any(|d| d.version == new));
    // Newest first.
    assert_eq!(history[0].version, "m6-test-new");

    // Clean up the fixture rows (they are committed, so they persist).
    let pool = pool().await;
    sqlx::query("DELETE FROM policy_version WHERE version = $1 OR version = $2")
        .bind(old)
        .bind(new)
        .execute(&pool)
        .await
        .unwrap();
}

#[db_test]
async fn export_payload_is_one_repeatable_read_snapshot(tx: &mut bikesnest_test_support::TestTx) {
    // The export used to read its ~13 sections one at a time on the pool, so a
    // concurrent edit could land between two of them and the document would
    // describe a state that never existed. Now every section reads inside one
    // REPEATABLE READ transaction: a write committed on another connection
    // *while the export is being assembled* must not appear in it.
    //
    // Proving that needs a write that lands mid-assembly. `assemble_payload` is
    // one call, so instead the test asserts the property that makes it hold —
    // the snapshot — by opening the same kind of transaction itself, letting a
    // second connection commit an edit, and checking the transaction still
    // reads the pre-edit row.
    const TAG: &str = "wp16-snapshot";
    drop_users(&pool().await, &["wp16-snapshot@example.com"], &[]).await;
    drop_locations(&pool().await, TAG).await;
    let loc = ParkingBuilder::new()
        .with_name("WP16 Snapshot")
        .with_fixture_tag(TAG)
        .create(tx.executor())
        .await
        .unwrap();
    let loc_id = loc.id();
    let user = UserBuilder::new()
        .with_email("wp16-snapshot@example.com")
        .create(tx.executor())
        .await
        .unwrap();
    let uid = user.id.0;
    let (review_id,): (i64,) = sqlx::query_as(
        "INSERT INTO review (location_id, author_id, rating, body) \
         VALUES ($1, $2, 4, 'before the edit') RETURNING id",
    )
    .bind(loc_id)
    .bind(uid)
    .fetch_one(tx.executor())
    .await
    .unwrap();
    for (rating, body) in [(3i16, "first version"), (4i16, "before the edit")] {
        sqlx::query("INSERT INTO review_revision (review_id, rating, body) VALUES ($1, $2, $3)")
            .bind(review_id)
            .bind(rating)
            .bind(body)
            .execute(tx.executor())
            .await
            .unwrap();
    }
    tx.commit_fixture().await;

    let pool = pool().await;

    // Open the snapshot the way the repository does, and take its first read.
    let mut snapshot = pool.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *snapshot)
        .await
        .unwrap();
    let first: String = sqlx::query_scalar("SELECT body FROM review WHERE id = $1")
        .bind(review_id)
        .fetch_one(&mut *snapshot)
        .await
        .unwrap();
    assert_eq!(first, "before the edit");

    // A second connection edits the review and commits.
    sqlx::query("UPDATE review SET body = 'after the edit' WHERE id = $1")
        .bind(review_id)
        .execute(&pool)
        .await
        .unwrap();

    // The snapshot still sees the pre-edit body: later sections of the export
    // read the same instant as the first one.
    let later: String = sqlx::query_scalar("SELECT body FROM review WHERE id = $1")
        .bind(review_id)
        .fetch_one(&mut *snapshot)
        .await
        .unwrap();
    assert_eq!(
        later, "before the edit",
        "a REPEATABLE READ transaction must not see a concurrent commit"
    );
    snapshot.commit().await.unwrap();

    // And the assembled payload carries every revision for the review — from
    // the one batched `WHERE review_id = ANY($1)` query, not one per review.
    let payload = SqlxExportRepository::new(db().await)
        .assemble_payload(UserId(uid))
        .await
        .unwrap();
    let review = payload
        .reviews
        .iter()
        .find(|r| r.id == review_id)
        .expect("the export must carry the review");
    assert_eq!(review.revisions.len(), 2, "both published versions");
    assert_eq!(review.revisions[0].body, "first version");
    assert_eq!(review.revisions[1].body, "before the edit");

    drop_locations(&pool, TAG).await;
    drop_users(&pool, &[], &[uid]).await;
}

#[db_test]
async fn export_batches_revisions_across_many_reviews(tx: &mut bikesnest_test_support::TestTx) {
    // Every review's history comes back, from one query rather than N.
    const TAG: &str = "wp16-revbatch";
    drop_users(&pool().await, &["wp16-revbatch@example.com"], &[]).await;
    drop_locations(&pool().await, TAG).await;
    let user = UserBuilder::new()
        .with_email("wp16-revbatch@example.com")
        .create(tx.executor())
        .await
        .unwrap();
    let uid = user.id.0;
    let mut expected: Vec<(i64, usize)> = Vec::new();
    for n in 0..3 {
        let loc = ParkingBuilder::new()
            .with_name(format!("WP16 RevBatch {n}"))
            .with_fixture_tag(TAG)
            .create(tx.executor())
            .await
            .unwrap();
        let (review_id,): (i64,) = sqlx::query_as(
            "INSERT INTO review (location_id, author_id, rating, body) \
             VALUES ($1, $2, 5, 'body') RETURNING id",
        )
        .bind(loc.id())
        .bind(uid)
        .fetch_one(tx.executor())
        .await
        .unwrap();
        // n + 1 published versions, so a mix-up between reviews would show.
        for v in 0..=n {
            sqlx::query("INSERT INTO review_revision (review_id, rating, body) VALUES ($1, 5, $2)")
                .bind(review_id)
                .bind(format!("v{v}"))
                .execute(tx.executor())
                .await
                .unwrap();
        }
        expected.push((review_id, n + 1));
    }
    tx.commit_fixture().await;

    let payload = SqlxExportRepository::new(db().await)
        .assemble_payload(UserId(uid))
        .await
        .unwrap();
    assert_eq!(payload.reviews.len(), 3);
    for (review_id, count) in expected {
        let review = payload
            .reviews
            .iter()
            .find(|r| r.id == review_id)
            .expect("review present");
        assert_eq!(
            review.revisions.len(),
            count,
            "review {review_id} must keep its own revisions"
        );
    }

    let pool = pool().await;
    drop_locations(&pool, TAG).await;
    drop_users(&pool, &[], &[uid]).await;
}

#[db_test]
async fn anonymize_nulls_roles_this_account_granted(tx: &mut bikesnest_test_support::TestTx) {
    const EMAILS: [&str; 4] = [
        "wp16-granter@example.com",
        "wp16-grantee@example.com",
        "wp16-admin-a@example.com",
        "wp16-admin-b@example.com",
    ];
    drop_users(&pool().await, &EMAILS, &[]).await;
    let granter = UserBuilder::new()
        .with_email("wp16-granter@example.com")
        .create(tx.executor())
        .await
        .unwrap();
    let grantee = UserBuilder::new()
        .with_email("wp16-grantee@example.com")
        .create(tx.executor())
        .await
        .unwrap();
    let (granter_id, grantee_id) = (granter.id.0, grantee.id.0);
    // Two more people the granter promoted. MODERATOR, not ADMIN, on purpose:
    // the ADMIN set is a system-wide singleton that the last-admin tests own
    // under a lock, and this test has nothing to say about it.
    for email in ["wp16-admin-a@example.com", "wp16-admin-b@example.com"] {
        let u = UserBuilder::new()
            .with_email(email)
            .create(tx.executor())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'MODERATOR', $2)",
        )
        .bind(u.id.0)
        .bind(granter_id)
        .execute(tx.executor())
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'MODERATOR', $2)")
        .bind(grantee_id)
        .bind(granter_id)
        .execute(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;

    let pool = pool().await;
    let report = SqlxAnonymizationRepository::new(db().await)
        .anonymize(UserId(granter_id), Utc::now())
        .await
        .unwrap();

    // Three rows named the granter; none may still do so.
    assert_eq!(report.roles_granted_by_anonymized, 3);
    let still_named: i64 =
        sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE granted_by = $1")
            .bind(granter_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        still_named, 0,
        "`granted_by` still names the anonymized account"
    );
    // The grants themselves survive — only the attribution is gone.
    let granted_by: Option<i64> = sqlx::query_scalar(
        "SELECT granted_by FROM user_roles WHERE user_id = $1 AND role = 'MODERATOR'",
    )
    .bind(grantee_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(granted_by.is_none());

    drop_users(&pool, &EMAILS, &[granter_id, grantee_id]).await;
}

#[db_test]
async fn anonymize_rewrites_an_email_shaped_audit_target(tx: &mut bikesnest_test_support::TestTx) {
    // A failed login is audited with the attempted *email* as `target_id`
    // (there is no user id to record). Nulling `actor_user_id` never reaches
    // it, so erasure has to rewrite it.
    const EMAIL: &str = "wp16-audit-pii@example.com";
    drop_users(&pool().await, &[EMAIL], &[]).await;
    let user = UserBuilder::new()
        .with_email(EMAIL)
        .create(tx.executor())
        .await
        .unwrap();
    let uid = user.id.0;
    tx.commit_fixture().await;

    let pool = pool().await;
    SqlxAuditLog::new(db().await)
        .record(AuditEvent::failure(None, "auth.login", "user", EMAIL))
        .await
        .unwrap();

    let report = SqlxAnonymizationRepository::new(db().await)
        .anonymize(UserId(uid), Utc::now())
        .await
        .unwrap();
    assert!(report.audit_targets_anonymized >= 1);

    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE target_type = 'user' AND target_id = $1",
    )
    .bind(EMAIL)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0, "the audit trail still names the deleted account");
    let rewritten: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE target_type = 'user' AND target_id = $1",
    )
    .bind(format!("deleted+{uid}@bikesnest.invalid"))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(rewritten >= 1, "the row must survive, anonymized");

    let mut audit_tx = bikesnest_test_support::audit_mutation_tx(&pool).await;
    sqlx::query("DELETE FROM audit_events WHERE target_id = $1")
        .bind(format!("deleted+{uid}@bikesnest.invalid"))
        .execute(&mut *audit_tx)
        .await
        .unwrap();
    audit_tx.commit().await.unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
}

#[db_test]
async fn audit_metadata_keys_stay_within_the_classified_allowlist(
    tx: &mut bikesnest_test_support::TestTx,
) {
    // `privacy/anonymize.rs` does not scrub `audit_events.metadata`, and that
    // is only correct while no key there can hold personal data. This is the
    // check that makes the assumption fail loudly: a new key must be added to
    // `AUDIT_METADATA_KEYS` (i.e. classified) or scrubbed.
    let _ = tx;
    let live: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT jsonb_object_keys(metadata) FROM audit_events \
         WHERE metadata <> '{}'::jsonb",
    )
    .fetch_all(&pool().await)
    .await
    .unwrap();
    let unclassified: Vec<&String> = live
        .iter()
        .filter(|k| !AUDIT_METADATA_KEYS.contains(&k.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "unclassified audit metadata keys {unclassified:?} — add them to \
         AUDIT_METADATA_KEYS after checking they hold no personal data, or \
         scrub them in privacy/anonymize.rs"
    );
}

/// Takes the shared ADMIN-set lock, empties the set, and hands back what was
/// in it for [`restore_admin_set`].
///
/// "Never zero administrators" is a whole-table property, so a test that needs
/// a known admin count has to own the table for its duration — see
/// [`bikesnest_test_support::admin_set_lock`].
///
/// **A test using this must not assert until it has restored.** The park is a
/// committed DELETE (the repository reads it from another connection, so it
/// cannot be a rollback-on-drop transaction), which means a panic between park
/// and restore leaves the database with no administrators. Collect every
/// observation into locals, restore, release the lock, and assert last.
async fn park_admin_set(
    pool: &sqlx::PgPool,
    lock: &mut sqlx::Transaction<'static, sqlx::Postgres>,
) -> Vec<(i64, Option<i64>)> {
    let parked: Vec<(i64, Option<i64>)> =
        sqlx::query_as("SELECT user_id, granted_by FROM user_roles WHERE role = 'ADMIN'")
            .fetch_all(&mut **lock)
            .await
            .unwrap();
    sqlx::query("DELETE FROM user_roles WHERE role = 'ADMIN'")
        .execute(pool)
        .await
        .unwrap();
    parked
}

/// Puts back what [`park_admin_set`] took out, skipping accounts another test
/// has hard-deleted in the meantime (`user_roles` cascades from `users`, and a
/// `granted_by` may have gone the same way).
async fn restore_admin_set(pool: &sqlx::PgPool, parked: Vec<(i64, Option<i64>)>) {
    sqlx::query("DELETE FROM user_roles WHERE role = 'ADMIN'")
        .execute(pool)
        .await
        .unwrap();
    for (user_id, granted_by) in parked {
        sqlx::query(
            "INSERT INTO user_roles (user_id, role, granted_by) \
             SELECT $1, 'ADMIN', (SELECT id FROM users WHERE id = $2) \
             WHERE EXISTS (SELECT 1 FROM users WHERE id = $1) \
             ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(granted_by)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// Deletes every committed fixture location a test tagged with `tag`
/// (`seed_key`), and with them — through the cascade — its photos and reviews.
async fn drop_locations(pool: &sqlx::PgPool, tag: &str) {
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(tag)
        .execute(pool)
        .await
        .unwrap();
}

/// Deletes the fixture accounts a test created, by email or by the anonymized
/// form their email becomes.
async fn drop_users(pool: &sqlx::PgPool, emails: &[&str], ids: &[i64]) {
    for email in emails {
        sqlx::query("DELETE FROM users WHERE email = $1")
            .bind(email)
            .execute(pool)
            .await
            .unwrap();
    }
    for id in ids {
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }
}

#[db_test]
async fn concurrent_deletions_of_the_last_two_admins_cannot_both_win(
    tx: &mut bikesnest_test_support::TestTx,
) {
    // Two admins deleting their accounts at the same moment used to both read
    // "another admin exists" and both proceed, leaving the system with none.
    // The guard now runs inside the anonymize transaction, holding `FOR UPDATE`
    // on the ADMIN rows, so the two serialize and the second sees the first's
    // commit.
    let emails = [
        unique_tag("wp16-race-a") + "@example.com",
        unique_tag("wp16-race-b") + "@example.com",
    ];
    let refs: Vec<&str> = emails.iter().map(String::as_str).collect();
    let pool = pool().await;
    drop_users(&pool, &refs, &[]).await;

    let mut admin_lock = bikesnest_test_support::admin_set_lock(&pool).await;
    let parked = park_admin_set(&pool, &mut admin_lock).await;

    let mut ids = Vec::new();
    for email in &emails {
        let u = UserBuilder::new()
            .with_email(email)
            .create(tx.executor())
            .await
            .unwrap();
        ids.push(u.id.0);
    }
    tx.commit_fixture().await;
    for id in &ids {
        sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'ADMIN')")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    let repo_a = SqlxAnonymizationRepository::new(db().await);
    let repo_b = SqlxAnonymizationRepository::new(db().await);
    let now = Utc::now();
    let (a, b) = tokio::join!(
        repo_a.anonymize(UserId(ids[0]), now),
        repo_b.anonymize(UserId(ids[1]), now),
    );
    let refused = [&a, &b]
        .iter()
        .filter(|r| matches!(r, Err(PrivacyError::LastAdmin)))
        .count();
    let admins_left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE role = 'ADMIN'")
            .fetch_one(&pool)
            .await
            .unwrap();

    // Restore first, assert second: an assertion that fires before the restore
    // would leave the database with no administrators (see `park_admin_set`).
    restore_admin_set(&pool, parked).await;
    drop_users(&pool, &refs, &ids).await;
    admin_lock.rollback().await.unwrap();

    assert_eq!(
        refused, 1,
        "exactly one deletion must be refused as the last admin (a={a:?}, b={b:?})"
    );
    assert_eq!(
        admins_left, 1,
        "the system must never be left without an admin"
    );
}

#[db_test]
async fn audit_events_are_append_only_but_purgeable(tx: &mut bikesnest_test_support::TestTx) {
    let _ = tx;
    let pool = pool().await;
    // Dated in the distant past so the purge below can name a cutoff that
    // covers this row and nothing else: `purge_audit_events_before` is a
    // whole-table DELETE, and the suite shares one database.
    let ancient = DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO audit_events \
         (actor_user_id, action, target_type, target_id, result, metadata, created_at) \
         VALUES (NULL, 'wp16.immutability.probe', 'system', 'probe', 'success', '{}'::jsonb, $1) \
         RETURNING id",
    )
    .bind(ancient)
    .fetch_one(&pool)
    .await
    .unwrap();

    // A content edit is refused …
    let err = sqlx::query("UPDATE audit_events SET result = 'failure' WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect_err("audit rows must not be editable");
    assert!(
        err.to_string().contains("append-only"),
        "unexpected error: {err}"
    );
    // … and so is a bare delete.
    let err = sqlx::query("DELETE FROM audit_events WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .expect_err("audit rows must not be deletable");
    assert!(
        err.to_string().contains("append-only"),
        "unexpected error: {err}"
    );

    // The sanctioned purge works, and reports what it removed.
    let removed: i64 = sqlx::query_scalar("SELECT purge_audit_events_before($1)")
        .bind(ancient + Duration::days(1))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(removed >= 1, "the purge function must delete rows");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[db_test]
async fn orphan_sweep_deletes_aged_unreferenced_objects_only(
    tx: &mut bikesnest_test_support::TestTx,
) {
    const TAG: &str = "wp16-orphan";
    drop_locations(&pool().await, TAG).await;
    let loc = ParkingBuilder::new()
        .with_name("WP16 Orphan")
        .with_fixture_tag(TAG)
        .create(tx.executor())
        .await
        .unwrap();
    let referenced = "uploads/wp16-referenced/full.jpg";
    sqlx::query(
        "INSERT INTO parking_photo (location_id, storage_key, content_type, position, \
         moderation_state) VALUES ($1, $2, 'image/jpeg', 0, 'APPROVED')",
    )
    .bind(loc.id())
    .bind(referenced)
    .execute(tx.executor())
    .await
    .unwrap();
    tx.commit_fixture().await;

    let policy = RetentionPolicy::default();
    let now = Utc::now();
    let aged = now - policy.upload_orphan_ttl - Duration::hours(1);

    let storage = std::sync::Arc::new(TestObjectStorage::new());
    storage.seed_aged("uploads/wp16-orphan/full.jpg", aged);
    storage.seed_aged("uploads/wp16-orphan/thumb.jpg", aged);
    storage.seed_aged(referenced, aged);
    storage.seed_aged("uploads/wp16-young/full.jpg", now);
    // Not under `uploads/` — the seeded dev dataset is never swept.
    storage.seed_aged("seed/curitiba/bike.jpg", aged);
    // Force pagination, so the loop (not just one page) is exercised.
    storage.set_page_size(2);

    let repo = SqlxRetentionRepository::new(db().await, policy, storage.clone());
    let purged = repo.purge_orphan_uploads(now).await.unwrap();

    assert_eq!(purged, 2, "only the two aged, unreferenced uploads");
    assert!(!storage.contains("uploads/wp16-orphan/full.jpg"));
    assert!(!storage.contains("uploads/wp16-orphan/thumb.jpg"));
    assert!(
        storage.contains(referenced),
        "a referenced key must survive"
    );
    assert!(
        storage.contains("uploads/wp16-young/full.jpg"),
        "an object inside the orphan TTL must survive"
    );
    assert!(
        storage.contains("seed/curitiba/bike.jpg"),
        "objects outside uploads/ are out of scope"
    );

    drop_locations(&pool().await, TAG).await;
}

#[db_test]
async fn orphan_sweep_propagates_a_listing_failure(tx: &mut bikesnest_test_support::TestTx) {
    // The old filesystem sweep swallowed its `read_dir` error and returned
    // `Ok(0)`, so media retention was a silent no-op for as long as the
    // directory was missing. A store that cannot be listed must be an error.
    let _ = tx;
    let storage = std::sync::Arc::new(TestObjectStorage::new());
    storage.fail_list();
    let repo = SqlxRetentionRepository::new(db().await, RetentionPolicy::default(), storage);
    let err = repo
        .purge_orphan_uploads(Utc::now())
        .await
        .expect_err("a failing list must not report a successful zero");
    assert!(matches!(err, PrivacyError::Unavailable), "got {err:?}");
}

#[db_test]
async fn reconcile_drops_aged_pending_rows_with_no_object(tx: &mut bikesnest_test_support::TestTx) {
    const TAG: &str = "wp16-reconcile";
    drop_locations(&pool().await, TAG).await;
    let loc = ParkingBuilder::new()
        .with_name("WP16 Reconcile")
        .with_fixture_tag(TAG)
        .create(tx.executor())
        .await
        .unwrap();
    let stored = "uploads/wp16-recon-ok/full.jpg";
    let missing = "uploads/wp16-recon-gone/full.jpg";
    let young = "uploads/wp16-recon-young/full.jpg";
    for (key, created) in [
        (stored, Utc::now() - Duration::hours(3)),
        (missing, Utc::now() - Duration::hours(3)),
        (young, Utc::now()),
    ] {
        sqlx::query(
            "INSERT INTO parking_photo (location_id, storage_key, content_type, position, \
             moderation_state, created_at) \
             VALUES ($1, $2, 'image/jpeg', 0, 'PENDING_REVIEW', $3)",
        )
        .bind(loc.id())
        .bind(key)
        .bind(created)
        .execute(tx.executor())
        .await
        .unwrap();
    }
    tx.commit_fixture().await;

    let storage = std::sync::Arc::new(TestObjectStorage::new());
    storage.seed(stored, b"full", "image/jpeg");
    storage.seed(young, b"full", "image/jpeg");

    let repo = SqlxRetentionRepository::new(db().await, RetentionPolicy::default(), storage);
    let deleted = repo.reconcile_pending_photos(Utc::now()).await.unwrap();
    assert_eq!(deleted, 1, "only the aged row whose object is gone");

    let pool = pool().await;
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT storage_key FROM parking_photo WHERE location_id = $1")
            .bind(loc.id())
            .fetch_all(&pool)
            .await
            .unwrap();
    assert!(keys.contains(&stored.to_string()));
    assert!(
        keys.contains(&young.to_string()),
        "a row inside the grace period is left alone"
    );
    assert!(!keys.contains(&missing.to_string()));

    drop_locations(&pool, TAG).await;
}

#[db_test]
async fn revoke_role_guarded_serializes_on_the_locked_admin_rows(
    tx: &mut bikesnest_test_support::TestTx,
) {
    // The guard and the delete are one transaction that takes `FOR UPDATE` on
    // the ADMIN rows. That is what makes the count it reads true at the moment
    // it deletes: a second guarded revoke cannot run between them. Proof: hold
    // those rows locked from outside and the call cannot make progress; release
    // the lock and it completes.
    //
    // (The refusal itself — "this would leave zero admins" — is asserted
    // deterministically in `crates/application/tests/auth_test.rs`, where the
    // admin set is the test's own.)
    use bikesnest_application::AccountRepository;
    use bikesnest_domain::Role;

    const EMAILS: [&str; 2] = [
        "wp16-forupdate-a@example.com",
        "wp16-forupdate-b@example.com",
    ];
    let pool = pool().await;
    drop_users(&pool, &EMAILS, &[]).await;

    // This test adds ADMIN rows, so it takes the same shared lock as the tests
    // that need to own the set — otherwise the two perturb each other's counts
    // and deadlock on the row locks they each take.
    let admin_lock = bikesnest_test_support::admin_set_lock(&pool).await;

    let mut ids = Vec::new();
    for email in EMAILS {
        let u = UserBuilder::new()
            .with_email(email)
            .create(tx.executor())
            .await
            .unwrap();
        ids.push(u.id.0);
    }
    tx.commit_fixture().await;
    // Two extra admins, so the revoke below is never refused — this test is
    // about the lock, not the refusal.
    for id in &ids {
        sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'ADMIN')")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }

    // Lock every ADMIN row from a separate transaction.
    let mut blocker = pool.begin().await.unwrap();
    let _: Vec<i64> =
        sqlx::query_scalar("SELECT user_id FROM user_roles WHERE role = 'ADMIN' FOR UPDATE")
            .fetch_all(&mut *blocker)
            .await
            .unwrap();

    let repo = bikesnest_infrastructure::SqlxAccountRepository::new(db().await);
    let target = UserId(ids[0]);
    let mut revoke = Box::pin(repo.revoke_role_guarded(target, Role::Admin));
    let blocked = tokio::time::timeout(std::time::Duration::from_millis(300), &mut revoke).await;
    assert!(
        blocked.is_err(),
        "the guarded revoke must wait for the ADMIN row locks, got {blocked:?}"
    );

    // Release the lock; the revoke now completes against the state it locked.
    blocker.rollback().await.unwrap();
    assert!(revoke.await.unwrap(), "the revoke removes a row");
    let still_admin: i64 =
        sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE role = 'ADMIN' AND user_id = $1")
            .bind(target.0)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(still_admin, 0);

    drop_users(&pool, &EMAILS, &ids).await;
    admin_lock.rollback().await.unwrap();
}

#[db_test]
async fn revoke_role_guarded_refuses_the_sole_admin_in_sql(
    tx: &mut bikesnest_test_support::TestTx,
) {
    // The refusal itself, against the real SQL rather than a hand-written
    // mirror of it. Without this, weakening the repository's own comparison
    // (`admins.len() <= 1` → `<= 0`) passes the whole suite: the FOR UPDATE
    // test deliberately seeds a second admin so the revoke is never refused,
    // and the application-level test drives a fake repository.
    use bikesnest_application::AccountRepository;
    use bikesnest_domain::Role;

    let sole_email = unique_tag("wp16-sql-sole") + "@example.com";
    let second_email = unique_tag("wp16-sql-second") + "@example.com";
    let refs = [sole_email.as_str(), second_email.as_str()];
    let pool = pool().await;
    drop_users(&pool, &refs, &[]).await;

    let mut admin_lock = bikesnest_test_support::admin_set_lock(&pool).await;
    let parked = park_admin_set(&pool, &mut admin_lock).await;

    let sole = UserBuilder::new()
        .with_email(&sole_email)
        .create(tx.executor())
        .await
        .unwrap();
    let second = UserBuilder::new()
        .with_email(&second_email)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    for (id, role) in [(sole.id.0, "ADMIN"), (second.id.0, "MODERATOR")] {
        sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, $2)")
            .bind(id)
            .bind(role)
            .execute(&pool)
            .await
            .unwrap();
    }

    let repo = bikesnest_infrastructure::SqlxAccountRepository::new(db().await);
    let count_role = async |user_id: i64, role: &str| -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE user_id = $1 AND role = $2")
            .bind(user_id)
            .bind(role)
            .fetch_one(&pool)
            .await
            .unwrap()
    };

    // Every observation is collected before anything is asserted: the park
    // above is a committed DELETE, so a panic here would leave the database
    // with no administrators (see `park_admin_set`).
    let only_admin = count_role(sole.id.0, "ADMIN").await;

    // 1) The sole admin cannot be demoted, and the row survives the attempt.
    let refusal = repo.revoke_role_guarded(sole.id, Role::Admin).await;
    let kept_after_refusal = count_role(sole.id.0, "ADMIN").await;

    // 2) A MODERATOR revoke removes no admin, so the admin count must not gate
    //    it — not even while there is exactly one admin.
    let moderator_revoke = repo.revoke_role_guarded(second.id, Role::Moderator).await;
    let moderator_left = count_role(second.id.0, "MODERATOR").await;

    // 3) With a second admin present the same call succeeds and the row goes.
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'ADMIN')")
        .bind(second.id.0)
        .execute(&pool)
        .await
        .unwrap();
    let second_revoke = repo.revoke_role_guarded(sole.id, Role::Admin).await;
    let sole_admin_left = count_role(sole.id.0, "ADMIN").await;
    let admins_left: i64 =
        sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE role = 'ADMIN'")
            .fetch_one(&pool)
            .await
            .unwrap();

    restore_admin_set(&pool, parked).await;
    drop_users(&pool, &refs, &[sole.id.0, second.id.0]).await;
    admin_lock.rollback().await.unwrap();

    assert_eq!(only_admin, 1, "the fixture must be the only admin");
    assert!(
        matches!(
            refusal,
            Err(bikesnest_application::AuthError::RefuseAdminSelfRevoke)
        ),
        "demoting the only admin must be refused, got {refusal:?}"
    );
    assert_eq!(
        kept_after_refusal, 1,
        "a refused revoke must not delete the row"
    );
    assert!(
        moderator_revoke.expect("a moderator revoke must not error"),
        "a non-admin revoke must not be blocked by the admin count"
    );
    assert_eq!(moderator_left, 0);
    assert!(
        second_revoke.expect("with two admins the revoke must not be refused"),
        "the revoke must remove the row"
    );
    assert_eq!(sole_admin_left, 0);
    assert_eq!(admins_left, 1, "the system is never left without an admin");
}
