use bikesnest_application::{
    AnonymizationRepository, AuthOutbox, EmailKind, EmailMessage, ExportRepository, NewAccount,
    PolicyReader, PrivacyError, TermsAcceptance, TermsAcknowledgementStore, TermsProof,
};
use bikesnest_domain::{
    AccountState, LocaleCode, PolicyKind, UserEmail, UserId, VerificationToken,
};
use bikesnest_infrastructure::{
    Db, POLICY_LOCALES, SeedPolicyDocument, SqlxAnonymizationRepository, SqlxAuthOutbox,
    SqlxExportRepository, SqlxPolicyReader, seed_policy_release,
};
use bikesnest_test_support::run_isolated_database_test;
use chrono::{DateTime, Utc};

const REMOVE_0028: &str = r#"
    DROP TABLE terms_acknowledgement, terms_notice_presentation;
    DROP TRIGGER policy_version_no_delete ON policy_version;
    DROP TRIGGER policy_version_preserve_identity ON policy_version;
    DROP FUNCTION reject_policy_version_delete();
    DROP FUNCTION preserve_policy_version_identity();
    DROP FUNCTION reject_terms_proof_update();
    ALTER TABLE policy_version
      DROP CONSTRAINT policy_version_kind_locale_effective_key,
      DROP CONSTRAINT policy_version_ack_terms_only,
      DROP COLUMN requires_acknowledgement;
"#;

fn at(value: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(value)
        .expect("fixed test time")
        .with_timezone(&Utc)
}

fn release<'a>(
    version: &'a str,
    effective_at: DateTime<Utc>,
    suffix: &'a str,
    material: bool,
) -> Vec<SeedPolicyDocument<'a>> {
    let mut docs = Vec::new();
    for kind in [PolicyKind::Privacy, PolicyKind::Terms, PolicyKind::Cookies] {
        for locale in POLICY_LOCALES {
            docs.push(SeedPolicyDocument {
                kind,
                locale,
                version,
                effective_at,
                content: suffix,
                requires_acknowledgement: material && kind == PolicyKind::Terms,
            });
        }
    }
    docs
}

fn verification_message(email: &UserEmail, token: &VerificationToken) -> EmailMessage {
    EmailMessage::linked(
        UserId(0),
        email.as_str(),
        LocaleCode::En,
        EmailKind::VerifyEmail {
            link: format!(
                "https://bikesnest.test/verify-email?token={}",
                token.to_base64url()
            ),
            expires_at: None,
        },
    )
}

async fn registration_snapshot(pool: &sqlx::PgPool, email: &str) -> serde_json::Value {
    sqlx::query_scalar(
        r#"SELECT jsonb_build_object(
             'account', (SELECT to_jsonb(u) FROM users u WHERE u.email=$1),
             'identities', (SELECT coalesce(jsonb_agg(to_jsonb(i) ORDER BY i.id),'[]')
                            FROM authentication_identities i JOIN users u ON u.id=i.user_id WHERE u.email=$1),
             'tokens', (SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY t.token_hash),'[]')
                        FROM email_verification_tokens t JOIN users u ON u.id=t.user_id WHERE u.email=$1),
             'jobs', (SELECT coalesce(jsonb_agg(to_jsonb(j) ORDER BY j.id),'[]')
                      FROM background_job j WHERE j.mail_account_id=(SELECT id FROM users WHERE email=$1)),
             'audits', (SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY a.id),'[]')
                        FROM audit_events a WHERE a.target_type='user'
                          AND a.target_id=(SELECT id::text FROM users WHERE email=$1)),
             'acknowledgements', (SELECT coalesce(jsonb_agg(to_jsonb(a) ORDER BY a.id),'[]')
                                  FROM terms_acknowledgement a JOIN users u ON u.id=a.user_id WHERE u.email=$1)
           )"#,
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[test]
fn coherent_release_is_atomic_immutable_and_exactly_replayable() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        // Exercise the forward-only identity correction explicitly. The
        // isolated database may be cloned from a base whose migration ledger
        // predates this newly added migration during a local review run.
        sqlx::raw_sql(include_str!(
            "../../../migrations/0029_policy_version_id_immutable.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let db = Db::from_pool(pool.clone());
        let first = release("2026.1", at("2026-01-01T00:00:00Z"), "first", false);
        seed_policy_release(&db, &first).await.unwrap();
        seed_policy_release(&db, &first).await.unwrap();

        let published_id: i64 = sqlx::query_scalar(
            "SELECT id FROM policy_version WHERE kind='terms' AND locale='en' AND version='2026.1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let identity_error = sqlx::query(
            "UPDATE policy_version SET id=DEFAULT,superseded_at='2026-06-01' WHERE id=$1",
        )
        .bind(published_id)
        .execute(&pool)
        .await
        .unwrap_err();
        assert!(
            identity_error
                .to_string()
                .contains("policy versions are immutable"),
            "the custom identity guard must reject id replacement during first supersession"
        );
        let preserved_id: i64 = sqlx::query_scalar(
            "SELECT id FROM policy_version WHERE kind='terms' AND locale='en' AND version='2026.1'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(preserved_id, published_id);

        let partial = &first[..5];
        assert!(matches!(
            seed_policy_release(&db, partial).await,
            Err(PrivacyError::InvalidField(_))
        ));
        let mut duplicate = first.clone();
        duplicate[5].locale = "pt-BR";
        assert!(matches!(
            seed_policy_release(&db, &duplicate).await,
            Err(PrivacyError::InvalidField(_))
        ));
        let mut mismatched_material = first.clone();
        mismatched_material
            .iter_mut()
            .find(|doc| doc.kind == PolicyKind::Terms && doc.locale == "en")
            .unwrap()
            .requires_acknowledgement = true;
        assert!(matches!(
            seed_policy_release(&db, &mismatched_material).await,
            Err(PrivacyError::InvalidField(_))
        ));
        let conflicting = release("2026.1", at("2026-01-01T00:00:00Z"), "changed", false);
        assert!(matches!(
            seed_policy_release(&db, &conflicting).await,
            Err(PrivacyError::Conflict)
        ));

        sqlx::query(
            r#"CREATE FUNCTION fail_policy_release() RETURNS trigger LANGUAGE plpgsql AS $$
               BEGIN IF NEW.kind='cookies' AND NEW.locale='en' THEN
                 RAISE EXCEPTION 'injected release failure'; END IF; RETURN NEW; END $$"#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "CREATE TRIGGER fail_policy_release_row BEFORE INSERT ON policy_version \
             FOR EACH ROW EXECUTE FUNCTION fail_policy_release()",
        )
        .execute(&pool)
        .await
        .unwrap();
        let second = release("2026.2", at("2027-01-01T00:00:00Z"), "second", true);
        assert!(seed_policy_release(&db, &second).await.is_err());
        let inserted: i64 =
            sqlx::query_scalar("SELECT count(*) FROM policy_version WHERE version='2026.2'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            inserted, 0,
            "a mid-release failure rolls back every locale and kind"
        );
        let still_open: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM policy_version WHERE version='2026.1' AND superseded_at IS NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(still_open, 6);

        assert!(
            sqlx::query("UPDATE policy_version SET content='mutated' WHERE version='2026.1'")
                .execute(&pool)
                .await
                .is_err()
        );
        assert!(
            sqlx::query("DELETE FROM policy_version WHERE version='2026.1'")
                .execute(&pool)
                .await
                .is_err()
        );
    });
}

#[test]
fn notices_include_current_and_only_nearest_future_and_acknowledge_by_release() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        seed_policy_release(
            &db,
            &release("current", at("2026-01-01T00:00:00Z"), "current", true),
        )
        .await
        .unwrap();
        seed_policy_release(
            &db,
            &release("future", at("2098-01-01T00:00:00Z"), "future", true),
        )
        .await
        .unwrap();
        seed_policy_release(
            &db,
            &release("later", at("2099-01-01T00:00:00Z"), "later", true),
        )
        .await
        .unwrap();
        let user_id: i64 = sqlx::query_scalar(
            "INSERT INTO users(email,account_state) VALUES('terms@example.test','ACTIVE') RETURNING id",
        ).fetch_one(&pool).await.unwrap();
        let store = SqlxPolicyReader::new(db);
        let notices = store.pending_notices(UserId(user_id), "en").await.unwrap();
        assert_eq!(notices.len(), 2);
        assert_eq!(notices[0].document.version, "current");
        assert!(notices[0].may_acknowledge);
        assert_eq!(notices[1].document.version, "future");
        assert!(!notices[1].may_acknowledge);

        let current = &notices[0].document;
        let proof = TermsProof {
            policy_version_id: current.id,
            terms_version: current.version.clone(),
            shown_locale: current.locale.clone(),
        };
        let wrong_version = TermsProof {
            terms_version: "hostile-version@example.test".into(),
            ..proof.clone()
        };
        assert!(matches!(
            store.present(UserId(user_id), &wrong_version).await,
            Err(PrivacyError::Conflict)
        ));
        let privacy = store
            .current(PolicyKind::Privacy, "en")
            .await
            .unwrap()
            .unwrap();
        let wrong_kind = TermsProof {
            policy_version_id: privacy.id,
            terms_version: privacy.version,
            shown_locale: privacy.locale,
        };
        assert!(matches!(
            store.present(UserId(user_id), &wrong_kind).await,
            Err(PrivacyError::Conflict)
        ));
        store.present(UserId(user_id), &proof).await.unwrap();
        store.present(UserId(user_id), &proof).await.unwrap();
        store
            .acknowledge_current(UserId(user_id), &proof)
            .await
            .unwrap();
        let pt = store
            .pending_notices(UserId(user_id), "pt-BR")
            .await
            .unwrap();
        assert_eq!(
            pt.len(),
            1,
            "the bilingual current release is satisfied once"
        );
        assert_eq!(pt[0].document.version, "future");

        let future = TermsProof {
            policy_version_id: notices[1].document.id,
            terms_version: "future".into(),
            shown_locale: "en".into(),
        };
        assert!(matches!(
            store.acknowledge_current(UserId(user_id), &future).await,
            Err(PrivacyError::Conflict)
        ));
        let presentations: i64 =
            sqlx::query_scalar("SELECT count(*) FROM terms_notice_presentation WHERE user_id=$1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(presentations, 1, "retries do not fabricate presentations");

        sqlx::query("UPDATE users SET account_state='SUSPENDED' WHERE id=$1")
            .bind(user_id)
            .execute(&pool)
            .await
            .unwrap();
        assert!(matches!(
            store.present(UserId(user_id), &future).await,
            Err(PrivacyError::NotAuthorized)
        ));
    });
}

#[test]
fn current_reader_excludes_a_scheduled_release() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool);
        seed_policy_release(
            &db,
            &release("current", at("2026-01-01T00:00:00Z"), "current", false),
        )
        .await
        .unwrap();
        seed_policy_release(
            &db,
            &release("scheduled", at("2099-01-01T00:00:00Z"), "scheduled", false),
        )
        .await
        .unwrap();
        let reader = SqlxPolicyReader::new(db);
        assert_eq!(
            reader
                .current(PolicyKind::Terms, "en")
                .await
                .unwrap()
                .unwrap()
                .version,
            "current"
        );
        assert!(
            reader
                .upcoming_material_terms("en")
                .await
                .unwrap()
                .is_none(),
            "a non-material future release does not create an advance notice"
        );
    });
}

#[test]
fn migration_0028_preserves_legacy_documents_and_adds_proof_schema() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        sqlx::raw_sql(REMOVE_0028).execute(&pool).await.unwrap();
        let legacy_id: i64 = sqlx::query_scalar(
            "INSERT INTO policy_version(kind,locale,version,effective_at,content) \
             VALUES('terms','en','legacy','2025-01-01','legacy body') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0028_terms_acknowledgement.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let row: (String, bool) = sqlx::query_as(
            "SELECT content,requires_acknowledgement FROM policy_version WHERE id=$1",
        )
        .bind(legacy_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row, ("legacy body".into(), false));
        let tables: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_schema='public' \
             AND table_name IN ('terms_acknowledgement','terms_notice_presentation')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(tables, 2);
    });
}

#[test]
fn migration_0028_fails_closed_on_ambiguous_legacy_schedule() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        sqlx::raw_sql(REMOVE_0028).execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO policy_version(kind,locale,version,effective_at,content) VALUES \
             ('terms','en','duplicate-a','2028-01-01','a'), \
             ('terms','en','duplicate-b','2028-01-01','b')",
        )
        .execute(&pool)
        .await
        .unwrap();
        let error = sqlx::raw_sql(include_str!(
            "../../../migrations/0028_terms_acknowledgement.sql"
        ))
        .execute(&pool)
        .await
        .expect_err("ambiguous published history must stop the upgrade");
        assert!(
            error
                .to_string()
                .contains("duplicate kind/locale/effective_at")
        );
        let has_column: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM information_schema.columns WHERE table_name='policy_version' AND column_name='requires_acknowledgement')",
        ).fetch_one(&pool).await.unwrap();
        assert!(!has_column, "the preflight runs before schema mutation");
    });
}

#[test]
fn export_describes_proof_semantics_and_anonymization_removes_it_immediately() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        seed_policy_release(
            &db,
            &release("proof", at("2026-01-01T00:00:00Z"), "proof", true),
        )
        .await
        .unwrap();
        let user_id: i64 = sqlx::query_scalar(
            "INSERT INTO users(email,account_state) VALUES('proof@example.test','ACTIVE') RETURNING id",
        ).fetch_one(&pool).await.unwrap();
        let store = SqlxPolicyReader::new(db.clone());
        let current = store
            .current(PolicyKind::Terms, "en")
            .await
            .unwrap()
            .unwrap();
        let proof = TermsProof {
            policy_version_id: current.id,
            terms_version: current.version,
            shown_locale: current.locale,
        };
        store.present(UserId(user_id), &proof).await.unwrap();
        store
            .acknowledge_current(UserId(user_id), &proof)
            .await
            .unwrap();

        let exported = SqlxExportRepository::new(db.clone())
            .assemble_payload(UserId(user_id))
            .await
            .unwrap();
        assert_eq!(exported.schema_version, 2);
        assert_eq!(exported.terms_notice_presentations.len(), 1);
        assert_eq!(exported.terms_acknowledgements.len(), 1);
        assert_eq!(exported.terms_acknowledgements[0].source, "in_product");

        let report = SqlxAnonymizationRepository::new(db)
            .anonymize(UserId(user_id), Utc::now())
            .await
            .unwrap();
        assert_eq!(report.terms_notice_presentations, 1);
        assert_eq!(report.terms_acknowledgements, 1);
        let remaining: i64 = sqlx::query_scalar(
            "SELECT (SELECT count(*) FROM terms_notice_presentation WHERE user_id=$1) + \
                    (SELECT count(*) FROM terms_acknowledgement WHERE user_id=$1)",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            remaining, 0,
            "soft anonymization deletes legal proof in the same transaction"
        );
    });
}

#[test]
fn signup_policy_decision_rechecks_after_release_lock_and_rolls_back_stale_form() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        seed_policy_release(
            &db,
            &release("old", at("2026-01-01T00:00:00Z"), "old", false),
        )
        .await
        .unwrap();
        let old = SqlxPolicyReader::new(db.clone())
            .current(PolicyKind::Terms, "en")
            .await
            .unwrap()
            .unwrap();
        let acceptance = TermsAcceptance {
            policy_version_id: old.id,
            version: old.version,
            shown_locale: old.locale,
        };
        let email = UserEmail::parse("stale-policy@example.test").unwrap();
        let token = VerificationToken::new([0x71; 32]);

        let mut publisher = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(726_159_001)")
            .execute(&mut *publisher)
            .await
            .unwrap();
        let wait_db = db.clone();
        let wait_email = email.clone();
        let registration = tokio::spawn(async move {
            SqlxAuthOutbox::new(wait_db, 3)
                .register(
                    NewAccount {
                        email: &wait_email,
                        display_name: None,
                        password_hash: "hash",
                        state: AccountState::PendingEmailVerification,
                        locale: LocaleCode::En,
                    },
                    &token,
                    Utc::now(),
                    verification_message(&wait_email, &token),
                    Some(&acceptance),
                )
                .await
        });
        let observed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')",
                ).fetch_one(&pool).await.unwrap();
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await;
        if observed.is_err() {
            publisher.rollback().await.unwrap();
            registration.abort();
            let _ = registration.await;
            panic!("registration did not reach the policy release lock");
        }
        let activation: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *publisher)
            .await
            .unwrap();
        sqlx::query("UPDATE policy_version SET superseded_at=$1 WHERE superseded_at IS NULL")
            .bind(activation)
            .execute(&mut *publisher)
            .await
            .unwrap();
        for doc in release("new", activation, "new", false) {
            sqlx::query("INSERT INTO policy_version(kind,locale,version,effective_at,content,requires_acknowledgement) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(doc.kind.as_code()).bind(doc.locale).bind(doc.version).bind(doc.effective_at)
                .bind(doc.content).bind(doc.requires_acknowledgement)
                .execute(&mut *publisher).await.unwrap();
        }
        publisher.commit().await.unwrap();
        assert!(matches!(
            registration.await.unwrap(),
            Err(bikesnest_application::AuthError::Conflict)
        ));
        assert_eq!(
            registration_snapshot(&pool, email.as_str()).await,
            serde_json::json!({
                "account": null,
                "identities": [],
                "tokens": [],
                "jobs": [],
                "audits": [],
                "acknowledgements": []
            })
        );
    });
}

#[test]
fn existing_registration_recovery_never_fabricates_a_terms_acknowledgement() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        seed_policy_release(
            &db,
            &release("current", at("2026-01-01T00:00:00Z"), "current", false),
        )
        .await
        .unwrap();
        let current = SqlxPolicyReader::new(db.clone())
            .current(PolicyKind::Terms, "en")
            .await
            .unwrap()
            .unwrap();
        let acceptance = TermsAcceptance {
            policy_version_id: current.id,
            version: current.version,
            shown_locale: current.locale,
        };
        let email = UserEmail::parse("legacy-retry@example.test").unwrap();
        let token = VerificationToken::new([0x72; 32]);
        let retry = VerificationToken::new([0x73; 32]);
        let outbox = SqlxAuthOutbox::new(db, 3);
        outbox
            .register(
                NewAccount {
                    email: &email,
                    display_name: None,
                    password_hash: "hash",
                    state: AccountState::PendingEmailVerification,
                    locale: LocaleCode::En,
                },
                &token,
                Utc::now(),
                verification_message(&email, &token),
                None,
            )
            .await
            .unwrap();
        outbox
            .register(
                NewAccount {
                    email: &email,
                    display_name: None,
                    password_hash: "different-hash",
                    state: AccountState::PendingEmailVerification,
                    locale: LocaleCode::En,
                },
                &retry,
                Utc::now(),
                verification_message(&email, &retry),
                Some(&acceptance),
            )
            .await
            .unwrap();
        let acknowledgements: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM terms_acknowledgement a JOIN users u ON u.id=a.user_id WHERE u.email=$1",
        ).bind(email.as_str()).fetch_one(&pool).await.unwrap();
        assert_eq!(acknowledgements, 0);
        let stored_hash: String = sqlx::query_scalar(
            "SELECT credential_hash FROM authentication_identities WHERE provider='password' AND provider_subject=$1",
        ).bind(email.as_str()).fetch_one(&pool).await.unwrap();
        assert_eq!(
            stored_hash, "different-hash",
            "re-registering a pending address replaces its unverified credential"
        );
    });
}

#[test]
fn deletion_wins_against_waiting_presentation_and_acknowledgement() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        seed_policy_release(
            &db,
            &release("current", at("2026-01-01T00:00:00Z"), "current", true),
        )
        .await
        .unwrap();
        let current = SqlxPolicyReader::new(db.clone())
            .current(PolicyKind::Terms, "en")
            .await
            .unwrap()
            .unwrap();
        let proof = TermsProof {
            policy_version_id: current.id,
            terms_version: current.version,
            shown_locale: current.locale,
        };

        for (suffix, acknowledge) in [("present", false), ("ack", true)] {
            let email = format!("delete-{suffix}@example.test");
            let user_id: i64 = sqlx::query_scalar(
                "INSERT INTO users(email,account_state) VALUES($1,'ACTIVE') RETURNING id",
            )
            .bind(&email)
            .fetch_one(&pool)
            .await
            .unwrap();
            let mut deletion = pool.begin().await.unwrap();
            sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
                .bind(user_id)
                .fetch_one(&mut *deletion)
                .await
                .unwrap();
            sqlx::query(
                "UPDATE users SET account_state='DELETED',deleted_at=clock_timestamp() WHERE id=$1",
            )
            .bind(user_id)
            .execute(&mut *deletion)
            .await
            .unwrap();
            let wait_store = SqlxPolicyReader::new(db.clone());
            let wait_proof = proof.clone();
            let operation = tokio::spawn(async move {
                if acknowledge {
                    wait_store
                        .acknowledge_current(UserId(user_id), &wait_proof)
                        .await
                } else {
                    wait_store.present(UserId(user_id), &wait_proof).await
                }
            });
            let observed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')",
                    ).fetch_one(&pool).await.unwrap();
                    if waiting { break; }
                    tokio::task::yield_now().await;
                }
            }).await;
            if observed.is_err() {
                deletion.rollback().await.unwrap();
                operation.abort();
                let _ = operation.await;
                panic!("{suffix} did not serialize on the account row");
            }
            deletion.commit().await.unwrap();
            assert!(matches!(
                operation.await.unwrap(),
                Err(PrivacyError::NotAuthorized)
            ));
            let proofs: i64 = sqlx::query_scalar(
                "SELECT (SELECT count(*) FROM terms_notice_presentation WHERE user_id=$1) + \
                        (SELECT count(*) FROM terms_acknowledgement WHERE user_id=$1)",
            )
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(proofs, 0);
        }
    });
}

#[test]
fn stale_terms_after_a_lock_wait_are_indistinguishable_for_every_registration_state() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        seed_policy_release(
            &db,
            &release("current", at("2026-01-01T00:00:00Z"), "current", false),
        )
        .await
        .unwrap();
        let current = SqlxPolicyReader::new(db.clone())
            .current(PolicyKind::Terms, "en")
            .await
            .unwrap()
            .unwrap();
        let stale = TermsAcceptance {
            policy_version_id: current.id,
            version: "stale".into(),
            shown_locale: current.locale,
        };
        for (index, (label, state)) in [
            ("new", None),
            (
                "pending-active",
                Some(AccountState::PendingEmailVerification),
            ),
            (
                "pending-repair",
                Some(AccountState::PendingEmailVerification),
            ),
            ("active", Some(AccountState::Active)),
        ]
        .into_iter()
        .enumerate()
        {
            let email = UserEmail::parse(&format!("terms-parity-{index}@example.test")).unwrap();
            let token = VerificationToken::new([0x80 + index as u8; 32]);
            if let Some(state) = state {
                SqlxAuthOutbox::new(db.clone(), 3)
                    .register(
                        NewAccount {
                            email: &email,
                            display_name: None,
                            password_hash: "original-hash",
                            state: AccountState::PendingEmailVerification,
                            locale: LocaleCode::En,
                        },
                        &token,
                        Utc::now(),
                        verification_message(&email, &token),
                        None,
                    )
                    .await
                    .unwrap();
                if state == AccountState::Active {
                    sqlx::query("UPDATE users SET account_state='ACTIVE' WHERE email=$1")
                        .bind(email.as_str())
                        .execute(&pool)
                        .await
                        .unwrap();
                }
                if label == "pending-repair" {
                    sqlx::query("DELETE FROM background_job WHERE payload->>'to'=$1")
                        .bind(email.as_str())
                        .execute(&pool)
                        .await
                        .unwrap();
                }
            }
            let before = registration_snapshot(&pool, email.as_str()).await;
            let mut policy_holder = pool.begin().await.unwrap();
            sqlx::query("SELECT pg_advisory_xact_lock(726_159_001)")
                .execute(&mut *policy_holder)
                .await
                .unwrap();
            let wait_db = db.clone();
            let wait_email = email.clone();
            let wait_token = token.clone();
            let wait_stale = stale.clone();
            let registration = tokio::spawn(async move {
                SqlxAuthOutbox::new(wait_db, 3)
                    .register(
                        NewAccount {
                            email: &wait_email,
                            display_name: None,
                            password_hash: "must-not-stick",
                            state: AccountState::PendingEmailVerification,
                            locale: LocaleCode::En,
                        },
                        &wait_token,
                        Utc::now(),
                        verification_message(&wait_email, &wait_token),
                        Some(&wait_stale),
                    )
                    .await
            });
            let waited = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    let waiting: bool = sqlx::query_scalar(
                        "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')",
                    )
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                    if waiting {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
            if waited.is_err() {
                policy_holder.rollback().await.unwrap();
                registration.abort();
                let _ = registration.await;
                panic!("{label} registration did not reach the policy lock");
            }
            policy_holder.commit().await.unwrap();
            let result = registration.await.unwrap();
            assert!(matches!(
                result,
                Err(bikesnest_application::AuthError::Conflict)
            ));
            let after = registration_snapshot(&pool, email.as_str()).await;
            assert_eq!(
                after, before,
                "stale terms cannot reveal or mutate account state"
            );
        }
    });
}

#[test]
fn pending_registration_rechecks_terms_after_waiting_for_policy_activation() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        seed_policy_release(
            &db,
            &release("old", at("2026-01-01T00:00:00Z"), "old", false),
        )
        .await
        .unwrap();
        let old = SqlxPolicyReader::new(db.clone())
            .current(PolicyKind::Terms, "en")
            .await
            .unwrap()
            .unwrap();
        let acceptance = TermsAcceptance {
            policy_version_id: old.id,
            version: old.version,
            shown_locale: old.locale,
        };
        let email = UserEmail::parse("pending-lock@example.test").unwrap();
        let token = VerificationToken::new([0x91; 32]);
        SqlxAuthOutbox::new(db.clone(), 3)
            .register(
                NewAccount {
                    email: &email,
                    display_name: None,
                    password_hash: "original-hash",
                    state: AccountState::PendingEmailVerification,
                    locale: LocaleCode::En,
                },
                &token,
                Utc::now(),
                verification_message(&email, &token),
                None,
            )
            .await
            .unwrap();
        let before = registration_snapshot(&pool, email.as_str()).await;

        let mut publisher = pool.begin().await.unwrap();
        sqlx::query("SELECT pg_advisory_xact_lock(726_159_001)")
            .execute(&mut *publisher)
            .await
            .unwrap();
        let retry_db = db.clone();
        let retry_email = email.clone();
        let retry = tokio::spawn(async move {
            SqlxAuthOutbox::new(retry_db, 3)
                .register(
                    NewAccount {
                        email: &retry_email,
                        display_name: None,
                        password_hash: "must-not-stick",
                        state: AccountState::PendingEmailVerification,
                        locale: LocaleCode::En,
                    },
                    &token,
                    Utc::now(),
                    verification_message(&retry_email, &token),
                    Some(&acceptance),
                )
                .await
        });
        let observed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')",
                ).fetch_one(&pool).await.unwrap();
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await;
        if observed.is_err() {
            publisher.rollback().await.unwrap();
            retry.abort();
            let _ = retry.await;
            panic!("pending recovery did not reach the policy lock");
        }
        let activation: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *publisher)
            .await
            .unwrap();
        sqlx::query("UPDATE policy_version SET superseded_at=$1 WHERE superseded_at IS NULL")
            .bind(activation)
            .execute(&mut *publisher)
            .await
            .unwrap();
        for doc in release("new", activation, "new", false) {
            sqlx::query("INSERT INTO policy_version(kind,locale,version,effective_at,content,requires_acknowledgement) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(doc.kind.as_code()).bind(doc.locale).bind(doc.version).bind(doc.effective_at)
                .bind(doc.content).bind(doc.requires_acknowledgement).execute(&mut *publisher).await.unwrap();
        }
        publisher.commit().await.unwrap();
        assert!(matches!(
            retry.await.unwrap(),
            Err(bikesnest_application::AuthError::Conflict)
        ));
        let after = registration_snapshot(&pool, email.as_str()).await;
        assert_eq!(after, before);
    });
}

#[test]
fn acknowledgement_waits_for_account_before_taking_policy_release_lock() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        seed_policy_release(
            &db,
            &release("current", at("2026-01-01T00:00:00Z"), "current", true),
        )
        .await
        .unwrap();
        let current = SqlxPolicyReader::new(db.clone())
            .current(PolicyKind::Terms, "en")
            .await
            .unwrap()
            .unwrap();
        let proof = TermsProof {
            policy_version_id: current.id,
            terms_version: current.version,
            shown_locale: current.locale,
        };
        let user_id: i64 = sqlx::query_scalar(
            "INSERT INTO users(email,account_state) VALUES('lock-order@example.test','ACTIVE') RETURNING id",
        ).fetch_one(&pool).await.unwrap();
        let mut account_holder = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
            .bind(user_id)
            .fetch_one(&mut *account_holder)
            .await
            .unwrap();
        let wait_store = SqlxPolicyReader::new(db);
        let mut ack = tokio::spawn(async move {
            wait_store
                .acknowledge_current(UserId(user_id), &proof)
                .await
        });
        let observed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock')",
                ).fetch_one(&pool).await.unwrap();
                if waiting { break; }
                tokio::task::yield_now().await;
            }
        }).await;
        if observed.is_err() {
            account_holder.rollback().await.unwrap();
            ack.abort();
            let _ = ack.await;
            panic!("acknowledgement did not wait for the account row");
        }

        let mut policy_probe = pool.begin().await.unwrap();
        let lock_available: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock(726_159_001)")
                .fetch_one(&mut *policy_probe)
                .await
                .unwrap();
        assert!(
            lock_available,
            "a waiter on the account must not hold the policy lock"
        );
        policy_probe.rollback().await.unwrap();
        account_holder.commit().await.unwrap();
        let completed = tokio::time::timeout(std::time::Duration::from_secs(2), &mut ack).await;
        if completed.is_err() {
            ack.abort();
            let _ = ack.await;
            panic!("acknowledgement must finish without lock inversion");
        }
        completed.unwrap().unwrap().unwrap();
    });
}
