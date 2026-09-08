//! Moderation infrastructure tests: the report repo state machine, the
//! moderation actions (proposal apply + supersede, parking invalidate revision),
//! and the audit-log reader filter/pagination. Approval scenarios use automatic
//! transaction rollback; legacy scenarios still use committed pooled fixtures.

use bikesnest_application::{
    AuditFilter, AuditLogReader, ModerationError, ModerationRepository, NewReport,
    ProposalApplication, ReportRepository,
};
use bikesnest_domain::{
    ModerationState, ProposedChange, ReportDescription, ReportOutcome, ReportState,
    ReportTargetType, UserId,
};
use bikesnest_infrastructure::{
    Db, SqlxAuditLogReader, SqlxModerationRepository, SqlxReportRepository,
};
use bikesnest_test_support::{ParkingBuilder, UserBuilder, db_test, pool};

async fn db() -> Db {
    Db::from_pool(pool().await)
}

/// Commit a user (with the given role) so repo writes see it on other connections.
async fn committed_user(tx: &mut bikesnest_test_support::TestTx, email: &str, role: &str) -> i64 {
    let user = UserBuilder::new()
        .with_email(email)
        .create(tx.executor())
        .await
        .unwrap();
    if role != "USER" {
        sqlx::query("INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, $2, NULL)")
            .bind(user.id.0)
            .bind(role)
            .execute(tx.executor())
            .await
            .unwrap();
    }
    tx.commit_fixture().await;
    user.id.0
}

#[db_test]
async fn report_repo_state_machine(tx: &mut bikesnest_test_support::TestTx) {
    let reporter = committed_user(tx, "m5-infra-rep@example.com", "USER").await;
    let moderator = committed_user(tx, "m5-infra-mod@example.com", "MODERATOR").await;
    let repo = SqlxReportRepository::new(db().await);
    let report_id = repo
        .create(&NewReport {
            reporter_id: UserId(reporter),
            target_type: ReportTargetType::Parking,
            target_id: 1,
            reason: "duplicate".to_string(),
            description: ReportDescription::new("dup").expect("in-range description"),
        })
        .await
        .unwrap();

    let got = repo.get(report_id).await.unwrap().unwrap();
    assert_eq!(got.state, ReportState::Open);
    assert_eq!(got.reporter_id, Some(UserId(reporter)));

    // Claim: OPEN → UNDER_REVIEW.
    repo.claim(report_id, UserId(moderator)).await.unwrap();
    let got = repo.get(report_id).await.unwrap().unwrap();
    assert_eq!(got.state, ReportState::UnderReview);
    assert_eq!(got.claimed_by, Some(UserId(moderator)));

    // Claiming again (not OPEN) → InvalidState.
    assert!(matches!(
        repo.claim(report_id, UserId(moderator)).await,
        Err(ModerationError::InvalidState)
    ));

    // Resolve → RESOLVED.
    repo.resolve(
        report_id,
        UserId(moderator),
        "hidden",
        ReportOutcome::Resolved,
    )
    .await
    .unwrap();
    let got = repo.get(report_id).await.unwrap().unwrap();
    assert_eq!(got.state, ReportState::Resolved);
    assert_eq!(got.resolution_note.as_deref(), Some("hidden"));

    // Previous state-listed rows reflect the filter.
    let open = repo.list(Some(ReportState::Open), None, 50).await.unwrap();
    assert!(open.iter().all(|r| r.state == ReportState::Open));

    let _ = tx;
    sqlx::query("DELETE FROM report WHERE reporter_id = $1")
        .bind(reporter)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id IN ($1, $2)")
        .bind(reporter)
        .bind(moderator)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn parking_invalidate_writes_moderation_revision(tx: &mut bikesnest_test_support::TestTx) {
    let moderator = committed_user(tx, "m5-infra-mod2@example.com", "MODERATOR").await;
    const MARK: &str = "m5-infra-inv";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Infra Invalidate")
        .with_fixture_tag(MARK)
        .with_version(1)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    let id = loc.id();

    let repo = SqlxModerationRepository::new(db().await);
    repo.set_parking_state(
        id,
        &[ModerationState::Active],
        ModerationState::Invalid,
        UserId(moderator),
    )
    .await
    .unwrap();

    let (state, version): (String, i64) =
        sqlx::query_as("SELECT moderation_state, version FROM parking_location WHERE id = $1")
            .bind(id)
            .fetch_one(&pool().await)
            .await
            .unwrap();
    assert_eq!(state, "INVALID");
    assert_eq!(version, 2, "version bumped on moderation");

    let (rev_kind, rev_version): (String, i64) = sqlx::query_as(
        "SELECT change_kind, version FROM parking_revision WHERE location_id = $1 AND change_kind = 'moderation'")
        .bind(id).fetch_one(&pool().await).await.unwrap();
    assert_eq!(rev_kind, "moderation");
    assert_eq!(rev_version, 2);

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(moderator)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn proposal_approve_applies_change_supersedes_and_writes_revision(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let moderator = UserBuilder::new()
        .with_email("approval-publish-mod@example.com")
        .create(&mut *conn)
        .await
        .unwrap()
        .id;
    let proposer = UserBuilder::new()
        .with_email("approval-publish-author@example.com")
        .create(&mut *conn)
        .await
        .unwrap()
        .id;
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'MODERATOR')")
        .bind(moderator.0)
        .execute(&mut *conn)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Infra Proposal")
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let id = loc.id();

    // Two PENDING proposals on the same location: an older existence-removal and
    // a newer one. Approving one must supersede the other.
    let (p1,): (i64,) = sqlx::query_as(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
         VALUES ($1, $2, 1, 'change_existence', '{\"existence\":\"removed\"}', 'PENDING') RETURNING id")
        .bind(id).bind(proposer.0).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    let (p2,): (i64,) = sqlx::query_as(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
         VALUES ($1, $2, 1, 'change_existence', '{\"existence\":\"removed\"}', 'PENDING') RETURNING id")
        .bind(id).bind(proposer.0).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();

    let repo = SqlxModerationRepository::new(db.clone());
    repo.approve_proposal(
        p1,
        moderator,
        ProposalApplication::ChangeExistence { exists: false },
    )
    .await
    .unwrap();

    let (lstate, lversion): (String, i64) =
        sqlx::query_as("SELECT moderation_state, version FROM parking_location WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(lstate, "REMOVED", "approved existence-removal sets REMOVED");
    assert_eq!(lversion, 2);

    let (p1_status,): (String,) =
        sqlx::query_as("SELECT status FROM parking_proposal WHERE id = $1")
            .bind(p1)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(p1_status, "APPROVED");
    // The other PENDING proposal on the same location is superseded.
    let (p2_status,): (String,) =
        sqlx::query_as("SELECT status FROM parking_proposal WHERE id = $1")
            .bind(p2)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(p2_status, "SUPERSEDED");

    let (rev,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM parking_revision WHERE location_id = $1 AND change_kind = 'moderation' AND version = 2")
        .bind(id).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(rev, 1, "approval writes a moderation revision");
}

#[db_test]
async fn proposal_approve_refuses_a_stale_base_version(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let moderator = UserBuilder::new()
        .with_email("approval-stale-mod@example.com")
        .create(&mut *conn)
        .await
        .unwrap()
        .id;
    let proposer = UserBuilder::new()
        .with_email("approval-stale-author@example.com")
        .create(&mut *conn)
        .await
        .unwrap()
        .id;
    sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'MODERATOR')")
        .bind(moderator.0)
        .execute(&mut *conn)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Infra Stale Proposal")
        .with_version(5)
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let id = loc.id();

    // Proposed against v3, but the location has moved on to v5.
    let (stale,): (i64,) = sqlx::query_as(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
         VALUES ($1, $2, 3, 'change_existence', '{\"existence\":\"removed\"}', 'PENDING') RETURNING id")
        .bind(id).bind(proposer.0).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();

    let repo = SqlxModerationRepository::new(db.clone());
    let err = repo
        .approve_proposal(
            stale,
            moderator,
            ProposalApplication::ChangeExistence { exists: false },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, ModerationError::StaleProposal),
        "expected StaleProposal, got {err:?}"
    );

    // Nothing changed: same state, same version, no revision, still PENDING.
    let (state, version): (String, i64) =
        sqlx::query_as("SELECT moderation_state, version FROM parking_location WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(state, "ACTIVE");
    assert_eq!(version, 5);
    let (revs,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM parking_revision WHERE location_id = $1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(revs, 0, "a refused approval writes no revision");
    let (status,): (String,) = sqlx::query_as("SELECT status FROM parking_proposal WHERE id = $1")
        .bind(stale)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(status, "PENDING");

    // A proposal made against the current version still applies.
    let (fresh,): (i64,) = sqlx::query_as(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
         VALUES ($1, $2, 5, 'change_existence', '{\"existence\":\"removed\"}', 'PENDING') RETURNING id")
        .bind(id).bind(proposer.0).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    repo.approve_proposal(
        fresh,
        moderator,
        ProposalApplication::ChangeExistence { exists: false },
    )
    .await
    .unwrap();
    let (state, version): (String, i64) =
        sqlx::query_as("SELECT moderation_state, version FROM parking_location WHERE id = $1")
            .bind(id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(state, "REMOVED");
    assert_eq!(version, 6);
    // Approving it superseded the stale sibling.
    let (status,): (String,) = sqlx::query_as("SELECT status FROM parking_proposal WHERE id = $1")
        .bind(stale)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(status, "SUPERSEDED");
}

#[db_test]
async fn report_dedupe_index_rejects_a_second_open_report(tx: &mut bikesnest_test_support::TestTx) {
    let reporter = committed_user(tx, "m5-infra-dupe-a@example.com", "USER").await;
    let other = committed_user(tx, "m5-infra-dupe-b@example.com", "USER").await;
    let moderator = committed_user(tx, "m5-infra-dupe-mod@example.com", "MODERATOR").await;
    let repo = SqlxReportRepository::new(db().await);

    let new = |who: i64| NewReport {
        reporter_id: UserId(who),
        target_type: ReportTargetType::Parking,
        target_id: 424_242,
        reason: "duplicate".to_string(),
        description: ReportDescription::new("dup").expect("in-range description"),
    };

    let first = repo.create(&new(reporter)).await.unwrap();

    // Same reporter, same target, still open → the partial unique index fires
    // and `db_err` classifies it as a Conflict (the service maps it onward).
    let err = repo.create(&new(reporter)).await.unwrap_err();
    assert!(
        matches!(err, ModerationError::Conflict),
        "expected Conflict, got {err:?}"
    );

    // A different reporter on the same target is a distinct signal.
    let second_reporter = repo.create(&new(other)).await.unwrap();

    // Once the first is resolved, the same reporter may report it again.
    repo.claim(first, UserId(moderator)).await.unwrap();
    repo.resolve(first, UserId(moderator), "done", ReportOutcome::Resolved)
        .await
        .unwrap();
    let third = repo.create(&new(reporter)).await.unwrap();
    assert_ne!(third, first);

    let _ = tx;
    let _ = second_reporter;
    sqlx::query("DELETE FROM report WHERE reporter_id = ANY($1)")
        .bind(vec![reporter, other])
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = ANY($1)")
        .bind(vec![reporter, other, moderator])
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn audit_reader_filters_and_paginates(tx: &mut bikesnest_test_support::TestTx) {
    let actor = committed_user(tx, "m5-infra-audit@example.com", "USER").await;
    let reader = SqlxAuditLogReader::new(db().await);
    // Insert a batch of audit events, then filter by action + keyset paginate.
    for i in 0..5 {
        sqlx::query(
            "INSERT INTO audit_events (actor_user_id, action, target_type, target_id, result, metadata) \
             VALUES ($1, $2, $3, $4, 'success', '{}'::jsonb)",
        )
        .bind(actor)
        .bind(if i % 2 == 0 { "mod.foo" } else { "mod.bar" })
        .bind("report")
        .bind(i.to_string())
        .execute(&pool().await)
        .await
        .unwrap();
    }
    let page = reader
        .list(AuditFilter {
            action: Some("mod.foo".to_string()),
            actor_id: Some(UserId(actor)),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(page.items.len() >= 3, "filter by action, actor");
    assert!(page.items.iter().all(|e| e.event.action == "mod.foo"));

    // Keyset pagination returns a next_cursor and a strictly smaller id set.
    let first = reader
        .list(AuditFilter {
            limit: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(first.items.len(), 2);
    assert!(first.next_cursor.is_some());
    let second = reader
        .list(AuditFilter {
            cursor: first.next_cursor,
            limit: 2,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        second.items.iter().all(|e| e.id < first.items[0].id),
        "keyset id DESC"
    );

    let _ = tx;
    let mut audit_tx = bikesnest_test_support::audit_mutation_tx(&pool().await).await;
    sqlx::query("DELETE FROM audit_events WHERE actor_user_id = $1")
        .bind(actor)
        .execute(&mut *audit_tx)
        .await
        .unwrap();
    audit_tx.commit().await.unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(actor)
        .execute(&pool().await)
        .await
        .unwrap();
}

// Note: these tests require the migration applied (0010/0011). The suite's
// `sqlx::migrate!` runs them on startup.

#[db_test]
async fn report_list_keyset_pagination_is_disjoint_and_stable(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let reporter = committed_user(tx, "m5-infra-report-keyset@example.com", "USER").await;
    let repo = SqlxReportRepository::new(db().await);

    let mut ids = Vec::new();
    for i in 0..5 {
        let id = repo
            .create(&NewReport {
                reporter_id: UserId(reporter),
                target_type: ReportTargetType::Parking,
                target_id: 900_000 + i,
                reason: "duplicate".to_string(),
                description: ReportDescription::new("keyset test").expect("in-range description"),
            })
            .await
            .unwrap();
        ids.push(id);
    }
    // Force identical `created_at` across the fixture rows: the queue orders
    // by `id ASC` alone now, so ties on the old `created_at` tiebreak must not
    // disturb the order or the keyset cursor.
    sqlx::query("UPDATE report SET created_at = now() WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&pool().await)
        .await
        .unwrap();

    // Two 2-item pages never overlap and stay in ascending id order — true
    // globally (this table also holds unrelated seed/fixture rows), which is
    // exactly the keyset-pagination contract under test.
    let page1 = repo.list(None, None, 2).await.unwrap();
    let page1_ids: Vec<i64> = page1.iter().map(|r| r.id).collect();
    assert_eq!(page1_ids.len(), 2);
    assert!(
        page1_ids.windows(2).all(|w| w[0] < w[1]),
        "ascending id order"
    );

    let cursor = *page1_ids.last().unwrap();
    let page2 = repo.list(None, Some(cursor), 2).await.unwrap();
    let page2_ids: Vec<i64> = page2.iter().map(|r| r.id).collect();
    assert_eq!(page2_ids.len(), 2);
    assert!(
        page2_ids.iter().all(|id| !page1_ids.contains(id)),
        "pages are disjoint"
    );
    assert!(
        page2_ids[0] > cursor,
        "page 2 starts strictly after the cursor"
    );

    let _ = tx;
    sqlx::query("DELETE FROM report WHERE reporter_id = $1")
        .bind(reporter)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(reporter)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn proposal_list_keyset_pagination_is_disjoint_and_stable(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let proposer = committed_user(tx, "m5-infra-prop-keyset@example.com", "USER").await;
    const MARK: &str = "m5-infra-prop-keyset";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Infra Proposal Keyset")
        .with_fixture_tag(MARK)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    let id = loc.id();

    let mut ids = Vec::new();
    for _ in 0..5 {
        let (pid,): (i64,) = sqlx::query_as(
            "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
             VALUES ($1, $2, 1, 'change_existence', '{\"existence\":\"removed\"}', 'PENDING') RETURNING id")
            .bind(id).bind(proposer).fetch_one(&pool().await).await.unwrap();
        ids.push(pid);
    }
    sqlx::query("UPDATE parking_proposal SET created_at = now() WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&pool().await)
        .await
        .unwrap();

    let repo = SqlxModerationRepository::new(db().await);
    let page1 = repo.list_pending_proposals(None, 2).await.unwrap();
    let page1_ids: Vec<i64> = page1.iter().map(|p| p.id).collect();
    assert_eq!(page1_ids.len(), 2);
    assert!(
        page1_ids.windows(2).all(|w| w[0] < w[1]),
        "ascending id order"
    );

    let cursor = *page1_ids.last().unwrap();
    let page2 = repo.list_pending_proposals(Some(cursor), 2).await.unwrap();
    let page2_ids: Vec<i64> = page2.iter().map(|p| p.id).collect();
    assert_eq!(page2_ids.len(), 2);
    assert!(
        page2_ids.iter().all(|pid| !page1_ids.contains(pid)),
        "pages are disjoint"
    );
    assert!(
        page2_ids[0] > cursor,
        "page 2 starts strictly after the cursor"
    );

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(proposer)
        .execute(&pool().await)
        .await
        .unwrap();
}

/// `queue_counts()` reads four global tables the whole suite shares, so a
/// before/after delta taken via two separate pool connections can be thrown
/// off by another test's concurrent commits (this happened in practice: a
/// sibling test bulk-inserts 51 reports for its own "load more" fixture).
/// Eliminate the race by taking both reads — and inserting the fixture rows
/// — on one `REPEATABLE READ` transaction/connection: that transaction's
/// snapshot is fixed at `BEGIN`, so no concurrently-committing test can be
/// observed by either read, only this transaction's own inserts. Uses
/// `SqlxModerationRepository::queue_counts_on`, the exact same query
/// `ModerationRepository::queue_counts` runs against the pool, just pointed
/// at this connection instead.
#[db_test]
async fn queue_counts_on_reflects_an_exact_delta_race_free(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let moderator = committed_user(tx, "m5-infra-queue-counts@example.com", "MODERATOR").await;
    const MARK: &str = "m5-infra-queue-counts";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Infra Queue Counts")
        .with_fixture_tag(MARK)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    let id = loc.id();

    let pool_ref = pool().await;
    let mut isolated = pool_ref.begin().await.unwrap();
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ")
        .execute(&mut *isolated)
        .await
        .unwrap();

    let before = SqlxModerationRepository::queue_counts_on(&mut *isolated)
        .await
        .unwrap();

    // One fresh row in each of the four counted categories, on the same
    // isolated connection.
    sqlx::query(
        "INSERT INTO parking_photo (location_id, storage_key, content_type, moderation_state) \
         VALUES ($1, 'm5-infra-queue-counts/pending.jpg', 'image/jpeg', 'PENDING_REVIEW')",
    )
    .bind(id)
    .execute(&mut *isolated)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO report (reporter_id, target_type, target_id, reason, state) \
         VALUES ($1, 'parking', $2, 'other', 'OPEN')",
    )
    .bind(moderator)
    .bind(id)
    .execute(&mut *isolated)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO report (reporter_id, target_type, target_id, reason, state) \
         VALUES ($1, 'parking_photo', $2, 'other', 'UNDER_REVIEW')",
    )
    .bind(moderator)
    .bind(id)
    .execute(&mut *isolated)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
         VALUES ($1, $2, 1, 'change_existence', '{\"existence\":\"removed\"}', 'PENDING')",
    )
    .bind(id)
    .bind(moderator)
    .execute(&mut *isolated)
    .await
    .unwrap();

    let after = SqlxModerationRepository::queue_counts_on(&mut *isolated)
        .await
        .unwrap();

    assert_eq!(after.pending_photos - before.pending_photos, 1);
    assert_eq!(after.open_reports - before.open_reports, 1);
    assert_eq!(after.under_review_reports - before.under_review_reports, 1);
    assert_eq!(after.pending_proposals - before.pending_proposals, 1);

    // Rolled back, not committed: the fixture rows never become visible to
    // any other connection (including the pool-backed `queue_counts()`), so
    // no separate cleanup is needed for them.
    isolated.rollback().await.unwrap();

    let _ = tx;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(moderator)
        .execute(&pool().await)
        .await
        .unwrap();
}

// ---------------------------------------------------------------------------
// The proposal payload is parsed at the repository boundary, and the
// queue rows carry the location's current values so a diff needs no extra
// query. Legacy rows must keep reading correctly with no migration.
// ---------------------------------------------------------------------------

#[db_test]
async fn proposal_rows_parse_the_stored_payload_and_carry_current_values(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let author = committed_user(tx, "wp13-infra-payload@example.com", "USER").await;
    const MARK: &str = "wp13-infra-payload";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Payload Spot")
        .with_fixture_tag(MARK)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    let id = loc.id();
    let (version,): (i64,) = sqlx::query_as("SELECT version FROM parking_location WHERE id = $1")
        .bind(id)
        .fetch_one(&pool().await)
        .await
        .unwrap();

    // Exactly the three payload shapes the database holds: the seeded
    // existence row, a move written by the previous form, and a row this build
    // cannot interpret.
    let mut ids = Vec::new();
    for (kind, payload) in [
        ("change_existence", r#"{"existence": "exists"}"#),
        (
            "move_location",
            r#"{"lat": -25.4284, "lon": -49.2733, "timezone": "America/Sao_Paulo", "reason": "pin is off"}"#,
        ),
        ("change_existence", r#"{"existence": "from_the_future"}"#),
    ] {
        let (pid,): (i64,) = sqlx::query_as(
            "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
             VALUES ($1, $2, $3, $4, $5::jsonb, 'PENDING') RETURNING id",
        )
        .bind(id)
        .bind(author)
        .bind(version)
        .bind(kind)
        .bind(payload)
        .fetch_one(&pool().await)
        .await
        .unwrap();
        ids.push(pid);
    }

    let repo = SqlxModerationRepository::new(db().await);

    let legacy = repo.get_proposal(ids[0]).await.unwrap().expect("row");
    assert_eq!(
        legacy.change,
        ProposedChange::ChangeExistence { exists: true },
        "the seeded legacy payload still parses without a migration"
    );
    assert_eq!(legacy.reason, None);
    // The current values ride along, so the queue diffs without a second query.
    assert_eq!(legacy.location_name, "Payload Spot");
    assert_eq!(legacy.current_state, ModerationState::Active);
    assert_eq!(legacy.location_version, version);
    assert!(!legacy.is_stale(), "written against the live version");

    let moved = repo.get_proposal(ids[1]).await.unwrap().expect("row");
    assert_eq!(
        moved.change,
        ProposedChange::MoveLocation {
            lat: -25.4284,
            lon: -49.2733,
            timezone: Some("America/Sao_Paulo".to_string()),
        }
    );
    assert_eq!(
        moved.reason.as_deref(),
        Some("pin is off"),
        "the proposer's note is lifted out of the payload"
    );

    let unreadable = repo.get_proposal(ids[2]).await.unwrap().expect("row");
    assert_eq!(
        unreadable.change,
        ProposedChange::Unknown,
        "an unreadable payload becomes a value, not an error"
    );

    // The list path parses the same way (and reaches these rows via its cursor).
    let listed = repo
        .list_pending_proposals(Some(ids[0] - 1), 50)
        .await
        .unwrap();
    let mine: Vec<_> = listed.iter().filter(|p| ids.contains(&p.id)).collect();
    assert_eq!(
        mine.len(),
        3,
        "all three rows list without failing the page"
    );

    // A location that moves on makes the proposal stale, from the row alone.
    sqlx::query("UPDATE parking_location SET version = version + 3 WHERE id = $1")
        .bind(id)
        .execute(&pool().await)
        .await
        .unwrap();
    let now_stale = repo.get_proposal(ids[0]).await.unwrap().expect("row");
    assert!(
        now_stale.is_stale(),
        "base_version {} against location version {}",
        now_stale.base_version,
        now_stale.location_version
    );

    sqlx::query("DELETE FROM parking_proposal WHERE location_id = $1")
        .bind(id)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(author)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn report_previews_resolve_every_target_kind_to_its_location(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let author = committed_user(tx, "wp13-infra-preview@example.com", "USER").await;
    const MARK: &str = "wp13-infra-preview";
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    let loc = ParkingBuilder::new()
        .with_name("Preview Spot")
        .with_fixture_tag(MARK)
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    let id = loc.id();

    let long_body = format!("{}END", "review ".repeat(60));
    let (review,): (i64,) = sqlx::query_as(
        "INSERT INTO review (location_id, author_id, rating, body, moderation_state) \
         VALUES ($1, $2, 3, $3, 'ACTIVE') RETURNING id",
    )
    .bind(id)
    .bind(author)
    .bind(&long_body)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    let (photo,): (i64,) = sqlx::query_as(
        "INSERT INTO parking_photo (location_id, uploader_id, storage_key, content_type, position, moderation_state) \
         VALUES ($1, $2, 'uploads/wp13-preview.jpg', 'image/jpeg', 0, 'APPROVED') RETURNING id",
    )
    .bind(id)
    .bind(author)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    let (review_photo,): (i64,) = sqlx::query_as(
        "INSERT INTO review_photo (review_id, uploader_id, storage_key, position, moderation_state) \
         VALUES ($1, $2, 'uploads/wp13-preview-r.jpg', 0, 'APPROVED') RETURNING id",
    )
    .bind(review)
    .bind(author)
    .fetch_one(&pool().await)
    .await
    .unwrap();

    let repo = SqlxModerationRepository::new(db().await);
    let previews = repo
        .report_previews(&[
            (ReportTargetType::Parking, id),
            (ReportTargetType::Review, review),
            (ReportTargetType::ParkingPhoto, photo),
            (ReportTargetType::ReviewPhoto, review_photo),
            // A target that no longer exists must simply be absent.
            (ReportTargetType::Parking, -1),
        ])
        .await
        .unwrap();
    assert_eq!(
        previews.len(),
        4,
        "the deleted target is absent, not an error"
    );

    // Every kind resolves to the location a moderator recognizes.
    for key in [
        (ReportTargetType::Parking, id),
        (ReportTargetType::Review, review),
        (ReportTargetType::ParkingPhoto, photo),
        (ReportTargetType::ReviewPhoto, review_photo),
    ] {
        let p = &previews[&key];
        assert_eq!(p.location_id, Some(id), "{key:?}");
        assert_eq!(p.location_name.as_deref(), Some("Preview Spot"), "{key:?}");
        assert!(p.location_address.is_some(), "{key:?}");
    }

    let review_preview = &previews[&(ReportTargetType::Review, review)];
    let excerpt = review_preview.review_excerpt.as_deref().expect("excerpt");
    assert!(excerpt.starts_with("review review"));
    assert!(
        excerpt.chars().count() <= bikesnest_application::REVIEW_EXCERPT_CHARS + 1,
        "the excerpt is bounded: {} chars",
        excerpt.chars().count()
    );
    assert!(!excerpt.contains("END"), "the tail of a long body is cut");
    assert_eq!(review_preview.review_rating, Some(3));
    assert_eq!(review_preview.target_state.as_deref(), Some("ACTIVE"));

    let photo_preview = &previews[&(ReportTargetType::ParkingPhoto, photo)];
    assert_eq!(
        photo_preview.photo_key.as_deref(),
        Some("uploads/wp13-preview.jpg"),
        "the reported photo's key comes back so the queue can show it"
    );
    assert_eq!(photo_preview.target_state.as_deref(), Some("APPROVED"));

    // A review photo carries its parent review, so the link can anchor at it.
    let rp = &previews[&(ReportTargetType::ReviewPhoto, review_photo)];
    assert_eq!(rp.review_id, Some(review));
    assert_eq!(rp.photo_id, Some(review_photo));

    // Hiding the review changes the state the queue reads to pick its action.
    repo.hide_review(review, UserId(author)).await.unwrap();
    let previews = repo
        .report_previews(&[(ReportTargetType::Review, review)])
        .await
        .unwrap();
    assert_eq!(
        previews[&(ReportTargetType::Review, review)]
            .target_state
            .as_deref(),
        Some("HIDDEN")
    );

    // An empty request does no work.
    assert!(repo.report_previews(&[]).await.unwrap().is_empty());

    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(MARK)
        .execute(&pool().await)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(author)
        .execute(&pool().await)
        .await
        .unwrap();
}
