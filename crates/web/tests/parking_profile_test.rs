use askama::Template;
use bikesnest_application::{ParkingContributionRepository, ParkingDetailsView};
use bikesnest_domain::{FreshnessCategory, OpenStatus};
use bikesnest_infrastructure::SqlxParkingContributionRepository;
use bikesnest_test_support::{ParkingBuilder, UserBuilder, db_test, test_config};
use bikesnest_web::{
    DetailsPage,
    auth::Auth,
    i18n::{Locale, Translator},
};

#[db_test]
async fn parking_profile_shows_real_pending_diffs_in_both_languages(tx: &mut TestTx) {
    let db = tx.db().await;
    let mut conn = db.acquire().await.unwrap();
    let user = UserBuilder::new().create(&mut *conn).await.unwrap();
    let location = ParkingBuilder::new()
        .at(-25.43, -49.27)
        .create(&mut conn)
        .await
        .unwrap();
    let mut edit = bikesnest_domain::ParkingEdit::from_location(&location);
    edit.name = "Proposed replacement name".into();
    edit.security = vec![bikesnest_domain::SecurityFeature::new(
        "cctv",
        bikesnest_domain::SecurityState::Yes,
    )];
    for (kind, payload, status) in [
        ("edit_details", edit.to_json(), "PENDING"),
        (
            "move_location",
            serde_json::json!({"lat": -25.44, "lon": -49.28, "timezone": "America/Manaus", "reason": "<script>proposal</script>"}),
            "PENDING",
        ),
        (
            "change_existence",
            serde_json::json!({"existence": "removed"}),
            "PENDING",
        ),
        ("move_location", serde_json::json!({}), "PENDING"),
        (
            "change_existence",
            serde_json::json!({"existence": "exists", "reason": "closed-proposal-reason"}),
            "REJECTED",
        ),
    ] {
        sqlx::query("INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) VALUES ($1, $2, 1, $3, $4, $5)")
            .bind(location.id()).bind(user.id.0).bind(kind).bind(payload).bind(status)
            .execute(&mut *conn).await.unwrap();
    }
    drop(conn);
    let proposals = SqlxParkingContributionRepository::new(db.clone())
        .listing_proposals(location.id(), 50)
        .await
        .unwrap();
    assert_eq!(proposals.len(), 5);
    for (index, locale) in [Locale::En, Locale::PtBr].into_iter().enumerate() {
        let mut page = DetailsPage::build(
            &test_config().map,
            Translator::new(locale),
            ParkingDetailsView {
                location: location.clone(),
                freshness: FreshnessCategory::Never,
                is_open_now: OpenStatus::Unknown,
            },
            Vec::new(),
            &Auth::default(),
        )
        .collaboration_proposals(proposals.clone());
        page.pending_photos = 2;
        assert_eq!(page.pending_count(), 6);
        let current = page.render().unwrap();
        assert!(!current.contains("data-od-id=\"proposal-card\""));
        assert!(current.contains("data-pending-field=\"point\""));
        assert!(current.contains("data-pending-field=\"photos\""));
        assert!(current.contains("data-pending-field=\"cctv\""));
        assert!(current.contains("data-pending-field=\"name\""));
        assert!(!current.contains("Proposed replacement name"));
        assert!(
            !current.contains("-25.440000, -49.280000"),
            "proposed values are not published"
        );
        page.tab = "approvals".into();
        let body = page.render().unwrap();
        assert!(body.contains("-25.430000, -49.270000"));
        assert!(body.contains("-25.440000, -49.280000"));
        assert!(body.contains("America/Manaus"));
        assert_eq!(body.matches("data-od-id=\"proposal-card\"").count(), 4);
        assert!(body.contains("Proposed replacement name"));
        assert!(!body.contains("closed-proposal-reason"));
        assert!(!body.contains("<script>proposal</script>"));
        assert!(
            !body.contains("{i18n?}"),
            "all visible labels are translated"
        );
        assert!(
            !body.contains("name=\"vote\""),
            "anonymous viewers cannot vote"
        );
        if index == 0 {
            assert!(body.contains("Published now"));
            assert!(body.contains("Needs manual review"));
            assert!(body.contains("Pending approvals"));
        } else {
            assert!(body.contains("Publicado agora"));
            assert!(body.contains("Precisa de revisão manual"));
            assert!(body.contains("Aprovações pendentes"));
        }
        let mut snapshot = bikesnest_domain::ParkingEdit::from_location(&location).to_json();
        snapshot["name"] = "Saved historic name".into();
        snapshot["private_media_key"] = "must-not-be-rendered".into();
        page = page.collaboration_history(vec![bikesnest_domain::RevisionSummary {
            version: 1,
            change_kind: bikesnest_domain::ChangeKind::Create,
            summary: None,
            at: chrono::Utc::now(),
            snapshot,
        }]);
        page.tab = "history".into();
        let history = page.render().unwrap();
        assert!(history.contains("Saved historic name"));
        assert!(!history.contains("Proposed replacement name"));
        assert!(!history.contains("must-not-be-rendered"));
    }
}
