//! Sequential approval transactions share the rollback-scoped harness connection;
//! genuine approval and eligibility races own disposable databases.
use bikesnest_application::{
    AccountRepository, ContributionError, ModerationError, ModerationRepository, NewProposal,
    ParkingContributionRepository, ParkingDetailsReader, ProposalApplication, ProposalVote,
    SessionStore, TokenStore,
};
use bikesnest_domain::{
    Cost, CsrfToken, CurrencyCode, Money, OpeningHours, ParkingEdit, PricingUnit, ProposalKind,
    ProposalStatus, ProposedChange, SecurityFeature, SecurityState, SessionId, TimeRange, UserId,
    VerificationToken,
};
use bikesnest_infrastructure::{
    Db, SqlxAccountRepository, SqlxModerationRepository, SqlxParkingContributionRepository,
    SqlxParkingDetailsReader, SqlxSessionStore, SqlxTokenStore,
};
use bikesnest_test_support::{ParkingBuilder, UserBuilder, db_test, run_isolated_database_test};

async fn eligible(db: &Db, suffix: &str) -> UserId {
    let mut conn = db.acquire().await.unwrap();
    let user = UserBuilder::new()
        .with_email(format!("approval-{suffix}@example.com"))
        .create(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET account_state='ACTIVE', email_verified_at=now() WHERE id=$1")
        .bind(user.id.0)
        .execute(&mut *conn)
        .await
        .unwrap();
    user.id
}

async fn wait_for_lockers(pool: &sqlx::PgPool, expected: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE datname=current_database() AND wait_event_type='Lock'",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting >= expected {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("all competing operations must reach real database locks");
}

async fn proposal_with_five_votes(db: &Db, marker: &str) -> (i64, i64, UserId, ParkingEdit) {
    let proposer = eligible(db, &format!("{marker}-author")).await;
    let moderator = eligible(db, &format!("{marker}-moderator")).await;
    let mut conn = db.acquire().await.unwrap();
    let location = ParkingBuilder::new()
        .with_name(format!("Race {marker}"))
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let mut edit = ParkingEdit::from_location(&location);
    edit.name = format!("Published {marker}");
    let repo = SqlxParkingContributionRepository::new(db.clone());
    let proposal = repo
        .create_proposal(&NewProposal {
            location_id: location.id(),
            proposer_id: proposer,
            base_version: 1,
            kind: ProposalKind::EditDetails,
            proposed: edit.to_json(),
        })
        .await
        .unwrap();
    for index in 0..5 {
        let voter = eligible(db, &format!("{marker}-voter-{index}")).await;
        repo.vote_on_proposal(proposal, voter, ProposalVote::Approve)
            .await
            .unwrap();
    }
    (location.id(), proposal, moderator, edit)
}

#[db_test]
async fn ineligible_accounts_cannot_submit_or_vote_and_old_votes_stop_counting(tx: &mut TestTx) {
    let db = tx.db().await;
    let author = eligible(&db, "eligibility-author").await;
    let voter = eligible(&db, "eligibility-voter").await;
    let other = eligible(&db, "eligibility-other").await;
    let mut conn = db.acquire().await.unwrap();
    let location = ParkingBuilder::new().create(&mut conn).await.unwrap();
    drop(conn);
    let repo = SqlxParkingContributionRepository::new(db.clone());
    let input = NewProposal {
        location_id: location.id(),
        proposer_id: author,
        base_version: 1,
        kind: ProposalKind::EditDetails,
        proposed: ParkingEdit::from_location(&location).to_json(),
    };
    let id = repo.create_proposal(&input).await.unwrap();
    assert_eq!(
        repo.vote_on_proposal(id, voter, ProposalVote::Approve)
            .await
            .unwrap()
            .approvals,
        1
    );
    sqlx::query("UPDATE users SET email_verified_at=NULL WHERE id=$1")
        .bind(voter.0)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert!(
        repo.vote_on_proposal(id, voter, ProposalVote::Approve)
            .await
            .is_err()
    );
    assert!(
        repo.create_proposal(&NewProposal {
            proposer_id: voter,
            ..input.clone()
        })
        .await
        .is_err()
    );
    assert_eq!(
        repo.vote_on_proposal(id, other, ProposalVote::Approve)
            .await
            .unwrap()
            .approvals,
        1
    );
    sqlx::query("UPDATE users SET account_state='SUSPENDED' WHERE id=$1")
        .bind(other.0)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert!(
        repo.vote_on_proposal(id, other, ProposalVote::Approve)
            .await
            .is_err()
    );
    assert!(
        repo.create_proposal(&NewProposal {
            proposer_id: other,
            ..input
        })
        .await
        .is_err()
    );
}

#[db_test]
async fn sixth_distinct_eligible_vote_publishes_all_fields_and_one_version(tx: &mut TestTx) {
    let db = tx.db().await;
    let proposer = eligible(&db, "author").await;
    let mut conn = db.acquire().await.unwrap();
    let location = ParkingBuilder::new().create(&mut conn).await.unwrap();
    drop(conn);
    let repo = SqlxParkingContributionRepository::new(db.clone());
    let reader = SqlxParkingDetailsReader::new(db.clone());
    let mut edit = ParkingEdit::from_location(&location);
    edit.name = "Approved name".into();
    edit.address = "Approved address".into();
    edit.description = Some("Approved description".into());
    edit.parking_type = bikesnest_domain::ParkingType::Indoor;
    edit.cost = Cost::Paid {
        price: Some(Money::new(
            1234,
            CurrencyCode::parse("BRL").unwrap(),
            PricingUnit::Hour,
        )),
    };
    edit.hours = OpeningHours::weekly(vec![(1, TimeRange::all_day())]);
    edit.security = vec![SecurityFeature::new("cctv", SecurityState::Yes)];
    let input = NewProposal {
        location_id: location.id(),
        proposer_id: proposer,
        base_version: 1,
        kind: ProposalKind::EditDetails,
        proposed: edit.to_json(),
    };
    let id = repo.create_proposal(&input).await.unwrap();
    let sibling = repo.create_proposal(&input).await.unwrap();
    let summary = repo.pending_proposal_summary(location.id()).await.unwrap();
    assert_eq!(summary.total, 2);
    assert!(
        summary.fields.iter().any(|cue| cue.field == "name"),
        "summary={summary:?}"
    );
    assert!(
        summary.fields.len() <= bikesnest_domain::SECURITY_FEATURE_CODES.len() + 9,
        "summary output is bounded by public fields, not proposal count"
    );
    let (first, total, has_more) = repo
        .listing_proposals_page(location.id(), None, 1)
        .await
        .unwrap();
    assert_eq!((first.len(), total, has_more), (1, 2, true));
    let (second, total, has_more) = repo
        .listing_proposals_page(location.id(), Some(first[0].id), 1)
        .await
        .unwrap();
    assert_eq!((second.len(), total, has_more), (1, 2, false));
    assert!(second[0].id < first[0].id);
    assert!(matches!(
        repo.vote_on_proposal(id, proposer, ProposalVote::Approve)
            .await,
        Err(ContributionError::Unauthorized)
    ));
    let mut voters = Vec::new();
    for n in 0..6 {
        voters.push(eligible(&db, &format!("voter-{n}")).await);
    }
    for voter in &voters[..5] {
        repo.vote_on_proposal(id, *voter, ProposalVote::Approve)
            .await
            .unwrap();
    }
    let totals = repo
        .vote_on_proposal(id, voters[0], ProposalVote::Approve)
        .await
        .unwrap();
    assert_eq!(totals.approvals, 5, "repeat vote cannot reach threshold");
    let current = reader.details(location.id()).await.unwrap().unwrap();
    assert_eq!(current.name(), location.name());
    assert_eq!(current.version(), 1);
    let totals = repo
        .vote_on_proposal(id, voters[0], ProposalVote::Reject)
        .await
        .unwrap();
    assert_eq!((totals.approvals, totals.rejections), (4, 1));
    repo.vote_on_proposal(id, voters[0], ProposalVote::Approve)
        .await
        .unwrap();
    repo.vote_on_proposal(id, voters[5], ProposalVote::Approve)
        .await
        .unwrap();
    let current = reader.details(location.id()).await.unwrap().unwrap();
    assert_eq!(
        ParkingEdit::from_location(&current).to_json(),
        edit.to_json()
    );
    assert_eq!(current.version(), 2);
    let proposals = repo.listing_proposals(location.id(), 50).await.unwrap();
    assert_eq!(
        proposals.iter().find(|p| p.id == id).unwrap().status,
        ProposalStatus::Approved
    );
    assert_eq!(
        proposals.iter().find(|p| p.id == sibling).unwrap().status,
        ProposalStatus::Superseded
    );
    let history = repo.revision_history(location.id(), 50).await.unwrap();
    assert_eq!(history.iter().filter(|r| r.version == 2).count(), 1);
    assert_eq!(
        ParkingEdit::from_json(&history[0].snapshot)
            .unwrap()
            .to_json(),
        edit.to_json()
    );
    let mut follow_up = edit.clone();
    follow_up.description = Some("A later revision".into());
    repo.apply_edit(location.id(), 2, &follow_up, proposer, chrono::Utc::now())
        .await
        .unwrap();
    let (newest, total, has_more) = repo
        .revision_history_page(location.id(), None, 1)
        .await
        .unwrap();
    assert_eq!((newest.len(), total, has_more), (1, 2, true));
    let (older, total, has_more) = repo
        .revision_history_page(location.id(), Some(newest[0].version), 1)
        .await
        .unwrap();
    assert_eq!((older.len(), total, has_more), (1, 2, false));
    assert!(older[0].version < newest[0].version);
    assert!(
        repo.vote_on_proposal(id, voters[5], ProposalVote::Approve)
            .await
            .is_err()
    );
    assert!(matches!(
        repo.create_proposal(&input).await,
        Err(ContributionError::VersionConflict)
    ));
}

#[db_test]
async fn moderator_rejection_stale_and_self_approval_do_not_publish(tx: &mut TestTx) {
    let db = tx.db().await;
    let proposer = eligible(&db, "reject-author").await;
    let moderator = eligible(&db, "moderator").await;
    let mut conn = db.acquire().await.unwrap();
    let location = ParkingBuilder::new().create(&mut conn).await.unwrap();
    drop(conn);
    let repo = SqlxParkingContributionRepository::new(db.clone());
    let moderation = SqlxModerationRepository::new(db.clone());
    let mut edit = ParkingEdit::from_location(&location);
    edit.name = "Not published".into();
    let input = NewProposal {
        location_id: location.id(),
        proposer_id: proposer,
        base_version: 1,
        kind: ProposalKind::EditDetails,
        proposed: edit.to_json(),
    };
    let rejected = repo.create_proposal(&input).await.unwrap();
    assert!(
        moderation
            .approve_proposal(
                rejected,
                proposer,
                ProposalApplication::EditDetails(edit.clone())
            )
            .await
            .is_err()
    );
    moderation
        .reject_proposal(rejected, moderator, "Incorrect information")
        .await
        .unwrap();
    assert!(
        moderation
            .approve_proposal(
                rejected,
                moderator,
                ProposalApplication::EditDetails(edit.clone())
            )
            .await
            .is_err()
    );
    let stale = repo.create_proposal(&input).await.unwrap();
    let mut conn = db.acquire().await.unwrap();
    sqlx::query("UPDATE parking_location SET version=version+1 WHERE id=$1")
        .bind(location.id())
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    assert!(matches!(
        moderation
            .approve_proposal(stale, moderator, ProposalApplication::EditDetails(edit))
            .await,
        Err(ModerationError::StaleProposal)
    ));
    assert!(matches!(
        repo.vote_on_proposal(stale, moderator, ProposalVote::Approve)
            .await,
        Err(ContributionError::VersionConflict)
    ));
    let current = SqlxParkingDetailsReader::new(db)
        .details(location.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.name(), location.name());
}

#[db_test]
async fn moderator_can_approve_details_moves_and_removal(tx: &mut TestTx) {
    let db = tx.db().await;
    let proposer = eligible(&db, "kinds-author").await;
    let moderator = eligible(&db, "kinds-moderator").await;
    let mut conn = db.acquire().await.unwrap();
    let location = ParkingBuilder::new().create(&mut conn).await.unwrap();
    drop(conn);
    let repo = SqlxParkingContributionRepository::new(db.clone());
    let moderation = SqlxModerationRepository::new(db.clone());
    let mut edit = ParkingEdit::from_location(&location);
    edit.name = "Moderator approved".into();
    for (index, change) in [
        ProposedChange::EditDetails(edit),
        ProposedChange::MoveLocation {
            lat: -25.44,
            lon: -49.28,
            timezone: Some("America/Manaus".into()),
        },
        ProposedChange::ChangeExistence { exists: false },
    ]
    .into_iter()
    .enumerate()
    {
        let kind = change.kind().unwrap();
        let id = repo
            .create_proposal(&NewProposal {
                location_id: location.id(),
                proposer_id: proposer,
                base_version: index as i64 + 1,
                kind,
                proposed: change.to_json(),
            })
            .await
            .unwrap();
        let proposal = moderation.get_proposal(id).await.unwrap().unwrap();
        assert_eq!(proposal.change.to_json(), change.to_json());
        let applied =
            ProposalApplication::merge(kind, &proposal.change, &Default::default()).unwrap();
        moderation
            .approve_proposal(id, moderator, applied)
            .await
            .unwrap();
        assert_eq!(
            repo.revision_history(location.id(), 50).await.unwrap()[0].version,
            index as i64 + 2
        );
    }
    let current = SqlxParkingDetailsReader::new(db)
        .details(location.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.name(), "Moderator approved");
    assert_eq!(current.point().lat(), -25.44);
    assert_eq!(
        current.moderation_state(),
        bikesnest_domain::ModerationState::Removed
    );
}

#[test]
fn sixth_vote_and_moderator_approval_publish_exactly_once() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let (location_id, proposal_id, moderator, edit) =
            proposal_with_five_votes(&db, "vote-moderator").await;
        let sixth = eligible(&db, "vote-moderator-sixth").await;
        let sibling = SqlxParkingContributionRepository::new(db.clone())
            .create_proposal(&NewProposal {
                location_id,
                proposer_id: eligible(&db, "vote-moderator-sibling-author").await,
                base_version: 1,
                kind: ProposalKind::EditDetails,
                proposed: edit.to_json(),
            })
            .await
            .unwrap();

        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE parking_location IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let vote_repo = SqlxParkingContributionRepository::new(db.clone());
        let moderation_repo = SqlxModerationRepository::new(db.clone());
        let mut decisions = Box::pin(async move {
            tokio::join!(
                vote_repo.vote_on_proposal(proposal_id, sixth, ProposalVote::Approve),
                moderation_repo.approve_proposal(
                    proposal_id,
                    moderator,
                    ProposalApplication::EditDetails(edit),
                ),
            )
        });
        let waited = tokio::select! {
            _ = wait_for_lockers(&pool, 2) => true,
            _ = &mut decisions => false,
        };
        blocker.rollback().await.unwrap();
        assert!(
            waited,
            "both approval paths must overlap at the location lock"
        );
        let (vote, moderation) =
            tokio::time::timeout(std::time::Duration::from_secs(10), decisions)
                .await
                .expect("both approval paths finish after lock release");
        assert!(
            matches!(
                (&vote, &moderation),
                (Ok(_), Err(ModerationError::InvalidState))
            ) || matches!(
                (&vote, &moderation),
                (Err(ContributionError::Conflict), Ok(()))
            ),
            "only the serialized winner may publish: vote={vote:?}, moderation={moderation:?}"
        );

        let (version, name): (i64, String) =
            sqlx::query_as("SELECT version,name FROM parking_location WHERE id=$1")
                .bind(location_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((version, name.as_str()), (2, "Published vote-moderator"));
        let (status, sibling_status, revisions): (String, String, i64) = sqlx::query_as(
            "SELECT p.status,s.status,
                    (SELECT count(*) FROM parking_revision WHERE location_id=$1 AND version=2)
             FROM parking_proposal p, parking_proposal s WHERE p.id=$2 AND s.id=$3",
        )
        .bind(location_id)
        .bind(proposal_id)
        .bind(sibling)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(status, "APPROVED");
        assert_eq!(sibling_status, "SUPERSEDED");
        assert_eq!(revisions, 1);
    });
}

#[test]
fn competing_sixth_votes_publish_only_one_proposal_for_a_location() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let proposer = eligible(&db, "rival-author").await;
        let mut conn = db.acquire().await.unwrap();
        let location = ParkingBuilder::new()
            .with_name("Rival proposal race")
            .create(&mut conn)
            .await
            .unwrap();
        drop(conn);
        let mut edit = ParkingEdit::from_location(&location);
        edit.name = "One published rival".into();
        let input = NewProposal {
            location_id: location.id(),
            proposer_id: proposer,
            base_version: 1,
            kind: ProposalKind::EditDetails,
            proposed: edit.to_json(),
        };
        let repo = SqlxParkingContributionRepository::new(db.clone());
        let first = repo.create_proposal(&input).await.unwrap();
        let second = repo.create_proposal(&input).await.unwrap();
        for index in 0..5 {
            let voter = eligible(&db, &format!("rival-common-{index}")).await;
            repo.vote_on_proposal(first, voter, ProposalVote::Approve)
                .await
                .unwrap();
            repo.vote_on_proposal(second, voter, ProposalVote::Approve)
                .await
                .unwrap();
        }
        let first_sixth = eligible(&db, "rival-first-sixth").await;
        let second_sixth = eligible(&db, "rival-second-sixth").await;

        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("LOCK TABLE parking_location IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *blocker)
            .await
            .unwrap();
        let first_repo = SqlxParkingContributionRepository::new(db.clone());
        let second_repo = SqlxParkingContributionRepository::new(db);
        let mut votes = Box::pin(async move {
            tokio::join!(
                first_repo.vote_on_proposal(first, first_sixth, ProposalVote::Approve),
                second_repo.vote_on_proposal(second, second_sixth, ProposalVote::Approve),
            )
        });
        let waited = tokio::select! {
            _ = wait_for_lockers(&pool, 2) => true,
            _ = &mut votes => false,
        };
        blocker.rollback().await.unwrap();
        assert!(waited, "both rival votes must wait on the location lock");
        let (first_result, second_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), votes)
                .await
                .expect("both rival votes finish after lock release");
        assert!(
            matches!(
                (&first_result, &second_result),
                (Ok(_), Err(ContributionError::VersionConflict))
                    | (Err(ContributionError::VersionConflict), Ok(_))
            ),
            "one rival must publish and the other observe the new version: {first_result:?}, {second_result:?}"
        );
        let (version, revisions): (i64, i64) = sqlx::query_as(
            "SELECT version,
                    (SELECT count(*) FROM parking_revision WHERE location_id=$1 AND version=2)
             FROM parking_location WHERE id=$1",
        )
        .bind(location.id())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!((version, revisions), (2, 1));
        let states: Vec<(i64, String, i64)> = sqlx::query_as(
            "SELECT p.id,p.status,count(v.voter_id)
             FROM parking_proposal p LEFT JOIN parking_proposal_vote v ON v.proposal_id=p.id
             WHERE p.id=ANY($1) GROUP BY p.id,p.status ORDER BY p.id",
        )
        .bind(vec![first, second])
        .fetch_all(&pool)
        .await
        .unwrap();
        assert!(
            matches!(
                states.as_slice(),
                [(_, approved, 6), (_, superseded, 5)]
                    if approved == "APPROVED" && superseded == "SUPERSEDED"
            ) || matches!(
                states.as_slice(),
                [(_, superseded, 5), (_, approved, 6)]
                    if approved == "APPROVED" && superseded == "SUPERSEDED"
            )
        );
    });
}

#[test]
fn sixth_vote_and_voter_suspension_have_a_consistent_eligibility_boundary() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let (blocked_location, blocked_proposal, moderator, _edit) =
            proposal_with_five_votes(&db, "suspend-first").await;
        let blocked_sixth = eligible(&db, "suspend-first-sixth").await;
        let now = chrono::Utc::now();
        let session = SessionId::new([201; 32]);
        SqlxSessionStore::new(db.clone())
            .create(blocked_sixth, &session, &CsrfToken::new([202; 32]), now)
            .await
            .unwrap();
        let reset = VerificationToken::new([203; 32]);
        assert!(
            SqlxTokenStore::new(db.clone())
                .issue_reset(blocked_sixth, &reset, now)
                .await
                .unwrap()
        );

        // Force the vote to reach its first authoritative location lock, then
        // commit suspension before allowing its eligibility read. It must not
        // retain the earlier request's eligibility or publish a sixth vote.
        let mut location_blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM parking_location WHERE id=$1 FOR UPDATE")
            .bind(blocked_location)
            .fetch_one(&mut *location_blocker)
            .await
            .unwrap();
        let vote_repo = SqlxParkingContributionRepository::new(db.clone());
        let mut blocked_vote = Box::pin(vote_repo.vote_on_proposal(
            blocked_proposal,
            blocked_sixth,
            ProposalVote::Approve,
        ));
        let vote_waited = tokio::select! {
            _ = wait_for_lockers(&pool, 1) => true,
            _ = &mut blocked_vote => false,
        };
        if !vote_waited {
            location_blocker.rollback().await.unwrap();
            panic!("the vote must wait at the authoritative location lock");
        }
        assert!(
            SqlxAccountRepository::new(db.clone())
                .suspend_by_admin(blocked_sixth, moderator)
                .await
                .unwrap()
        );
        location_blocker.rollback().await.unwrap();
        assert!(matches!(
            tokio::time::timeout(std::time::Duration::from_secs(10), blocked_vote)
                .await
                .expect("blocked vote finishes after location release"),
            Err(ContributionError::NotVerified)
        ));
        let blocked_state: (String, String, i64, i64, i64, bool, bool, i64) = sqlx::query_as(
            "SELECT u.account_state,p.status,l.version,
                    (SELECT count(*) FROM parking_revision WHERE location_id=$1 AND version=2),
                    (SELECT count(*) FROM parking_proposal_vote WHERE proposal_id=$2),
                    (SELECT revoked_at IS NOT NULL FROM sessions WHERE user_id=$3),
                    (SELECT used_at IS NOT NULL FROM password_reset_tokens WHERE user_id=$3),
                    (SELECT count(*) FROM audit_events WHERE target_id=$4
                     AND action='user.suspended' AND result='success')
             FROM users u,parking_proposal p,parking_location l
             WHERE u.id=$3 AND p.id=$2 AND l.id=$1",
        )
        .bind(blocked_location)
        .bind(blocked_proposal)
        .bind(blocked_sixth.0)
        .bind(blocked_sixth.0.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            blocked_state,
            ("SUSPENDED".into(), "PENDING".into(), 1, 0, 5, true, true, 1)
        );

        // Complementary linearization: a vote that commits first publishes
        // legitimately; a later suspension must not erase public history.
        let (published_location, published_proposal, second_moderator, _edit) =
            proposal_with_five_votes(&db, "vote-first").await;
        let published_sixth = eligible(&db, "vote-first-sixth").await;
        SqlxParkingContributionRepository::new(db.clone())
            .vote_on_proposal(published_proposal, published_sixth, ProposalVote::Approve)
            .await
            .unwrap();
        assert!(
            SqlxAccountRepository::new(db.clone())
                .suspend_by_admin(published_sixth, second_moderator)
                .await
                .unwrap()
        );
        let published_state: (String, String, i64, i64, i64) = sqlx::query_as(
            "SELECT u.account_state,p.status,l.version,
                    (SELECT count(*) FROM parking_revision WHERE location_id=$2 AND version=2),
                    (SELECT count(*) FROM audit_events WHERE target_id=$4
                     AND action='user.suspended' AND result='success')
             FROM users u,parking_proposal p,parking_location l
             WHERE u.id=$1 AND p.id=$3 AND l.id=$2",
        )
        .bind(published_sixth.0)
        .bind(published_location)
        .bind(published_proposal)
        .bind(published_sixth.0.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            published_state,
            ("SUSPENDED".into(), "APPROVED".into(), 2, 1, 1)
        );
    });
}

#[db_test]
async fn community_approved_proposal_that_cannot_merge_is_escalated_to_moderators(tx: &mut TestTx) {
    let db = tx.db().await;
    let proposer = eligible(&db, "escalate-author").await;
    let mut conn = db.acquire().await.unwrap();
    let location = ParkingBuilder::new()
        .with_name("Escalate")
        .create(&mut conn)
        .await
        .unwrap();
    // A stored payload that no longer reads as a change: submission validates
    // payloads, so only a row written by another version can look like this.
    let proposal: i64 = sqlx::query_scalar(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed)
         VALUES ($1, $2, 1, 'edit_details', '{\"unexpected\":true}') RETURNING id",
    )
    .bind(location.id())
    .bind(proposer.0)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    drop(conn);

    let repo = SqlxParkingContributionRepository::new(db.clone());
    let threshold = bikesnest_domain::COMMUNITY_APPROVALS_TO_PUBLISH;
    let mut last = None;
    for index in 0..threshold {
        let voter = eligible(&db, &format!("escalate-voter-{index}")).await;
        last = Some(
            repo.vote_on_proposal(proposal, voter, ProposalVote::Approve)
                .await
                .expect("a vote on an unmergeable proposal still records"),
        );
    }
    let last = last.unwrap();
    assert_eq!(last.approvals, threshold);
    assert!(
        !last.published,
        "nothing can be published from this payload"
    );

    let escalated = async || -> (String, Option<chrono::DateTime<chrono::Utc>>, i64) {
        sqlx::query_as(
            "SELECT p.status, p.escalated_at, l.version
             FROM parking_proposal p JOIN parking_location l ON l.id = p.location_id
             WHERE p.id = $1",
        )
        .bind(proposal)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap()
    };
    let (status, first_escalation, version) = escalated().await;
    assert_eq!(status, "PENDING", "the proposal waits for a moderator");
    assert!(
        first_escalation.is_some(),
        "the proposal is flagged for review"
    );
    assert_eq!(version, 1, "the listing is unchanged");
    assert!(
        SqlxModerationRepository::new(db.clone())
            .list_pending_proposals(None, 200)
            .await
            .unwrap()
            .iter()
            .any(|p| p.id == proposal),
        "the escalated proposal is in the moderation queue"
    );

    // Later votes do not retry the merge or move the escalation time.
    let late = eligible(&db, "escalate-voter-late").await;
    let totals = repo
        .vote_on_proposal(proposal, late, ProposalVote::Approve)
        .await
        .unwrap();
    assert_eq!(totals.approvals, threshold + 1);
    assert!(!totals.published);
    assert_eq!(escalated().await.1, first_escalation);
}

#[db_test]
async fn every_vote_tally_counts_the_same_eligible_voters(tx: &mut TestTx) {
    let db = tx.db().await;
    let proposer = eligible(&db, "tally-author").await;
    let moderator = eligible(&db, "tally-moderator").await;
    let mut conn = db.acquire().await.unwrap();
    let location = ParkingBuilder::new()
        .with_name("Tally")
        .create(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let repo = SqlxParkingContributionRepository::new(db.clone());
    let proposal = repo
        .create_proposal(&NewProposal {
            location_id: location.id(),
            proposer_id: proposer,
            base_version: 1,
            kind: ProposalKind::EditDetails,
            proposed: ParkingEdit::from_location(&location).to_json(),
        })
        .await
        .unwrap();
    let mut voters = Vec::new();
    for (index, vote) in [
        ProposalVote::Approve,
        ProposalVote::Approve,
        ProposalVote::Approve,
        ProposalVote::Reject,
        ProposalVote::Reject,
    ]
    .into_iter()
    .enumerate()
    {
        let voter = eligible(&db, &format!("tally-voter-{index}")).await;
        repo.vote_on_proposal(proposal, voter, vote).await.unwrap();
        voters.push(voter);
    }
    // One approver is suspended and one rejecter loses verification: neither
    // counts any more, in any tally.
    let mut conn = db.acquire().await.unwrap();
    sqlx::query("UPDATE users SET account_state='SUSPENDED' WHERE id=$1")
        .bind(voters[0].0)
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("UPDATE users SET email_verified_at=NULL WHERE id=$1")
        .bind(voters[3].0)
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);

    let voting = repo
        .vote_on_proposal(
            proposal,
            eligible(&db, "tally-voter-last").await,
            ProposalVote::Reject,
        )
        .await
        .unwrap();
    assert_eq!((voting.approvals, voting.rejections), (2, 2));

    let listed = repo.listing_proposals(location.id(), 10).await.unwrap();
    let listed = listed.iter().find(|p| p.id == proposal).unwrap();
    assert_eq!((listed.approvals, listed.rejections), (2, 2));
    let (page, _, _) = repo
        .listing_proposals_page(location.id(), None, 10)
        .await
        .unwrap();
    let paged = page.iter().find(|p| p.id == proposal).unwrap();
    assert_eq!((paged.approvals, paged.rejections), (2, 2));

    SqlxModerationRepository::new(db.clone())
        .reject_proposal(proposal, moderator, "not needed")
        .await
        .unwrap();
    let decided: (Option<i64>, Option<i64>) = sqlx::query_as(
        "SELECT decision_approvals, decision_rejections FROM parking_proposal WHERE id=$1",
    )
    .bind(proposal)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(decided, (Some(2), Some(2)));
}
