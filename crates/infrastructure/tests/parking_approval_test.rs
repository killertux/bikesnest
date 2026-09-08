//! Approval transactions share the harness connection and always roll back.
use bikesnest_application::{
    ContributionError, ModerationError, ModerationRepository, NewProposal,
    ParkingContributionRepository, ParkingDetailsReader, ProposalApplication, ProposalVote,
};
use bikesnest_domain::{
    Cost, CurrencyCode, Money, OpeningHours, ParkingEdit, PricingUnit, ProposalKind,
    ProposalStatus, ProposedChange, SecurityFeature, SecurityState, TimeRange, UserId,
};
use bikesnest_infrastructure::{
    Db, SqlxModerationRepository, SqlxParkingContributionRepository, SqlxParkingDetailsReader,
};
use bikesnest_test_support::{ParkingBuilder, UserBuilder, db_test};

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
