//! Database-backed auth integration tests against real PostgreSQL.
//!
//! Older tests below use committed, uniquely marked pool fixtures and clean
//! them up explicitly. New sequential repository regressions inject `tx.db()`
//! and rely on the harness's automatic outer rollback.

use bikesnest_application::{AccountRepository, AuditEvent, AuditLog, SessionStore, TokenStore};
use bikesnest_domain::{
    AccountState, AuthenticationProvider, CsrfToken, Role, SessionId, UserEmail, VerificationToken,
};
use bikesnest_infrastructure::{
    Db, SqlxAccountRepository, SqlxAuditLog, SqlxSessionStore, SqlxTokenStore,
};
use bikesnest_test_support::{db_test, pool, run_isolated_database_test};
use chrono::{Duration, Utc};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn unique_email(label: &str) -> String {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{}-{n}@bikesnest.test", std::process::id())
}

fn marker_email(label: &str) -> String {
    unique_email(label)
}

async fn assert_exact_password_reset_audit(db: &Db, user_id: bikesnest_domain::UserId, count: i64) {
    let mut conn = db.acquire().await.unwrap();
    let rows: Vec<(
        Option<i64>,
        String,
        String,
        String,
        String,
        serde_json::Value,
    )> = sqlx::query_as(
        "SELECT actor_user_id, action, target_type, target_id, result, metadata
             FROM audit_events WHERE actor_user_id = $1 AND action = 'auth.password_changed'",
    )
    .bind(user_id.0)
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    assert_eq!(rows.len() as i64, count);
    for row in rows {
        assert_eq!(
            row,
            (
                Some(user_id.0),
                "auth.password_changed".to_string(),
                "user".to_string(),
                user_id.0.to_string(),
                "success".to_string(),
                serde_json::json!({}),
            )
        );
    }
}

async fn assert_unused_reset_count(db: &Db, user_id: bikesnest_domain::UserId, expected: i64) {
    let mut conn = db.acquire().await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM password_reset_tokens WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(user_id.0)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    assert_eq!(count, expected);
}

async fn cleanup_user(email: &str) {
    sqlx::query("DELETE FROM users WHERE email = $1")
        .bind(email)
        .execute(&pool().await)
        .await
        .unwrap();
}

#[db_test]
async fn account_repo_round_trip(_tx: &mut bikesnest_test_support::TestTx) {
    let db = Db::from_pool(pool().await);
    let repo = SqlxAccountRepository::new(db);
    let email = marker_email("repo");
    cleanup_user(&email).await;
    let eu = UserEmail::parse(&email).unwrap();

    let id = repo
        .create(bikesnest_application::NewAccount {
            email: &eu,
            display_name: Some("Ada"),
            password_hash: "$argon2id$test",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();

    let found = repo.find_by_email(&eu).await.unwrap().unwrap();
    assert_eq!(found.id, id);
    assert_eq!(found.account_state, AccountState::Active);
    assert!(found.has_role(Role::User), "USER is the implicit baseline");

    // Password identity carries the hash; subject == lowercased email.
    let idrec = repo
        .find_identity(AuthenticationProvider::Password, email.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(idrec.user_id, id);
    assert_eq!(idrec.credential_hash.as_deref(), Some("$argon2id$test"));

    // Role grant / revoke.
    repo.grant_role(id, Role::Moderator, id).await.unwrap();
    let roles = repo.roles(id).await.unwrap();
    assert!(roles.contains(&Role::Moderator));
    assert!(repo.revoke_role_guarded(id, Role::Moderator).await.unwrap());

    // Verify + list.
    repo.mark_email_verified(id, Utc::now()).await.unwrap();
    let found2 = repo.find_by_id(id).await.unwrap().unwrap();
    assert!(found2.is_verified());
    let all = repo.list_users().await.unwrap();
    assert!(all.iter().any(|u| u.id == id));

    cleanup_user(&email).await;
}

#[db_test]
async fn update_canonical_email_keeps_identity_in_sync(_tx: &mut bikesnest_test_support::TestTx) {
    let db = Db::from_pool(pool().await);
    let repo = SqlxAccountRepository::new(db);
    let email = marker_email("sync");
    cleanup_user(&email).await;
    let eu = UserEmail::parse(&email).unwrap();

    let id = repo
        .create(bikesnest_application::NewAccount {
            email: &eu,
            display_name: None,
            password_hash: "h",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();

    let new_email = UserEmail::parse("renamed@bikesnest.test").unwrap();
    repo.update_canonical_email(id, &new_email).await.unwrap();

    // Login lookup key (password subject) is now the new email; old subject gone.
    assert!(
        repo.find_identity(AuthenticationProvider::Password, email.as_str())
            .await
            .unwrap()
            .is_none()
    );
    let idrec = repo
        .find_identity(AuthenticationProvider::Password, new_email.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(idrec.user_id, id);

    cleanup_user(&email).await;
    cleanup_user("renamed@bikesnest.test").await;
}

#[db_test]
async fn session_store_create_resolve_expire_revoke(_tx: &mut bikesnest_test_support::TestTx) {
    let db = Db::from_pool(pool().await);
    let store = SqlxSessionStore::new(db);
    let email = marker_email("session");
    cleanup_user(&email).await;
    let (user_id,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    let user_id = bikesnest_domain::UserId(user_id);

    let raw = SessionId::new([7u8; 32]);
    let csrf = CsrfToken::new([9u8; 32]);
    let now = Utc::now();
    store.create(user_id, &raw, &csrf, now).await.unwrap();

    // Resolve succeeds and refreshes last_seen_at.
    let s = store.resolve(&raw, now).await.unwrap().unwrap();
    assert_eq!(s.user_id, user_id);
    assert_eq!(s.csrf_token.to_base64url(), csrf.to_base64url());

    // Absolute expiry: past the 90-day cap.
    assert!(
        store
            .resolve(&raw, now + Duration::days(91))
            .await
            .unwrap()
            .is_none()
    );
    // Idle expiry: 31 days without use.
    assert!(
        store
            .resolve(&raw, now + Duration::days(31))
            .await
            .unwrap()
            .is_none()
    );

    // Revoke.
    store.revoke(&raw).await.unwrap();
    assert!(store.resolve(&raw, now).await.unwrap().is_none());

    cleanup_user(&email).await;
}

#[db_test]
async fn token_store_single_use_is_atomic(_tx: &mut bikesnest_test_support::TestTx) {
    let db = Db::from_pool(pool().await);
    let store = SqlxTokenStore::new(db);
    let email = marker_email("token");
    cleanup_user(&email).await;
    let (user_id,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    let user_id = bikesnest_domain::UserId(user_id);
    let now = Utc::now();

    let raw = VerificationToken::new([42u8; 32]);
    store
        .issue_verification(
            user_id,
            &email,
            &raw,
            now,
            AccountState::PendingEmailVerification,
        )
        .await
        .unwrap();

    // Two concurrent consumes: exactly one wins (atomic used_at guard).
    let (a, b) = tokio::join!(
        store.consume_verification(&raw, now),
        store.consume_verification(&raw, now),
    );
    let hits = [a, b].iter().filter(|r| matches!(r, Ok(Some(_)))).count();
    assert_eq!(hits, 1, "single-use guard must allow exactly one consume");

    // The consumed token is no longer usable.
    assert!(
        store
            .consume_verification(&raw, now)
            .await
            .unwrap()
            .is_none()
    );

    // Reset token similarly single-use and short-lived.
    store.issue_reset(user_id, &raw, now).await.unwrap();
    assert!(store.consume_reset(&raw, now).await.unwrap().is_some());
    assert!(store.consume_reset(&raw, now).await.unwrap().is_none());

    cleanup_user(&email).await;
}

#[db_test]
async fn token_expiry_blocks_consumption_after_ttl(_tx: &mut bikesnest_test_support::TestTx) {
    let db = Db::from_pool(pool().await);
    let store = SqlxTokenStore::new(db);
    let email = marker_email("expiry");
    cleanup_user(&email).await;
    let (user_id,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    let user_id = bikesnest_domain::UserId(user_id);
    let now = Utc::now();
    let raw = VerificationToken::new([5u8; 32]);

    // Verification token: issued at `now`, TTL 24h — a consume at +25h is expired.
    store
        .issue_verification(
            user_id,
            &email,
            &raw,
            now,
            AccountState::PendingEmailVerification,
        )
        .await
        .unwrap();
    assert!(
        store
            .consume_verification(&raw, now + Duration::hours(25))
            .await
            .unwrap()
            .is_none()
    );

    // Reset token: TTL 1h — a consume at +2h is expired.
    store.issue_reset(user_id, &raw, now).await.unwrap();
    assert!(
        store
            .consume_reset(&raw, now + Duration::hours(2))
            .await
            .unwrap()
            .is_none()
    );

    cleanup_user(&email).await;
}

#[db_test]
async fn pending_account_confirmation_activates_without_changing_identity(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db);
    let email = UserEmail::parse(&unique_email("pending-confirm")).unwrap();
    let user_id = accounts
        .create(bikesnest_application::NewAccount {
            email: &email,
            display_name: None,
            password_hash: "pending-hash",
            state: AccountState::PendingEmailVerification,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    let token = VerificationToken::new([61; 32]);
    let now = Utc::now();
    assert!(
        tokens
            .issue_verification(
                user_id,
                email.as_str(),
                &token,
                now,
                AccountState::PendingEmailVerification,
            )
            .await
            .unwrap()
    );

    let outcome = accounts
        .confirm_email_verification(&token, now)
        .await
        .unwrap()
        .expect("pending account is eligible for initial confirmation");
    assert_eq!(outcome.user_id, user_id);
    assert!(!outcome.email_changed);
    let user = accounts.find_by_id(user_id).await.unwrap().unwrap();
    assert_eq!(user.account_state, AccountState::Active);
    assert!(user.email_verified_at.is_some());
    assert_eq!(user.email, email);
    let identity = accounts
        .find_identity(AuthenticationProvider::Password, email.as_str())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(identity.user_id, user_id);
    assert!(
        accounts
            .confirm_email_verification(&token, now)
            .await
            .unwrap()
            .is_none(),
        "confirmation remains single-use"
    );
}

#[db_test]
async fn confirmation_rejects_unused_tokens_for_suspended_and_deleted_accounts(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db);
    for (index, blocked_state) in [AccountState::Suspended, AccountState::Deleted]
        .into_iter()
        .enumerate()
    {
        let old_email = UserEmail::parse(&unique_email("blocked-confirm-old")).unwrap();
        let new_email = UserEmail::parse(&unique_email("blocked-confirm-new")).unwrap();
        let user_id = accounts
            .create(bikesnest_application::NewAccount {
                email: &old_email,
                display_name: None,
                password_hash: "blocked-hash",
                state: AccountState::Active,
                locale: bikesnest_domain::LocaleCode::PtBr,
            })
            .await
            .unwrap();
        let token = VerificationToken::new([71 + index as u8; 32]);
        let now = Utc::now();
        assert!(
            tokens
                .issue_verification(
                    user_id,
                    new_email.as_str(),
                    &token,
                    now,
                    AccountState::Active,
                )
                .await
                .unwrap()
        );
        accounts.set_state(user_id, blocked_state).await.unwrap();

        assert!(
            accounts
                .confirm_email_verification(&token, now)
                .await
                .unwrap()
                .is_none()
        );
        let user = accounts.find_by_id(user_id).await.unwrap().unwrap();
        assert_eq!(user.account_state, blocked_state);
        assert!(user.email_verified_at.is_none());
        assert_eq!(user.email, old_email);
        assert_eq!(
            accounts
                .find_identity(AuthenticationProvider::Password, old_email.as_str())
                .await
                .unwrap()
                .unwrap()
                .user_id,
            user_id
        );
        assert!(
            tokens
                .find_verification(&token, now)
                .await
                .unwrap()
                .is_some(),
            "the guard is exercised with a still-unused token"
        );
    }
}

#[db_test]
async fn token_issuance_rejects_mismatched_and_blocked_account_states(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db);
    let now = Utc::now();
    for (index, state) in [
        AccountState::PendingEmailVerification,
        AccountState::Active,
        AccountState::Suspended,
        AccountState::Deleted,
    ]
    .into_iter()
    .enumerate()
    {
        let email = UserEmail::parse(&unique_email("issuance-guard")).unwrap();
        let user_id = accounts
            .create(bikesnest_application::NewAccount {
                email: &email,
                display_name: None,
                password_hash: "guard-hash",
                state,
                locale: bikesnest_domain::LocaleCode::PtBr,
            })
            .await
            .unwrap();
        let expected_states: &[AccountState] = match state {
            AccountState::PendingEmailVerification => &[AccountState::Active],
            AccountState::Active => &[AccountState::PendingEmailVerification],
            AccountState::Suspended => &[
                AccountState::PendingEmailVerification,
                AccountState::Active,
                AccountState::Suspended,
            ],
            AccountState::Deleted => &[
                AccountState::PendingEmailVerification,
                AccountState::Active,
                AccountState::Deleted,
            ],
        };
        let reset = VerificationToken::new([111 + index as u8; 32]);

        for (expected_index, expected_state) in expected_states.iter().copied().enumerate() {
            let verification =
                VerificationToken::new([101 + (index * 4 + expected_index) as u8; 32]);
            assert!(
                !tokens
                    .issue_verification(
                        user_id,
                        email.as_str(),
                        &verification,
                        now,
                        expected_state,
                    )
                    .await
                    .unwrap(),
                "actual {state:?} must reject expected {expected_state:?}"
            );
            assert!(
                tokens
                    .find_verification(&verification, now)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        if matches!(state, AccountState::Suspended | AccountState::Deleted) {
            assert!(!tokens.issue_reset(user_id, &reset, now).await.unwrap());
            assert!(tokens.consume_reset(&reset, now).await.unwrap().is_none());
        }
    }
}

#[db_test]
async fn suspension_revokes_security_tokens_and_sessions_across_restore(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
    let sessions = SqlxSessionStore::new(db);
    let email = UserEmail::parse(&unique_email("suspension-atomic")).unwrap();
    let user_id = accounts
        .create(bikesnest_application::NewAccount {
            email: &email,
            display_name: None,
            password_hash: "hash",
            state: AccountState::PendingEmailVerification,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    let now = Utc::now();
    let verification = VerificationToken::new([81; 32]);
    let reset = VerificationToken::new([82; 32]);
    assert!(
        tokens
            .issue_verification(
                user_id,
                email.as_str(),
                &verification,
                now,
                AccountState::PendingEmailVerification,
            )
            .await
            .unwrap()
    );
    tokens.issue_reset(user_id, &reset, now).await.unwrap();
    let session = SessionId::new([83; 32]);
    sessions
        .create(user_id, &session, &CsrfToken::new([84; 32]), now)
        .await
        .unwrap();

    accounts
        .suspend_and_revoke_security_tokens(user_id)
        .await
        .unwrap();
    accounts
        .set_state(user_id, AccountState::Active)
        .await
        .unwrap();

    assert!(
        accounts
            .confirm_email_verification(&verification, now)
            .await
            .unwrap()
            .is_none(),
        "a pre-suspension verification token stays revoked after restore"
    );
    assert!(tokens.consume_reset(&reset, now).await.unwrap().is_none());
    assert!(sessions.resolve(&session, now).await.unwrap().is_none());
    let user = accounts.find_by_id(user_id).await.unwrap().unwrap();
    assert!(user.email_verified_at.is_none());
    assert_eq!(user.account_state, AccountState::Active);
}

#[db_test]
async fn failed_email_confirmation_rolls_back_token_consumption(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db);
    let old_email = UserEmail::parse(&unique_email("rollback-old")).unwrap();
    let occupied_email = UserEmail::parse(&unique_email("rollback-occupied")).unwrap();
    let freed_email = UserEmail::parse(&unique_email("rollback-freed")).unwrap();
    let source = accounts
        .create(bikesnest_application::NewAccount {
            email: &old_email,
            display_name: None,
            password_hash: "source-hash",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    let occupied = accounts
        .create(bikesnest_application::NewAccount {
            email: &occupied_email,
            display_name: None,
            password_hash: "occupied-hash",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    let token = VerificationToken::new([91; 32]);
    let now = Utc::now();
    assert!(
        tokens
            .issue_verification(
                source,
                occupied_email.as_str(),
                &token,
                now,
                AccountState::Active,
            )
            .await
            .unwrap()
    );

    assert_eq!(
        accounts.confirm_email_verification(&token, now).await,
        Err(bikesnest_application::AuthError::Conflict)
    );
    assert_eq!(
        accounts.find_by_id(source).await.unwrap().unwrap().email,
        old_email
    );

    accounts
        .update_canonical_email(occupied, &freed_email)
        .await
        .unwrap();
    let outcome = accounts
        .confirm_email_verification(&token, now)
        .await
        .unwrap()
        .expect("the failed transaction must leave the token usable");
    assert_eq!(outcome.user_id, source);
    assert!(outcome.email_changed);
    assert_eq!(
        accounts.find_by_id(source).await.unwrap().unwrap().email,
        occupied_email
    );
}

#[db_test]
async fn password_reset_atomically_updates_credential_revokes_sessions_and_competitors(
    tx: &mut bikesnest_test_support::TestTx,
) {
    for state in [AccountState::PendingEmailVerification, AccountState::Active] {
        let db = tx.db().await;
        let accounts = SqlxAccountRepository::new(db.clone());
        let tokens = SqlxTokenStore::new(db.clone());
        let sessions = SqlxSessionStore::new(db.clone());
        let email = UserEmail::parse(&unique_email("reset-atomic")).unwrap();
        let user_id = accounts
            .create(bikesnest_application::NewAccount {
                email: &email,
                display_name: None,
                password_hash: "old-hash",
                state,
                locale: bikesnest_domain::LocaleCode::PtBr,
            })
            .await
            .unwrap();
        let now = Utc::now();
        let first = VerificationToken::new([121 + state as u8; 32]);
        let competing = VerificationToken::new([123 + state as u8; 32]);
        assert!(tokens.issue_reset(user_id, &first, now).await.unwrap());
        assert!(tokens.issue_reset(user_id, &competing, now).await.unwrap());
        let session = SessionId::new([125 + state as u8; 32]);
        sessions
            .create(user_id, &session, &CsrfToken::new([127; 32]), now)
            .await
            .unwrap();

        assert_eq!(
            accounts
                .complete_password_reset(&first, "new-hash", now)
                .await
                .unwrap(),
            Some(user_id)
        );
        assert_eq!(
            accounts
                .find_by_id(user_id)
                .await
                .unwrap()
                .unwrap()
                .account_state,
            state
        );
        assert_eq!(
            accounts
                .find_identity(AuthenticationProvider::Password, email.as_str())
                .await
                .unwrap()
                .unwrap()
                .credential_hash
                .as_deref(),
            Some("new-hash")
        );
        assert!(sessions.resolve(&session, now).await.unwrap().is_none());
        assert!(
            tokens
                .consume_reset(&competing, now)
                .await
                .unwrap()
                .is_none()
        );
        assert_exact_password_reset_audit(&db, user_id, 1).await;
        assert!(
            accounts
                .complete_password_reset(&first, "later-hash", now)
                .await
                .unwrap()
                .is_none()
        );
        assert_exact_password_reset_audit(&db, user_id, 1).await;
    }
}

#[db_test]
async fn password_reset_rejects_expired_and_blocked_accounts_without_state_change(
    tx: &mut bikesnest_test_support::TestTx,
) {
    for (index, state) in [AccountState::Suspended, AccountState::Deleted]
        .into_iter()
        .enumerate()
    {
        let db = tx.db().await;
        let accounts = SqlxAccountRepository::new(db.clone());
        let tokens = SqlxTokenStore::new(db.clone());
        let sessions = SqlxSessionStore::new(db.clone());
        let email = UserEmail::parse(&unique_email("reset-blocked")).unwrap();
        let user_id = accounts
            .create(bikesnest_application::NewAccount {
                email: &email,
                display_name: None,
                password_hash: "old-hash",
                state: AccountState::Active,
                locale: bikesnest_domain::LocaleCode::PtBr,
            })
            .await
            .unwrap();
        let now = Utc::now();
        let token = VerificationToken::new([141 + index as u8; 32]);
        let competing = VerificationToken::new([145 + index as u8; 32]);
        assert!(tokens.issue_reset(user_id, &token, now).await.unwrap());
        assert!(tokens.issue_reset(user_id, &competing, now).await.unwrap());
        let session = SessionId::new([147 + index as u8; 32]);
        sessions
            .create(user_id, &session, &CsrfToken::new([149; 32]), now)
            .await
            .unwrap();
        accounts.set_state(user_id, state).await.unwrap();

        assert!(
            accounts
                .complete_password_reset(&token, "new-hash", now)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            accounts
                .find_by_id(user_id)
                .await
                .unwrap()
                .unwrap()
                .account_state,
            state
        );
        assert_eq!(
            accounts
                .find_identity(AuthenticationProvider::Password, email.as_str())
                .await
                .unwrap()
                .unwrap()
                .credential_hash
                .as_deref(),
            Some("old-hash")
        );
        assert!(sessions.resolve(&session, now).await.unwrap().is_some());
        assert_unused_reset_count(&db, user_id, 2).await;
        assert_exact_password_reset_audit(&db, user_id, 0).await;
        accounts
            .set_state(user_id, AccountState::Active)
            .await
            .unwrap();
        assert_eq!(
            tokens.consume_reset(&token, now).await.unwrap(),
            Some(user_id)
        );
        assert_eq!(
            tokens.consume_reset(&competing, now).await.unwrap(),
            Some(user_id)
        );
    }

    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
    let sessions = SqlxSessionStore::new(db.clone());
    let email = UserEmail::parse(&unique_email("reset-expired")).unwrap();
    let user_id = accounts
        .create(bikesnest_application::NewAccount {
            email: &email,
            display_name: None,
            password_hash: "old-hash",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    let now = Utc::now();
    let expired = VerificationToken::new([151; 32]);
    let expired_competing = VerificationToken::new([153; 32]);
    assert!(tokens.issue_reset(user_id, &expired, now).await.unwrap());
    assert!(
        tokens
            .issue_reset(user_id, &expired_competing, now)
            .await
            .unwrap()
    );
    let expired_session = SessionId::new([154; 32]);
    sessions
        .create(user_id, &expired_session, &CsrfToken::new([155; 32]), now)
        .await
        .unwrap();
    assert!(
        accounts
            .complete_password_reset(&expired, "new-hash", now + Duration::hours(2))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        accounts
            .find_identity(AuthenticationProvider::Password, email.as_str())
            .await
            .unwrap()
            .unwrap()
            .credential_hash
            .as_deref(),
        Some("old-hash")
    );
    assert!(
        sessions
            .resolve(&expired_session, now)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        tokens.consume_reset(&expired_competing, now).await.unwrap(),
        Some(user_id)
    );
    assert_unused_reset_count(&db, user_id, 1).await;
    assert_exact_password_reset_audit(&db, user_id, 0).await;

    let oauth_email = UserEmail::parse(&unique_email("reset-no-password")).unwrap();
    let oauth_user = accounts
        .create(bikesnest_application::NewAccount {
            email: &oauth_email,
            display_name: None,
            password_hash: "",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    let no_identity = VerificationToken::new([152; 32]);
    let no_identity_competing = VerificationToken::new([156; 32]);
    assert!(
        tokens
            .issue_reset(oauth_user, &no_identity, now)
            .await
            .unwrap()
    );
    assert!(
        tokens
            .issue_reset(oauth_user, &no_identity_competing, now)
            .await
            .unwrap()
    );
    let no_identity_session = SessionId::new([157; 32]);
    sessions
        .create(
            oauth_user,
            &no_identity_session,
            &CsrfToken::new([158; 32]),
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        accounts
            .complete_password_reset(&no_identity, "new-hash", now)
            .await,
        Err(bikesnest_application::AuthError::Internal)
    );
    assert_unused_reset_count(&db, oauth_user, 2).await;
    assert_eq!(
        tokens.consume_reset(&no_identity, now).await.unwrap(),
        Some(oauth_user)
    );
    assert_eq!(
        tokens
            .consume_reset(&no_identity_competing, now)
            .await
            .unwrap(),
        Some(oauth_user)
    );
    assert!(
        sessions
            .resolve(&no_identity_session, now)
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        accounts
            .find_identity(AuthenticationProvider::Password, oauth_email.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert_exact_password_reset_audit(&db, oauth_user, 0).await;
}

#[db_test]
async fn final_audit_failure_rolls_back_the_entire_password_reset(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
    let sessions = SqlxSessionStore::new(db.clone());
    let email = UserEmail::parse(&unique_email("reset-rollback")).unwrap();
    let user_id = accounts
        .create(bikesnest_application::NewAccount {
            email: &email,
            display_name: None,
            password_hash: "old-hash",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    let now = Utc::now();
    let token = VerificationToken::new([161; 32]);
    let competing = VerificationToken::new([164; 32]);
    assert!(tokens.issue_reset(user_id, &token, now).await.unwrap());
    assert!(tokens.issue_reset(user_id, &competing, now).await.unwrap());
    let session = SessionId::new([162; 32]);
    sessions
        .create(user_id, &session, &CsrfToken::new([163; 32]), now)
        .await
        .unwrap();
    {
        let mut conn = db.acquire().await.unwrap();
        sqlx::query(
            "CREATE TEMP TABLE audit_events (
                actor_user_id BIGINT, action TEXT, target_type TEXT, target_id TEXT,
                result TEXT CHECK (false), metadata JSONB
             ) ON COMMIT PRESERVE ROWS",
        )
        .execute(&mut *conn)
        .await
        .unwrap();
    }

    assert_eq!(
        accounts
            .complete_password_reset(&token, "new-hash", now)
            .await,
        Err(bikesnest_application::AuthError::Internal)
    );
    assert_eq!(
        accounts
            .find_identity(AuthenticationProvider::Password, email.as_str())
            .await
            .unwrap()
            .unwrap()
            .credential_hash
            .as_deref(),
        Some("old-hash")
    );
    assert!(sessions.resolve(&session, now).await.unwrap().is_some());
    {
        let mut conn = db.acquire().await.unwrap();
        let unused: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM password_reset_tokens
             WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id.0)
        .fetch_one(&mut *conn)
        .await
        .unwrap();
        assert_eq!(unused, 2, "both reset credentials must roll back");
    }
    {
        let mut conn = db.acquire().await.unwrap();
        sqlx::query("DROP TABLE pg_temp.audit_events")
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    assert_exact_password_reset_audit(&db, user_id, 0).await;
    assert_eq!(
        accounts
            .complete_password_reset(&token, "new-hash", now)
            .await
            .unwrap(),
        Some(user_id)
    );
    assert!(sessions.resolve(&session, now).await.unwrap().is_none());
    assert!(
        tokens
            .consume_reset(&competing, now)
            .await
            .unwrap()
            .is_none()
    );
    assert_exact_password_reset_audit(&db, user_id, 1).await;
}

#[test]
fn reset_expiring_during_account_lock_wait_is_rejected() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let accounts = SqlxAccountRepository::new(db.clone());
        let tokens = SqlxTokenStore::new(db.clone());
        let sessions = SqlxSessionStore::new(db.clone());
        let email = UserEmail::parse(&unique_email("reset-lock-expiry")).unwrap();
        let user_id = accounts
            .create(bikesnest_application::NewAccount {
                email: &email,
                display_name: None,
                password_hash: "old-hash",
                state: AccountState::Active,
                locale: bikesnest_domain::LocaleCode::PtBr,
            })
            .await
            .unwrap();
        let supplied_at = Utc::now();
        let token = VerificationToken::new([171; 32]);
        let competing = VerificationToken::new([172; 32]);
        assert!(
            tokens
                .issue_reset(user_id, &token, supplied_at)
                .await
                .unwrap()
        );
        assert!(
            tokens
                .issue_reset(user_id, &competing, supplied_at)
                .await
                .unwrap()
        );
        let session = SessionId::new([173; 32]);
        sessions
            .create(user_id, &session, &CsrfToken::new([174; 32]), supplied_at)
            .await
            .unwrap();

        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE id = $1 FOR UPDATE")
            .bind(user_id.0)
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        // The reset first observes the original one-hour deadline, then waits
        // on this account lock. Shorten the deadline inside the blocker so the
        // committed deadline is already past when the waiter resumes.
        sqlx::query(
            "UPDATE password_reset_tokens
             SET expires_at = clock_timestamp() + interval '300 milliseconds'
             WHERE user_id = $1",
        )
        .bind(user_id.0)
        .execute(&mut *blocker)
        .await
        .unwrap();

        let reset_accounts = SqlxAccountRepository::new(db.clone());
        let reset_task = tokio::spawn(async move {
            reset_accounts
                .complete_password_reset(&token, "new-hash", supplied_at)
                .await
        });
        let mut observed_wait = false;
        for _ in 0..50 {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (
                    SELECT 1 FROM pg_stat_activity
                    WHERE datname = current_database() AND wait_event_type = 'Lock'
                )",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if waiting {
                observed_wait = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(
            observed_wait,
            "reset transaction never reached the account lock"
        );
        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
        blocker.commit().await.unwrap();

        assert!(reset_task.await.unwrap().unwrap().is_none());
        assert_eq!(
            accounts
                .find_identity(AuthenticationProvider::Password, email.as_str())
                .await
                .unwrap()
                .unwrap()
                .credential_hash
                .as_deref(),
            Some("old-hash")
        );
        assert!(
            sessions
                .resolve(&session, Utc::now())
                .await
                .unwrap()
                .is_some()
        );
        let unused: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM password_reset_tokens
             WHERE user_id = $1 AND used_at IS NULL",
        )
        .bind(user_id.0)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unused, 2);
        assert_exact_password_reset_audit(&db, user_id, 0).await;
    });
}

#[test]
fn isolated_database_helper_cleans_up_after_panic() {
    let created_name = std::sync::Arc::new(std::sync::Mutex::new(None));
    let name_from_test = created_name.clone();
    let panic = std::panic::catch_unwind(|| {
        run_isolated_database_test(|pool: sqlx::PgPool| async move {
            let name: String = sqlx::query_scalar("SELECT current_database()")
                .fetch_one(&pool)
                .await
                .unwrap();
            *name_from_test.lock().unwrap() = Some(name);
            panic!("intentional isolated-helper cleanup test");
        });
    });
    assert!(panic.is_err());
    let dropped_name = created_name.lock().unwrap().clone().unwrap();

    let successfully_dropped = run_isolated_database_test(|_| async move {});
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM pg_database WHERE datname = ANY($1)
             )",
        )
        .bind(vec![dropped_name, successfully_dropped])
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(
            !exists,
            "successful and panicking helper databases must be gone"
        );
    });
}

#[db_test]
async fn audit_insert_round_trip(_tx: &mut bikesnest_test_support::TestTx) {
    let db = Db::from_pool(pool().await);
    let audit = SqlxAuditLog::new(db);
    let email = marker_email("audit");
    cleanup_user(&email).await;
    let (user_id,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&pool().await)
        .await
        .unwrap();

    audit
        .record(AuditEvent::success(
            Some(bikesnest_domain::UserId(user_id)),
            "auth.login",
            "user",
            user_id.to_string(),
        ))
        .await
        .unwrap();

    let (count,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM audit_events WHERE actor_user_id = $1 AND action = 'auth.login'",
    )
    .bind(user_id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(count, 1);

    cleanup_user(&email).await;
}

/// `resolve` runs on every authenticated request, so its `last_seen_at` write
/// is throttled to at most once per five minutes. The 30-day idle window
/// is unaffected: the column may lag by five minutes, which is immaterial
/// against 30 days.
#[db_test]
async fn resolve_throttles_the_last_seen_write(_tx: &mut bikesnest_test_support::TestTx) {
    let db = Db::from_pool(pool().await);
    let store = SqlxSessionStore::new(db);
    let email = marker_email("session-throttle");
    cleanup_user(&email).await;
    let (uid,): (i64,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    let user_id = bikesnest_domain::UserId(uid);

    let raw = SessionId::new([31u8; 32]);
    let csrf = CsrfToken::new([32u8; 32]);
    let now = Utc::now();
    store.create(user_id, &raw, &csrf, now).await.unwrap();

    async fn last_seen(uid: i64) -> chrono::DateTime<Utc> {
        sqlx::query_scalar("SELECT last_seen_at FROM sessions WHERE user_id = $1")
            .bind(uid)
            .fetch_one(&pool().await)
            .await
            .unwrap()
    }

    // Two resolves a minute apart: inside the throttle window, so the column is
    // left exactly as `create` wrote it.
    let before = last_seen(uid).await;
    assert!(store.resolve(&raw, now).await.unwrap().is_some());
    assert!(
        store
            .resolve(&raw, now + Duration::minutes(1))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        last_seen(uid).await,
        before,
        "last_seen_at must not be rewritten inside the throttle window"
    );

    // Age the row past the throttle: the next resolve does write.
    sqlx::query("UPDATE sessions SET last_seen_at = $2 WHERE user_id = $1")
        .bind(uid)
        .bind(now - Duration::minutes(10))
        .execute(&pool().await)
        .await
        .unwrap();
    let stale = last_seen(uid).await;
    let at = now + Duration::seconds(1);
    let session = store.resolve(&raw, at).await.unwrap().expect("still valid");
    // The row is returned as *read* — the update lands in the same statement,
    // under the same snapshot.
    assert_eq!(session.last_seen_at, stale);
    let refreshed = last_seen(uid).await;
    assert!(
        refreshed > stale,
        "a stale last_seen_at must be refreshed: {refreshed} vs {stale}"
    );
    assert_eq!(refreshed.timestamp(), at.timestamp());

    cleanup_user(&email).await;
}

// ---------------------------------------------------------------------------
// The admin user list is a searched, bounded page with batched
// counters, instead of "load every account and render it".
// ---------------------------------------------------------------------------

#[db_test]
async fn search_users_matches_email_or_name_and_pages_by_keyset(
    _tx: &mut bikesnest_test_support::TestTx,
) {
    let repo = SqlxAccountRepository::new(Db::from_pool(pool().await));
    let needle = format!("adminneedle{}", std::process::id());
    let mut ids = Vec::new();
    let mut emails = Vec::new();
    for n in 0..3 {
        let email = format!("{needle}-{n}@bikesnest.test");
        cleanup_user(&email).await;
        let eu = UserEmail::parse(&email).unwrap();
        let id = repo
            .create(bikesnest_application::NewAccount {
                email: &eu,
                display_name: Some(&format!("Admin Person {n}")),
                password_hash: "$argon2id$test",
                state: AccountState::Active,
                locale: bikesnest_domain::LocaleCode::PtBr,
            })
            .await
            .unwrap();
        ids.push(id.0);
        emails.push(email);
    }
    ids.sort_unstable();

    let search = |query: Option<&'static str>, after_id, limit| {
        repo.search_users(bikesnest_application::UserSearch {
            query,
            after_id,
            limit,
        })
    };

    // Matching on the email substring finds exactly these three.
    let hits = repo
        .search_users(bikesnest_application::UserSearch {
            query: Some(&needle),
            after_id: None,
            limit: 50,
        })
        .await
        .unwrap();
    assert_eq!(hits.len(), 3, "the search finds every matching account");
    // Newest id first — the order the keyset cursor walks.
    let got: Vec<i64> = hits.iter().map(|u| u.id.0).collect();
    let mut expected = ids.clone();
    expected.reverse();
    assert_eq!(got, expected);
    assert!(
        hits[0].has_role(Role::User),
        "roles are hydrated for the page, as the table renders them"
    );

    // Matching on the display name works too.
    let by_name = repo
        .search_users(bikesnest_application::UserSearch {
            query: Some("Admin Person 1"),
            after_id: None,
            limit: 50,
        })
        .await
        .unwrap();
    assert_eq!(by_name.len(), 1, "display-name search narrows to one");

    // Keyset paging: limit 2, then continue below the last id seen.
    let page1 = repo
        .search_users(bikesnest_application::UserSearch {
            query: Some(&needle),
            after_id: None,
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(page1.len(), 2);
    let page2 = repo
        .search_users(bikesnest_application::UserSearch {
            query: Some(&needle),
            after_id: Some(page1.last().unwrap().id.0),
            limit: 2,
        })
        .await
        .unwrap();
    assert_eq!(page2.len(), 1, "the second page holds the remainder");
    assert!(
        !page2.iter().any(|u| page1.iter().any(|p| p.id == u.id)),
        "the pages are disjoint"
    );

    // `_` matches only a literal underscore: the wildcards are escaped.
    let underscore = search(Some("admin_eedle"), None, 50).await.unwrap();
    assert!(
        underscore.is_empty(),
        "`_` is escaped, so it does not match any character"
    );
    // …and a literal underscore in the term matches a literal underscore
    // (the escape character must be the one the query declares).
    let under_email = format!("{needle}-under@bikesnest.test");
    cleanup_user(&under_email).await;
    let under = UserEmail::parse(&under_email).unwrap();
    repo.create(bikesnest_application::NewAccount {
        email: &under,
        display_name: Some("Admin Under_score"),
        password_hash: "$argon2id$test",
        state: AccountState::Active,
        locale: bikesnest_domain::LocaleCode::PtBr,
    })
    .await
    .unwrap();
    emails.push(under_email);
    let literal = search(Some("Under_sc"), None, 50).await.unwrap();
    assert_eq!(literal.len(), 1, "a literal `_` in the term matches itself");

    // Batched labels: display name wins over email, unknown ids are absent.
    let labels = repo.labels_for(&[ids[0], ids[1], -1]).await.unwrap();
    assert_eq!(labels.len(), 2, "an unknown id is simply absent");
    assert!(
        labels[&ids[0]].starts_with("Admin Person"),
        "the label prefers the display name: {:?}",
        labels[&ids[0]]
    );

    // With no display name, the label falls back to the email.
    sqlx::query("UPDATE users SET display_name = NULL WHERE id = $1")
        .bind(ids[0])
        .execute(&pool().await)
        .await
        .unwrap();
    let labels = repo.labels_for(&[ids[0]]).await.unwrap();
    assert!(
        labels[&ids[0]].contains(&needle),
        "no display name falls back to the email: {:?}",
        labels[&ids[0]]
    );
    // A blank display name is not a label either.
    sqlx::query("UPDATE users SET display_name = '   ' WHERE id = $1")
        .bind(ids[1])
        .execute(&pool().await)
        .await
        .unwrap();
    let labels = repo.labels_for(&[ids[1]]).await.unwrap();
    assert!(
        labels[&ids[1]].contains(&needle),
        "a whitespace-only name falls back too: {:?}",
        labels[&ids[1]]
    );

    assert!(repo.labels_for(&[]).await.unwrap().is_empty());

    for email in &emails {
        cleanup_user(email).await;
    }
}

#[db_test]
async fn activity_for_reports_last_seen_and_a_contribution_total(
    _tx: &mut bikesnest_test_support::TestTx,
) {
    let repo = SqlxAccountRepository::new(Db::from_pool(pool().await));
    let email = marker_email("admin-activity");
    cleanup_user(&email).await;
    let eu = UserEmail::parse(&email).unwrap();
    let id = repo
        .create(bikesnest_application::NewAccount {
            email: &eu,
            display_name: None,
            password_hash: "$argon2id$test",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap()
        .0;

    // A brand-new account: present in the map, with nothing to report.
    let activity = repo.activity_for(&[id]).await.unwrap();
    let a = activity[&id];
    assert_eq!(a.last_active_at, None, "never signed in");
    assert_eq!(a.contributions, 0);

    // A session gives it a last-seen; a location and a proposal give it two
    // contributions, counted in the same statement.
    let seen = Utc::now() - Duration::hours(2);
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, csrf_token, last_seen_at, expires_at) \
         VALUES ($1, $2, 'csrf', $3, $4)",
    )
    .bind(format!("admin-hash-{id}"))
    .bind(id)
    .bind(seen)
    .bind(Utc::now() + Duration::days(1))
    .execute(&pool().await)
    .await
    .unwrap();
    let (loc,): (i64,) = sqlx::query_as(
        "INSERT INTO parking_location \
           (name, address, parking_type, cost_kind, location, timezone, moderation_state, creator_id, seed_key) \
         VALUES ('Admin Activity', 'Rua X', 'rack', 'unknown', \
                 ST_SetSRID(ST_MakePoint(-49.27, -25.43), 4326)::geography, \
                 'America/Sao_Paulo', 'ACTIVE', $1, $2) RETURNING id",
    )
    .bind(id)
    .bind(format!("admin-activity-{id}"))
    .fetch_one(&pool().await)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
         VALUES ($1, $2, 1, 'change_existence', '{\"existence\":\"removed\"}'::jsonb, 'PENDING')",
    )
    .bind(loc)
    .bind(id)
    .execute(&pool().await)
    .await
    .unwrap();

    let a = repo.activity_for(&[id]).await.unwrap()[&id];
    assert!(
        a.last_active_at
            .is_some_and(|at| (at - seen).num_seconds().abs() < 2),
        "last-seen comes from the newest session: {:?}",
        a.last_active_at
    );
    assert_eq!(
        a.contributions, 2,
        "one location + one proposal, in a single batched query"
    );

    // Unknown ids come back with a zeroed row rather than being missing, so
    // the admin table always has a value to render.
    let batch = repo.activity_for(&[id, -1]).await.unwrap();
    assert_eq!(batch.len(), 2);
    assert_eq!(batch[&-1].contributions, 0);

    assert!(repo.activity_for(&[]).await.unwrap().is_empty());

    sqlx::query("DELETE FROM parking_location WHERE id = $1")
        .bind(loc)
        .execute(&pool().await)
        .await
        .unwrap();
    cleanup_user(&email).await;
}

/// `users.locale` round-trips: the registration locale is persisted, every read
/// model carries it, and the language toggle updates it. This column is the
/// only thing a background job can read to know which language to write in.
#[db_test]
async fn account_locale_is_persisted_and_updatable(_tx: &mut bikesnest_test_support::TestTx) {
    use bikesnest_domain::LocaleCode;

    let db = Db::from_pool(pool().await);
    let repo = SqlxAccountRepository::new(db);
    let email = marker_email("locale");
    cleanup_user(&email).await;
    let eu = UserEmail::parse(&email).unwrap();

    let id = repo
        .create(bikesnest_application::NewAccount {
            email: &eu,
            display_name: None,
            password_hash: "$argon2id$test",
            state: AccountState::Active,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();

    // Both read paths hydrate it (they are separate queries).
    assert_eq!(
        repo.find_by_id(id).await.unwrap().unwrap().locale,
        LocaleCode::En
    );
    assert_eq!(
        repo.find_by_email(&eu).await.unwrap().unwrap().locale,
        LocaleCode::En
    );

    repo.set_locale(id, LocaleCode::PtBr).await.unwrap();
    assert_eq!(
        repo.find_by_id(id).await.unwrap().unwrap().locale,
        LocaleCode::PtBr
    );
    // Stored in the canonical spelling the CHECK constraint allows.
    let stored: String = sqlx::query_scalar("SELECT locale FROM users WHERE id = $1")
        .bind(id.0)
        .fetch_one(&pool().await)
        .await
        .unwrap();
    assert_eq!(stored, "pt-BR");

    cleanup_user(&email).await;
}
