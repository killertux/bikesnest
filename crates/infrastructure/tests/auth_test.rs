//! Database-backed auth integration tests against real PostgreSQL.
//!
//! Sequential repositories share rollback-scoped fixtures. True multi-connection
//! races use owned disposable databases.

use bikesnest_application::{
    AccountRepository, AuditEvent, AuditLog, AuthOutbox, EmailKind, EmailMessage, NewAccount,
    SessionStore, TokenStore,
};
use bikesnest_domain::{
    AccountState, AuthenticationProvider, CsrfToken, LocaleCode, Role, SessionId, UserEmail,
    UserId, VerificationToken,
};
use bikesnest_infrastructure::{
    Db, FakeEmailProvider, SendEmailHandler, SqlxAccountRepository, SqlxAuditLog, SqlxAuthOutbox,
    SqlxSessionStore, SqlxTokenStore,
};
use bikesnest_test_support::{db_test, run_isolated_database_test};
use chrono::{DateTime, Duration, Utc};
use sha2::Digest as _;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn verification_message(
    account_id: UserId,
    email: &str,
    token: &VerificationToken,
) -> EmailMessage {
    EmailMessage::linked(
        account_id,
        email,
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

async fn confirm_email(
    db: &Db,
    token: &VerificationToken,
    at: DateTime<Utc>,
) -> Result<Option<bikesnest_application::EmailConfirmationOutcome>, bikesnest_application::AuthError>
{
    let tokens = SqlxTokenStore::new(db.clone());
    let Some(user_id) = tokens.find_verification(token, at).await? else {
        return Ok(None);
    };
    let Some(user) = SqlxAccountRepository::new(db.clone())
        .find_by_id(user_id)
        .await?
    else {
        return Ok(None);
    };
    let notice = EmailMessage::linked(
        user_id,
        user.email.as_str(),
        user.locale,
        EmailKind::EmailAddressChanged {
            account_link: "https://bikesnest.test/login".into(),
            notification_id: format!("test-confirm-{}", token.to_hex()),
        },
    );
    SqlxAuthOutbox::new(db.clone(), 3)
        .confirm_email(token, at, notice)
        .await
}

async fn complete_reset(
    db: &Db,
    token: &VerificationToken,
    hash: &str,
    at: DateTime<Utc>,
) -> Result<Option<UserId>, bikesnest_application::AuthError> {
    let tokens = SqlxTokenStore::new(db.clone());
    let Some(user_id) = tokens.find_reset(token, at).await? else {
        return Ok(None);
    };
    let Some(user) = SqlxAccountRepository::new(db.clone())
        .find_by_id(user_id)
        .await?
    else {
        return Ok(None);
    };
    let notice = EmailMessage::linked(
        user_id,
        user.email.as_str(),
        user.locale,
        EmailKind::PasswordChanged {
            account_link: "https://bikesnest.test/login".into(),
            notification_id: format!("test-reset-{}", token.to_hex()),
        },
    );
    SqlxAuthOutbox::new(db.clone(), 3)
        .complete_password_reset(token, hash, at, notice)
        .await
        .map(|mail| mail.map(|_| user_id))
}

#[db_test]
async fn authenticated_password_change_requires_owned_live_session_and_commits_notice_atomically(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let sessions = SqlxSessionStore::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
    let email = UserEmail::parse("notice-password@bikesnest.test").unwrap();
    let user_id = accounts
        .create(NewAccount {
            email: &email,
            display_name: None,
            password_hash: "old-hash",
            state: AccountState::Active,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();
    let at = Utc::now();
    let current = SessionId::new([0xe1; 32]);
    let other = SessionId::new([0xe2; 32]);
    sessions
        .create(user_id, &current, &CsrfToken::new([0xe3; 32]), at)
        .await
        .unwrap();
    sessions
        .create(user_id, &other, &CsrfToken::new([0xe4; 32]), at)
        .await
        .unwrap();
    let reset = VerificationToken::new([0xe5; 32]);
    assert!(tokens.issue_reset(user_id, &reset, at).await.unwrap());
    let notice = || {
        EmailMessage::linked(
            user_id,
            email.as_str(),
            LocaleCode::En,
            EmailKind::PasswordChanged {
                account_link: "https://bikesnest.test/login".into(),
                notification_id: "password-notice-stable".into(),
            },
        )
    };
    let outbox = SqlxAuthOutbox::new(db.clone(), 3);

    assert!(
        outbox
            .change_password(
                user_id,
                "old-hash",
                "must-not-stick",
                &SessionId::new([0xff; 32]),
                at,
                notice(),
            )
            .await
            .unwrap()
            .is_none()
    );
    let before: (Option<String>, i64, i64) = sqlx::query_as(
        "SELECT i.credential_hash,
          (SELECT count(*) FROM audit_events WHERE action='auth.password_changed' AND target_id=$1),
          (SELECT count(*) FROM background_job WHERE mail_purpose='password_changed' AND mail_account_id=$2)
         FROM authentication_identities i WHERE i.user_id=$2 AND i.provider='password'",
    )
    .bind(user_id.0.to_string())
    .bind(user_id.0)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(before, (Some("old-hash".into()), 0, 0));

    let other_email = UserEmail::parse("notice-password-other@bikesnest.test").unwrap();
    let other_user = accounts
        .create(NewAccount {
            email: &other_email,
            display_name: None,
            password_hash: "other-hash",
            state: AccountState::Active,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();
    let cross_account = SessionId::new([0xf1; 32]);
    sessions
        .create(other_user, &cross_account, &CsrfToken::new([0xf2; 32]), at)
        .await
        .unwrap();
    let revoked = SessionId::new([0xf3; 32]);
    sessions
        .create(user_id, &revoked, &CsrfToken::new([0xf4; 32]), at)
        .await
        .unwrap();
    sessions.revoke(&revoked).await.unwrap();
    let expired = SessionId::new([0xf5; 32]);
    sessions
        .create(
            user_id,
            &expired,
            &CsrfToken::new([0xf6; 32]),
            at - Duration::days(31),
        )
        .await
        .unwrap();
    let expired_hash: String = sha2::Sha256::digest(expired.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    sqlx::query("UPDATE sessions SET expires_at=$2 WHERE token_hash=$1")
        .bind(expired_hash)
        .bind(at - Duration::seconds(1))
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    for invalid in [&cross_account, &revoked, &expired] {
        assert!(
            outbox
                .change_password(user_id, "old-hash", "must-not-stick", invalid, at, notice())
                .await
                .unwrap()
                .is_none()
        );
    }
    let still_unchanged: (Option<String>, i64) = sqlx::query_as(
        "SELECT credential_hash,
          (SELECT count(*) FROM background_job WHERE mail_purpose='password_changed' AND mail_account_id=$1)
         FROM authentication_identities WHERE user_id=$1 AND provider='password'",
    )
    .bind(user_id.0)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(still_unchanged, (Some("old-hash".into()), 0));

    let admitted = outbox
        .change_password(user_id, "old-hash", "new-hash", &current, at, notice())
        .await
        .unwrap()
        .expect("live owned session admits the atomic transition");
    assert_eq!(admitted.message.kind.code(), "password_changed");
    assert!(sessions.resolve(&current, at).await.unwrap().is_some());
    assert!(sessions.resolve(&other, at).await.unwrap().is_none());
    assert!(tokens.find_reset(&reset, at).await.unwrap().is_none());
    let committed: (Option<String>, String, bool, bool, i64) = sqlx::query_as(
        "SELECT i.credential_hash,j.mail_purpose,
                j.mail_recipient_hash IS NOT NULL,j.mail_transition_audit_id IS NOT NULL,
                (SELECT count(*) FROM audit_events WHERE action='auth.password_changed' AND target_id=$1)
         FROM authentication_identities i JOIN background_job j ON j.id=$2
         WHERE i.user_id=$3 AND i.provider='password'",
    )
    .bind(user_id.0.to_string())
    .bind(admitted.job_id)
    .bind(user_id.0)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(
        committed,
        (
            Some("new-hash".into()),
            "password_changed".into(),
            true,
            true,
            1
        )
    );
}

#[test]
fn password_change_rechecks_session_expiry_after_waiting_for_its_row_lock() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let accounts = SqlxAccountRepository::new(db.clone());
        let sessions = SqlxSessionStore::new(db.clone());
        let email = UserEmail::parse("session-expiry-wait@bikesnest.test").unwrap();
        let user_id = accounts
            .create(NewAccount {
                email: &email,
                display_name: None,
                password_hash: "old-hash",
                state: AccountState::Active,
                locale: LocaleCode::En,
            })
            .await
            .unwrap();
        let supplied_at = Utc::now();
        let session = SessionId::new([0xf7; 32]);
        sessions
            .create(user_id, &session, &CsrfToken::new([0xf8; 32]), supplied_at)
            .await
            .unwrap();
        let session_hash: String = sha2::Sha256::digest(session.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let expires_at: DateTime<Utc> = sqlx::query_scalar(
            "UPDATE sessions SET expires_at=clock_timestamp()+interval '300 milliseconds'
             WHERE token_hash=$1 RETURNING expires_at",
        )
        .bind(&session_hash)
        .fetch_one(&pool)
        .await
        .unwrap();
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT token_hash FROM sessions WHERE token_hash=$1 FOR UPDATE")
            .bind(&session_hash)
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        let notice = EmailMessage::linked(
            user_id,
            email.as_str(),
            LocaleCode::En,
            EmailKind::PasswordChanged {
                account_link: "https://bikesnest.test/login".into(),
                notification_id: "session-expiry-wait".into(),
            },
        );
        let wait_db = db.clone();
        let wait_session = session.clone();
        let transition = tokio::spawn(async move {
            SqlxAuthOutbox::new(wait_db, 3)
                .change_password(
                    user_id,
                    "old-hash",
                    "new-hash",
                    &wait_session,
                    supplied_at,
                    notice,
                )
                .await
        });
        let mut observed_wait = false;
        for _ in 0..50 {
            observed_wait = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_stat_activity
                 WHERE datname=current_database() AND wait_event_type='Lock')",
            )
            .fetch_one(&pool)
            .await
            .unwrap();
            if observed_wait {
                break;
            }
            tokio::task::yield_now().await;
        }
        if !observed_wait {
            blocker.rollback().await.unwrap();
            transition.abort();
            let _ = transition.await;
            panic!("password transition must be waiting on the session row");
        }
        let clock_passed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                let now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                if now > expires_at {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        if clock_passed.is_err() {
            blocker.rollback().await.unwrap();
            transition.abort();
            let _ = transition.await;
            panic!("database clock must pass the committed session expiry");
        }
        blocker.commit().await.unwrap();
        assert!(transition.await.unwrap().unwrap().is_none());
        let persisted: (Option<String>, i64) = sqlx::query_as(
            "SELECT credential_hash,
             (SELECT count(*) FROM background_job WHERE mail_account_id=$1 AND mail_purpose='password_changed')
             FROM authentication_identities WHERE user_id=$1 AND provider='password'",
        )
        .bind(user_id.0)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(persisted, (Some("old-hash".into()), 0));
    });
}

#[db_test]
async fn registration_outbox_insert_failure_rolls_back_every_row(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let email = UserEmail::parse("atomic-register-fail@bikesnest.test").unwrap();
    let token = VerificationToken::new([0x91; 32]);
    let mut setup = db.acquire().await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION pg_temp.fail_auth_outbox() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected outbox failure'; END $$; CREATE TRIGGER fail_auth_outbox BEFORE INSERT ON background_job FOR EACH ROW EXECUTE FUNCTION pg_temp.fail_auth_outbox()")
        .execute(&mut *setup).await.unwrap();
    drop(setup);
    let outbox = SqlxAuthOutbox::new(db.clone(), 3);
    let result = outbox
        .register(
            NewAccount {
                email: &email,
                display_name: Some("Atomic"),
                password_hash: "hash",
                state: AccountState::PendingEmailVerification,
                locale: LocaleCode::En,
            },
            &token,
            Utc::now(),
            verification_message(UserId(0), email.as_str(), &token),
            None,
        )
        .await;
    assert!(result.is_err());
    let mut conn = db.acquire().await.unwrap();
    let counts: (i64,i64,i64,i64) = sqlx::query_as("SELECT (SELECT count(*) FROM users WHERE email=$1),(SELECT count(*) FROM email_verification_tokens t JOIN users u ON u.id=t.user_id WHERE u.email=$1),(SELECT count(*) FROM background_job WHERE payload->>'to'=$1),(SELECT count(*) FROM audit_events WHERE action='auth.register' AND target_id IN (SELECT id::text FROM users WHERE email=$1))")
        .bind(email.as_str()).fetch_one(&mut *conn).await.unwrap();
    assert_eq!(counts, (0, 0, 0, 0));
}

#[db_test]
async fn registration_commits_account_token_outbox_and_audit_together(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let email = UserEmail::parse("atomic-register-ok@bikesnest.test").unwrap();
    let token = VerificationToken::new([0x92; 32]);
    let admitted = SqlxAuthOutbox::new(db.clone(), 3)
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
            verification_message(UserId(0), email.as_str(), &token),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(admitted.job_id > 0);
    let row: (String,String,String,i64) = sqlx::query_as("SELECT u.account_state::text,t.email,j.mail_purpose,j.mail_account_id FROM users u JOIN email_verification_tokens t ON t.user_id=u.id JOIN background_job j ON j.mail_account_id=u.id WHERE u.email=$1")
        .bind(email.as_str()).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(row.0, "PENDING_EMAIL_VERIFICATION");
    assert_eq!(row.1, email.as_str());
    assert_eq!(row.2, "verify");
    assert_eq!(row.3, admitted.message.account_id);
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE actor_user_id=$1 AND action='auth.register'",
    )
    .bind(row.3)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(audits, 1);
}

#[db_test]
async fn registration_retry_replaces_pending_credential_and_revokes_earlier_access(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let email = UserEmail::parse("atomic-register-retry@bikesnest.test").unwrap();
    let first = VerificationToken::new([0x93; 32]);
    let outbox = SqlxAuthOutbox::new(db.clone(), 3);
    let new = || NewAccount {
        email: &email,
        display_name: Some("pre-registrant"),
        password_hash: "original-hash",
        state: AccountState::PendingEmailVerification,
        locale: LocaleCode::En,
    };
    let admitted = outbox
        .register(
            new(),
            &first,
            Utc::now(),
            verification_message(UserId(0), email.as_str(), &first),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    // The first registrant can sign in while pending; that session must not
    // survive someone else (possibly the real mailbox owner) re-registering.
    let user_id = UserId(admitted.message.account_id);
    let sessions = SqlxSessionStore::new(db.clone());
    let pending_session = SessionId::new([0x95; 32]);
    let now = Utc::now();
    sessions
        .create(user_id, &pending_session, &CsrfToken::new([0x96; 32]), now)
        .await
        .unwrap();
    let retry = VerificationToken::new([0x94; 32]);
    let reissued = outbox
        .register(
            NewAccount {
                email: &email,
                display_name: Some("mailbox owner"),
                password_hash: "replacement-hash",
                state: AccountState::PendingEmailVerification,
                locale: LocaleCode::PtBr,
            },
            &retry,
            Utc::now(),
            verification_message(UserId(0), email.as_str(), &retry),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_ne!(reissued.job_id, admitted.job_id);
    assert_eq!(reissued.message.account_id, user_id.0);
    assert_eq!(reissued.message.locale, LocaleCode::En);
    let identity: (String, String, Option<String>) = sqlx::query_as("SELECT credential_hash,u.locale,u.display_name FROM authentication_identities i JOIN users u ON u.id=i.user_id WHERE u.email=$1 AND i.provider='password'").bind(email.as_str()).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(
        identity,
        (
            "replacement-hash".into(),
            "en".into(),
            Some("mailbox owner".into())
        )
    );
    assert!(
        sessions
            .resolve(&pending_session, Utc::now())
            .await
            .unwrap()
            .is_none(),
        "the earlier registrant's pending session is revoked"
    );
    assert!(
        confirm_email(&db, &first, Utc::now())
            .await
            .unwrap()
            .is_none(),
        "a link minted for the replaced credential can no longer activate the account"
    );
    assert_eq!(unused_verification_tokens(&db, user_id).await, 1);
    let admitted = reissued;
    sqlx::query("UPDATE background_job SET state='pending',attempts=max_attempts WHERE id=$1")
        .bind(admitted.job_id)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let third = VerificationToken::new([0x97; 32]);
    let repaired = outbox
        .register(
            NewAccount {
                email: &email,
                display_name: None,
                password_hash: "replacement-hash",
                state: AccountState::PendingEmailVerification,
                locale: LocaleCode::PtBr,
            },
            &third,
            Utc::now(),
            verification_message(UserId(0), email.as_str(), &third),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    assert_ne!(repaired.job_id, admitted.job_id);
    assert_eq!(repaired.message.locale, LocaleCode::En);
    let exhausted: (String, serde_json::Value) =
        sqlx::query_as("SELECT state,payload FROM background_job WHERE id=$1")
            .bind(admitted.job_id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(exhausted, ("failed".into(), serde_json::json!({})));
    let counts:(i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM users WHERE email=$1),(SELECT count(*) FROM authentication_identities i JOIN users u ON u.id=i.user_id WHERE u.email=$1)").bind(email.as_str()).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(counts, (1, 1));
}

#[derive(Clone, Copy)]
enum RecoveryState {
    Expired,
    Missing,
    Succeeded,
    Failed,
    ActiveRunning,
    ActiveRunningExhausted,
}

async fn registration_recovery_case(
    tx: &mut bikesnest_test_support::TestTx,
    suffix: &str,
    state: RecoveryState,
) {
    let db = tx.db().await;
    let email = UserEmail::parse(&format!("recovery-{suffix}@bikesnest.test")).unwrap();
    let first = VerificationToken::new([suffix.as_bytes()[0]; 32]);
    let outbox = SqlxAuthOutbox::new(db.clone(), 3);
    let admitted = outbox
        .register(
            NewAccount {
                email: &email,
                display_name: Some("Original name"),
                password_hash: "original-hash",
                state: AccountState::PendingEmailVerification,
                locale: LocaleCode::En,
            },
            &first,
            Utc::now(),
            verification_message(UserId(0), email.as_str(), &first),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let account_before: (i64, Option<String>, String, String) = sqlx::query_as(
        "SELECT u.id,u.display_name,u.locale,i.credential_hash FROM users u JOIN authentication_identities i ON i.user_id=u.id AND i.provider='password' WHERE u.email=$1",
    )
    .bind(email.as_str())
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();

    let mutation = match state {
        RecoveryState::Expired => {
            sqlx::query("UPDATE email_verification_tokens SET expires_at=clock_timestamp()-interval '1 second' WHERE token_hash=(SELECT mail_token_hash FROM background_job WHERE id=$1)")
                .bind(admitted.job_id).execute(&mut *db.acquire().await.unwrap()).await.unwrap();
            "UPDATE background_job SET mail_token_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1"
        }
        RecoveryState::Missing => "DELETE FROM background_job WHERE id=$1",
        RecoveryState::Succeeded => {
            "UPDATE background_job SET state='succeeded',finished_at=clock_timestamp() WHERE id=$1"
        }
        RecoveryState::Failed => {
            "UPDATE background_job SET state='failed',finished_at=clock_timestamp(),payload='{}' WHERE id=$1"
        }
        RecoveryState::ActiveRunning => {
            "UPDATE background_job SET state='running',attempts=1,claimed_by='matrix-owner',lease_expires_at=clock_timestamp()+interval '5 minutes' WHERE id=$1"
        }
        RecoveryState::ActiveRunningExhausted => {
            "UPDATE background_job SET state='running',attempts=max_attempts,claimed_by='matrix-owner',lease_expires_at=clock_timestamp()+interval '5 minutes' WHERE id=$1"
        }
    };
    sqlx::query(mutation)
        .bind(admitted.job_id)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();

    let old_token_before: (String, String, DateTime<Utc>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT token_hash,email,expires_at,used_at FROM email_verification_tokens WHERE user_id=$1")
            .bind(account_before.0)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    let old_job_before: Option<(i64, String, i32, i32, serde_json::Value, Option<String>)> =
        sqlx::query_as("SELECT id,state,attempts,max_attempts,payload,claimed_by FROM background_job WHERE id=$1")
            .bind(admitted.job_id)
            .fetch_optional(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();

    let second = VerificationToken::new([suffix.as_bytes()[0].wrapping_add(1); 32]);
    let reissued = outbox
        .register(
            NewAccount {
                email: &email,
                display_name: Some("Replacement name"),
                password_hash: "replacement-hash",
                state: AccountState::PendingEmailVerification,
                locale: LocaleCode::PtBr,
            },
            &second,
            Utc::now(),
            verification_message(UserId(0), email.as_str(), &second),
            None,
        )
        .await
        .unwrap()
        .unwrap();

    let account_after: (i64, Option<String>, String, String) = sqlx::query_as(
        "SELECT u.id,u.display_name,u.locale,i.credential_hash FROM users u JOIN authentication_identities i ON i.user_id=u.id AND i.provider='password' WHERE u.email=$1",
    )
    .bind(email.as_str())
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(
        account_after,
        (
            account_before.0,
            Some("Replacement name".into()),
            "en".into(),
            "replacement-hash".into()
        )
    );
    // Whatever state the earlier delivery is in, its link was minted for the
    // replaced credential: it is retired and a fresh link is issued.
    let old_token_after: (String, String, DateTime<Utc>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT token_hash,email,expires_at,used_at FROM email_verification_tokens WHERE token_hash=$1")
            .bind(&old_token_before.0)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(old_token_after.0, old_token_before.0);
    assert_eq!(old_token_after.1, old_token_before.1);
    assert_eq!(old_token_after.2, old_token_before.2);
    assert!(old_token_after.3.is_some(), "the earlier link is retired");

    assert_ne!(reissued.job_id, admitted.job_id);
    assert_eq!(reissued.message.account_id, account_before.0);
    assert_eq!(reissued.message.locale, LocaleCode::En);
    let new_row: (String, String, i64, String, Option<DateTime<Utc>>) = sqlx::query_as("SELECT t.token_hash,t.email,j.id,j.state,t.used_at FROM email_verification_tokens t JOIN background_job j ON j.mail_token_hash=t.token_hash WHERE j.id=$1")
        .bind(reissued.job_id).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_ne!(new_row.0, old_token_before.0);
    assert_eq!(new_row.1, email.as_str());
    assert_eq!(new_row.2, reissued.job_id);
    assert_eq!(new_row.3, "pending");
    assert!(new_row.4.is_none());
    // An in-flight delivery row is left to its worker; its retired token makes
    // the worker drop it instead of sending a dead link.
    let old_job_after = sqlx::query_as(
        "SELECT id,state,attempts,max_attempts,payload,claimed_by FROM background_job WHERE id=$1",
    )
    .bind(admitted.job_id)
    .fetch_optional(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(old_job_after, old_job_before);
}

#[db_test]
async fn registration_recovery_repairs_expired_work(tx: &mut bikesnest_test_support::TestTx) {
    registration_recovery_case(tx, "expired", RecoveryState::Expired).await;
}
#[db_test]
async fn registration_recovery_repairs_missing_work(tx: &mut bikesnest_test_support::TestTx) {
    registration_recovery_case(tx, "missing", RecoveryState::Missing).await;
}
#[db_test]
async fn registration_recovery_repairs_succeeded_work(tx: &mut bikesnest_test_support::TestTx) {
    registration_recovery_case(tx, "succeeded", RecoveryState::Succeeded).await;
}
#[db_test]
async fn registration_recovery_repairs_failed_work(tx: &mut bikesnest_test_support::TestTx) {
    registration_recovery_case(tx, "failed", RecoveryState::Failed).await;
}
#[db_test]
async fn registration_retry_supersedes_active_running_work(
    tx: &mut bikesnest_test_support::TestTx,
) {
    registration_recovery_case(tx, "active", RecoveryState::ActiveRunning).await;
}
#[db_test]
async fn registration_retry_supersedes_active_exhausted_work(
    tx: &mut bikesnest_test_support::TestTx,
) {
    registration_recovery_case(tx, "exhausted", RecoveryState::ActiveRunningExhausted).await;
}

#[db_test]
async fn auth_mail_admission_failure_rolls_back_each_token_transition(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let repo = SqlxAccountRepository::new(db.clone());
    let active_email = UserEmail::parse("atomic-mail-active@bikesnest.test").unwrap();
    let active = repo
        .create(NewAccount {
            email: &active_email,
            display_name: None,
            password_hash: "hash",
            state: AccountState::Active,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();
    let pending_email = UserEmail::parse("atomic-mail-pending@bikesnest.test").unwrap();
    let pending = repo
        .create(NewAccount {
            email: &pending_email,
            display_name: None,
            password_hash: "hash",
            state: AccountState::PendingEmailVerification,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();
    let mut setup = db.acquire().await.unwrap();
    sqlx::raw_sql("CREATE FUNCTION pg_temp.fail_auth_mail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected mail failure'; END $$; CREATE TRIGGER fail_auth_mail BEFORE INSERT ON background_job FOR EACH ROW EXECUTE FUNCTION pg_temp.fail_auth_mail()").execute(&mut *setup).await.unwrap();
    drop(setup);
    let outbox = SqlxAuthOutbox::new(db.clone(), 3);
    let verify = VerificationToken::new([0xa1; 32]);
    assert!(
        outbox
            .issue_verification(
                pending,
                pending_email.as_str(),
                &verify,
                Utc::now(),
                AccountState::PendingEmailVerification,
                verification_message(pending, pending_email.as_str(), &verify),
                None
            )
            .await
            .is_err()
    );
    let reset = VerificationToken::new([0xa2; 32]);
    let reset_msg = EmailMessage::linked(
        active,
        active_email.as_str(),
        LocaleCode::En,
        EmailKind::ResetPassword {
            link: format!(
                "https://bikesnest.test/password-reset/new?token={}",
                reset.to_base64url()
            ),
            expires_at: None,
        },
    );
    assert!(
        outbox
            .issue_reset(active, &reset, Utc::now(), reset_msg)
            .await
            .is_err()
    );
    let change = VerificationToken::new([0xa3; 32]);
    let changed = "atomic-mail-new@bikesnest.test";
    let change_msg = EmailMessage::linked(
        active,
        changed,
        LocaleCode::En,
        EmailKind::ConfirmEmailChange {
            link: format!(
                "https://bikesnest.test/verify-email?token={}",
                change.to_base64url()
            ),
            expires_at: None,
        },
    );
    assert!(
        outbox
            .issue_verification(
                active,
                changed,
                &change,
                Utc::now(),
                AccountState::Active,
                change_msg,
                Some("auth.email_change_requested")
            )
            .await
            .is_err()
    );
    let counts:(i64,i64,i64)=sqlx::query_as("SELECT (SELECT count(*) FROM email_verification_tokens WHERE user_id=ANY($1)),(SELECT count(*) FROM password_reset_tokens WHERE user_id=ANY($1)),(SELECT count(*) FROM background_job WHERE mail_account_id=ANY($1))").bind([active.0,pending.0]).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
    let audits:i64=sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE actor_user_id=$1 AND action='auth.email_change_requested'").bind(active.0).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(audits, 0);
}

#[db_test]
async fn registration_final_audit_failure_rolls_back_account_token_and_job(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let audits_before: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE action='auth.register'")
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    sqlx::raw_sql("CREATE FUNCTION pg_temp.fail_register_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected final audit failure'; END $$; CREATE TRIGGER fail_register_audit BEFORE INSERT ON audit_events FOR EACH ROW WHEN (NEW.action='auth.register') EXECUTE FUNCTION pg_temp.fail_register_audit()")
        .execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    let email = UserEmail::parse("late-audit-register@bikesnest.test").unwrap();
    let token = VerificationToken::new([0xd1; 32]);
    let result = SqlxAuthOutbox::new(db.clone(), 3)
        .register(
            NewAccount {
                email: &email,
                display_name: Some("Late audit"),
                password_hash: "hash",
                state: AccountState::PendingEmailVerification,
                locale: LocaleCode::En,
            },
            &token,
            Utc::now(),
            verification_message(UserId(0), email.as_str(), &token),
            None,
        )
        .await;
    assert!(result.is_err());
    let counts: (i64, i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM users WHERE email=$1),(SELECT count(*) FROM email_verification_tokens t JOIN users u ON u.id=t.user_id WHERE u.email=$1),(SELECT count(*) FROM background_job WHERE payload->>'to'=$1),(SELECT count(*) FROM audit_events WHERE action='auth.register')")
        .bind(email.as_str()).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(counts, (0, 0, 0, audits_before));
}

#[db_test]
async fn email_change_final_audit_failure_rolls_back_token_and_job(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let original = UserEmail::parse("late-audit-change@bikesnest.test").unwrap();
    let account = SqlxAccountRepository::new(db.clone())
        .create(NewAccount {
            email: &original,
            display_name: Some("Original"),
            password_hash: "original-hash",
            state: AccountState::Active,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();
    let account_before: (String, Option<String>, String) =
        sqlx::query_as("SELECT email,display_name,locale FROM users WHERE id=$1")
            .bind(account.0)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    sqlx::raw_sql("CREATE FUNCTION pg_temp.fail_change_audit() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected final audit failure'; END $$; CREATE TRIGGER fail_change_audit BEFORE INSERT ON audit_events FOR EACH ROW WHEN (NEW.action='auth.email_change_requested') EXECUTE FUNCTION pg_temp.fail_change_audit()")
        .execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    let token = VerificationToken::new([0xd2; 32]);
    let changed = "late-audit-new@bikesnest.test";
    let result = SqlxAuthOutbox::new(db.clone(), 3)
        .issue_verification(
            account,
            changed,
            &token,
            Utc::now(),
            AccountState::Active,
            verification_message(account, changed, &token),
            Some("auth.email_change_requested"),
        )
        .await;
    assert!(result.is_err());
    let account_after: (String, Option<String>, String) =
        sqlx::query_as("SELECT email,display_name,locale FROM users WHERE id=$1")
            .bind(account.0)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(account_after, account_before);
    let counts: (i64, i64, i64) = sqlx::query_as("SELECT (SELECT count(*) FROM email_verification_tokens WHERE user_id=$1),(SELECT count(*) FROM background_job WHERE mail_account_id=$1),(SELECT count(*) FROM audit_events WHERE actor_user_id=$1 AND action='auth.email_change_requested')")
        .bind(account.0).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    assert_eq!(counts, (0, 0, 0));
}

fn unique_email(label: &str) -> String {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{}-{n}@bikesnest.test", std::process::id())
}

fn marker_email(label: &str) -> String {
    unique_email(label)
}

async fn persisted_account_state(
    db: &Db,
    user_id: bikesnest_domain::UserId,
) -> (
    String,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
) {
    sqlx::query_as(
        "SELECT account_state, suspended_at, deleted_at, updated_at FROM users WHERE id = $1",
    )
    .bind(user_id.0)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap()
}

async fn unused_verification_tokens(db: &Db, user_id: bikesnest_domain::UserId) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM email_verification_tokens
         WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind(user_id.0)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap()
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

#[db_test]
async fn account_repo_round_trip(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let repo = SqlxAccountRepository::new(db.clone());
    let email = marker_email("repo");

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
}

#[db_test]
async fn update_canonical_email_keeps_identity_in_sync(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let repo = SqlxAccountRepository::new(db.clone());
    let email = marker_email("sync");

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
}

#[db_test]
async fn session_store_create_resolve_expire_revoke(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let store = SqlxSessionStore::new(db.clone());
    let email = marker_email("session");

    let (user_id,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&mut *db.acquire().await.unwrap())
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
}

#[test]
fn token_store_single_use_is_atomic() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let store = SqlxTokenStore::new(db);
        let email = marker_email("token");

        let (user_id,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
            .bind(&email)
            .fetch_one(&pool)
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

        // Hold the exact token while both real consumers reach their row locks.
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT token_hash FROM email_verification_tokens WHERE user_id=$1 FOR UPDATE")
            .bind(user_id.0)
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        let mut consumes = Box::pin(async {
            tokio::join!(
                store.consume_verification(&raw, now),
                store.consume_verification(&raw, now),
            )
        });
        let observed = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let count: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'",
                    ).fetch_one(&pool).await.unwrap();
                    if count >= 2 { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            }) => result.is_ok(),
            _ = &mut consumes => false,
        };
        blocker.rollback().await.unwrap();
        assert!(observed, "both consumers must overlap at the token lock");
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(10), consumes)
            .await
            .expect("both consumers finish after lock release");
        assert!(
            a.is_ok() && b.is_ok(),
            "neither consumer may fail: {a:?}, {b:?}"
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
    });
}

#[db_test]
async fn token_expiry_blocks_consumption_after_ttl(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let store = SqlxTokenStore::new(db.clone());
    let email = marker_email("expiry");

    let (user_id,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&mut *db.acquire().await.unwrap())
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
}

#[db_test]
async fn pending_account_confirmation_activates_without_changing_identity(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
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

    let outcome = confirm_email(&db, &token, now)
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
        confirm_email(&db, &token, now).await.unwrap().is_none(),
        "confirmation remains single-use"
    );
}

#[db_test]
async fn first_verification_revokes_sessions_opened_while_pending(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let email = UserEmail::parse(&unique_email("pending-session")).unwrap();
    let token = VerificationToken::new([0x31; 32]);
    let admitted = SqlxAuthOutbox::new(db.clone(), 3)
        .register(
            NewAccount {
                email: &email,
                display_name: None,
                password_hash: "pre-registrant-hash",
                state: AccountState::PendingEmailVerification,
                locale: LocaleCode::En,
            },
            &token,
            Utc::now(),
            verification_message(UserId(0), email.as_str(), &token),
            None,
        )
        .await
        .unwrap()
        .unwrap();
    let user_id = UserId(admitted.message.account_id);
    let sessions = SqlxSessionStore::new(db.clone());
    let pending_session = SessionId::new([0x32; 32]);
    let now = Utc::now();
    sessions
        .create(user_id, &pending_session, &CsrfToken::new([0x33; 32]), now)
        .await
        .unwrap();
    assert!(
        sessions
            .resolve(&pending_session, now)
            .await
            .unwrap()
            .is_some()
    );

    let outcome = confirm_email(&db, &token, now).await.unwrap().unwrap();
    assert!(!outcome.email_changed);
    assert!(
        sessions
            .resolve(&pending_session, now)
            .await
            .unwrap()
            .is_none(),
        "a session opened before the mailbox owner verified must not survive activation"
    );
}

#[db_test]
async fn confirming_one_link_retires_every_other_outstanding_link(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
    let email = UserEmail::parse(&unique_email("older-link")).unwrap();
    let user_id = accounts
        .create(NewAccount {
            email: &email,
            display_name: None,
            password_hash: "hash",
            state: AccountState::PendingEmailVerification,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();
    let now = Utc::now();
    let older = VerificationToken::new([0x34; 32]);
    let newer = VerificationToken::new([0x35; 32]);
    for token in [&older, &newer] {
        assert!(
            tokens
                .issue_verification(
                    user_id,
                    email.as_str(),
                    token,
                    now,
                    AccountState::PendingEmailVerification,
                )
                .await
                .unwrap()
        );
    }
    assert!(confirm_email(&db, &newer, now).await.unwrap().is_some());
    assert_eq!(unused_verification_tokens(&db, user_id).await, 0);

    // A pending email-change link issued before a later confirmation dies too.
    let change_to = UserEmail::parse(&unique_email("older-link-change")).unwrap();
    let change = VerificationToken::new([0x36; 32]);
    let reverify = VerificationToken::new([0x37; 32]);
    assert!(
        tokens
            .issue_verification(
                user_id,
                change_to.as_str(),
                &change,
                now,
                AccountState::Active
            )
            .await
            .unwrap()
    );
    assert!(
        tokens
            .issue_verification(
                user_id,
                email.as_str(),
                &reverify,
                now,
                AccountState::Active
            )
            .await
            .unwrap()
    );
    assert!(confirm_email(&db, &reverify, now).await.unwrap().is_some());
    assert!(confirm_email(&db, &older, now).await.unwrap().is_none());
    assert!(confirm_email(&db, &change, now).await.unwrap().is_none());
    assert_eq!(
        accounts.find_by_id(user_id).await.unwrap().unwrap().email,
        email
    );
}

#[db_test]
async fn password_change_retires_pending_email_change_links(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
    let sessions = SqlxSessionStore::new(db.clone());
    let email = UserEmail::parse(&unique_email("pw-change-link")).unwrap();
    let user_id = accounts
        .create(NewAccount {
            email: &email,
            display_name: None,
            password_hash: "old-hash",
            state: AccountState::Active,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();
    let now = Utc::now();
    let current = SessionId::new([0x38; 32]);
    sessions
        .create(user_id, &current, &CsrfToken::new([0x39; 32]), now)
        .await
        .unwrap();
    let change_to = UserEmail::parse(&unique_email("pw-change-link-attacker")).unwrap();
    let change = VerificationToken::new([0x3a; 32]);
    assert!(
        tokens
            .issue_verification(
                user_id,
                change_to.as_str(),
                &change,
                now,
                AccountState::Active
            )
            .await
            .unwrap()
    );
    let notice = EmailMessage::linked(
        user_id,
        email.as_str(),
        LocaleCode::En,
        EmailKind::PasswordChanged {
            account_link: "https://bikesnest.test/login".into(),
            notification_id: "pw-change-link".into(),
        },
    );
    assert!(
        SqlxAuthOutbox::new(db.clone(), 3)
            .change_password(user_id, "old-hash", "new-hash", &current, now, notice)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(unused_verification_tokens(&db, user_id).await, 0);
    assert!(confirm_email(&db, &change, now).await.unwrap().is_none());
    assert_eq!(
        accounts.find_by_id(user_id).await.unwrap().unwrap().email,
        email
    );
}

#[db_test]
async fn password_reset_retires_pending_email_change_links(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
    let email = UserEmail::parse(&unique_email("reset-link")).unwrap();
    let user_id = accounts
        .create(NewAccount {
            email: &email,
            display_name: None,
            password_hash: "old-hash",
            state: AccountState::Active,
            locale: LocaleCode::En,
        })
        .await
        .unwrap();
    let now = Utc::now();
    let change_to = UserEmail::parse(&unique_email("reset-link-attacker")).unwrap();
    let change = VerificationToken::new([0x3b; 32]);
    assert!(
        tokens
            .issue_verification(
                user_id,
                change_to.as_str(),
                &change,
                now,
                AccountState::Active
            )
            .await
            .unwrap()
    );
    let reset = VerificationToken::new([0x3c; 32]);
    assert!(tokens.issue_reset(user_id, &reset, now).await.unwrap());
    assert_eq!(
        complete_reset(&db, &reset, "new-hash", now).await.unwrap(),
        Some(user_id)
    );
    assert_eq!(unused_verification_tokens(&db, user_id).await, 0);
    assert!(confirm_email(&db, &change, now).await.unwrap().is_none());
    assert_eq!(
        accounts.find_by_id(user_id).await.unwrap().unwrap().email,
        email
    );
}

#[db_test]
async fn confirmation_rejects_unused_tokens_for_suspended_and_deleted_accounts(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
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

        assert!(confirm_email(&db, &token, now).await.unwrap().is_none());
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
    let sessions = SqlxSessionStore::new(db.clone());
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

    accounts.suspend_by_admin(user_id, user_id).await.unwrap();
    accounts.restore_by_admin(user_id, user_id).await.unwrap();

    assert!(
        confirm_email(&db, &verification, now)
            .await
            .unwrap()
            .is_none(),
        "a pre-suspension verification token stays revoked after restore"
    );
    assert!(tokens.consume_reset(&reset, now).await.unwrap().is_none());
    assert!(sessions.resolve(&session, now).await.unwrap().is_none());
    let user = accounts.find_by_id(user_id).await.unwrap().unwrap();
    assert!(user.email_verified_at.is_none());
    assert_eq!(user.account_state, AccountState::PendingEmailVerification);
}

#[db_test]
async fn administrator_account_transition_state_matrix_and_exact_audits(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let create = |label: &'static str, state| {
        let accounts = &accounts;
        async move {
            let email = UserEmail::parse(&unique_email(label)).unwrap();
            accounts
                .create(bikesnest_application::NewAccount {
                    email: &email,
                    display_name: None,
                    password_hash: "hash",
                    state,
                    locale: bikesnest_domain::LocaleCode::PtBr,
                })
                .await
                .unwrap()
        }
    };
    let actor = create("state-actor", AccountState::Active).await;
    let active = create("state-active", AccountState::Active).await;
    let pending = create("state-pending", AccountState::PendingEmailVerification).await;
    let suspended_verified = create("state-suspended-verified", AccountState::Suspended).await;
    let suspended_unverified = create("state-suspended-unverified", AccountState::Suspended).await;
    let deleted = create("state-deleted", AccountState::Deleted).await;
    accounts
        .mark_email_verified(active, Utc::now())
        .await
        .unwrap();
    accounts
        .mark_email_verified(suspended_verified, Utc::now())
        .await
        .unwrap();
    for (index, id) in [
        active,
        pending,
        suspended_verified,
        suspended_unverified,
        deleted,
    ]
    .into_iter()
    .enumerate()
    {
        sqlx::query(
            "UPDATE users SET suspended_at = TIMESTAMPTZ '1990-01-01 00:00:00+00'
                    + ($2 * interval '1 day'),
                 updated_at = TIMESTAMPTZ '1991-01-01 00:00:00+00'
                    + ($2 * interval '1 day')
             WHERE id = $1",
        )
        .bind(id.0)
        .bind(index as i32)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    }
    let active_before_noop = persisted_account_state(&db, active).await;
    let pending_before_noop = persisted_account_state(&db, pending).await;
    let deleted_before_noop = persisted_account_state(&db, deleted).await;

    assert!(!accounts.restore_by_admin(active, actor).await.unwrap());
    assert!(!accounts.restore_by_admin(pending, actor).await.unwrap());
    assert!(!accounts.suspend_by_admin(deleted, actor).await.unwrap());
    assert!(!accounts.restore_by_admin(deleted, actor).await.unwrap());
    assert_eq!(
        persisted_account_state(&db, active).await,
        active_before_noop
    );
    assert_eq!(
        persisted_account_state(&db, pending).await,
        pending_before_noop
    );
    assert_eq!(
        persisted_account_state(&db, deleted).await,
        deleted_before_noop
    );
    assert!(
        !accounts
            .restore_by_admin(bikesnest_domain::UserId(-1), actor)
            .await
            .unwrap()
    );
    assert!(
        !accounts
            .suspend_by_admin(bikesnest_domain::UserId(-1), actor)
            .await
            .unwrap()
    );

    assert!(accounts.suspend_by_admin(active, actor).await.unwrap());
    let active_suspended_before_noop = persisted_account_state(&db, active).await;
    assert!(!accounts.suspend_by_admin(active, actor).await.unwrap());
    assert_eq!(
        persisted_account_state(&db, active).await,
        active_suspended_before_noop
    );
    assert!(accounts.restore_by_admin(active, actor).await.unwrap());
    assert!(accounts.suspend_by_admin(pending, actor).await.unwrap());
    assert!(accounts.restore_by_admin(pending, actor).await.unwrap());
    assert!(
        accounts
            .restore_by_admin(suspended_verified, actor)
            .await
            .unwrap()
    );
    assert!(
        accounts
            .restore_by_admin(suspended_unverified, actor)
            .await
            .unwrap()
    );

    for (id, expected) in [
        (active, AccountState::Active),
        (pending, AccountState::PendingEmailVerification),
        (suspended_verified, AccountState::Active),
        (suspended_unverified, AccountState::PendingEmailVerification),
        (deleted, AccountState::Deleted),
    ] {
        assert_eq!(
            accounts
                .find_by_id(id)
                .await
                .unwrap()
                .unwrap()
                .account_state,
            expected
        );
    }
    let rows: Vec<(
        Option<i64>,
        String,
        String,
        String,
        String,
        serde_json::Value,
    )> = sqlx::query_as(
        "SELECT actor_user_id, action, target_type, target_id, result, metadata
         FROM audit_events
         WHERE action IN ('user.suspended', 'user.restored')
           AND target_id = ANY($1)
         ORDER BY id",
    )
    .bind(vec![
        active.0.to_string(),
        pending.0.to_string(),
        suspended_verified.0.to_string(),
        suspended_unverified.0.to_string(),
        deleted.0.to_string(),
        "-1".to_string(),
    ])
    .fetch_all(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(rows.len(), 6, "only six real transitions are audited");
    assert_eq!(
        rows.iter().filter(|row| row.1 == "user.suspended").count(),
        2
    );
    assert_eq!(
        rows.iter().filter(|row| row.1 == "user.restored").count(),
        4
    );
    assert!(rows.iter().all(|row| {
        row.0 == Some(actor.0)
            && row.2 == "user"
            && row.4 == "success"
            && row.5 == serde_json::json!({})
            && row.3 != deleted.0.to_string()
            && row.3 != "-1"
    }));
}

#[db_test]
async fn administrator_transition_audit_failure_rolls_back_state_and_revocations(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
    let sessions = SqlxSessionStore::new(db.clone());
    let actor_email = UserEmail::parse(&unique_email("transition-rollback-actor")).unwrap();
    let actor = accounts
        .create(bikesnest_application::NewAccount {
            email: &actor_email,
            display_name: None,
            password_hash: "hash",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    let email = UserEmail::parse(&unique_email("transition-rollback-target")).unwrap();
    let target = accounts
        .create(bikesnest_application::NewAccount {
            email: &email,
            display_name: None,
            password_hash: "hash",
            state: AccountState::Active,
            locale: bikesnest_domain::LocaleCode::PtBr,
        })
        .await
        .unwrap();
    accounts
        .mark_email_verified(target, Utc::now())
        .await
        .unwrap();
    let now = Utc::now();
    let reset = VerificationToken::new([191; 32]);
    assert!(tokens.issue_reset(target, &reset, now).await.unwrap());
    let verification = VerificationToken::new([194; 32]);
    assert!(
        tokens
            .issue_verification(
                target,
                email.as_str(),
                &verification,
                now,
                AccountState::Active,
            )
            .await
            .unwrap()
    );
    let session = SessionId::new([192; 32]);
    sessions
        .create(target, &session, &CsrfToken::new([193; 32]), now)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE users SET suspended_at = TIMESTAMPTZ '2001-02-03 04:05:06+00',
             updated_at = TIMESTAMPTZ '2002-03-04 05:06:07+00' WHERE id = $1",
    )
    .bind(target.0)
    .execute(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    let before_failed_suspend = persisted_account_state(&db, target).await;
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
        accounts.suspend_by_admin(target, actor).await,
        Err(bikesnest_application::AuthError::Internal)
    );
    assert_eq!(
        persisted_account_state(&db, target).await,
        before_failed_suspend
    );
    assert!(sessions.resolve(&session, now).await.unwrap().is_some());
    assert_eq!(unused_verification_tokens(&db, target).await, 1);
    assert_eq!(
        tokens.consume_reset(&reset, now).await.unwrap(),
        Some(target)
    );
    {
        let mut conn = db.acquire().await.unwrap();
        sqlx::query("DROP TABLE pg_temp.audit_events")
            .execute(&mut *conn)
            .await
            .unwrap();
    }
    assert!(accounts.suspend_by_admin(target, actor).await.unwrap());
    let suspended = persisted_account_state(&db, target).await;
    assert_eq!(suspended.0, "SUSPENDED");
    assert!(suspended.1.is_some());
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
        accounts.restore_by_admin(target, actor).await,
        Err(bikesnest_application::AuthError::Internal)
    );
    assert_eq!(persisted_account_state(&db, target).await, suspended);
    assert_eq!(unused_verification_tokens(&db, target).await, 0);
}

#[db_test]
async fn failed_email_confirmation_rolls_back_token_consumption(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let accounts = SqlxAccountRepository::new(db.clone());
    let tokens = SqlxTokenStore::new(db.clone());
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
        confirm_email(&db, &token, now).await,
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
    let outcome = confirm_email(&db, &token, now)
        .await
        .unwrap()
        .expect("the failed transaction must leave the token usable");
    assert_eq!(outcome.user_id, source);
    assert!(outcome.email_changed);
    let notice = outcome
        .mail
        .expect("old address warning is admitted atomically");
    assert_eq!(notice.message.to, old_email.as_str());
    assert_eq!(notice.message.kind.code(), "email_changed");
    let notice_metadata: (bool, bool) = sqlx::query_as(
        "SELECT mail_recipient_hash IS NOT NULL,mail_transition_audit_id IS NOT NULL
         FROM background_job WHERE id=$1",
    )
    .bind(notice.job_id)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(notice_metadata, (true, true));
    let provider = std::sync::Arc::new(FakeEmailProvider::with_root(None));
    bikesnest_application::JobHandler::run(
        &SendEmailHandler::new(db.clone(), provider.clone()),
        &serde_json::to_value(&notice.message).unwrap(),
    )
    .await
    .unwrap();
    let sent = provider.emails();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, old_email.as_str());
    assert_eq!(sent[0].kind, "email_changed");
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
            complete_reset(&db, &first, "new-hash", now).await.unwrap(),
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
            complete_reset(&db, &first, "later-hash", now)
                .await
                .unwrap()
                .is_none()
        );
        assert_exact_password_reset_audit(&db, user_id, 1).await;
    }
}

#[test]
fn two_independent_password_reset_consumers_commit_exactly_one_transition() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let accounts = SqlxAccountRepository::new(db.clone());
        let tokens = SqlxTokenStore::new(db.clone());
        let sessions = SqlxSessionStore::new(db.clone());
        let email = UserEmail::parse(&unique_email("reset-two-consumers")).unwrap();
        let user_id = accounts
            .create(NewAccount {
                email: &email,
                display_name: None,
                password_hash: "old-hash",
                state: AccountState::Active,
                locale: LocaleCode::En,
            })
            .await
            .unwrap();
        let now = Utc::now();
        let first = VerificationToken::new([181; 32]);
        let second = VerificationToken::new([182; 32]);
        let first_notice_id = format!("test-reset-{}", first.to_hex());
        let second_notice_id = format!("test-reset-{}", second.to_hex());
        assert!(tokens.issue_reset(user_id, &first, now).await.unwrap());
        assert!(tokens.issue_reset(user_id, &second, now).await.unwrap());
        let session = SessionId::new([183; 32]);
        sessions
            .create(user_id, &session, &CsrfToken::new([184; 32]), now)
            .await
            .unwrap();

        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE id=$1 FOR UPDATE")
            .bind(user_id.0)
            .fetch_one(&mut *blocker)
            .await
            .unwrap();
        let first_db = db.clone();
        let second_db = db.clone();
        let mut resets = Box::pin(async move {
            tokio::join!(
                complete_reset(&first_db, &first, "first-hash", now),
                complete_reset(&second_db, &second, "second-hash", now),
            )
        });
        let overlapped = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let waiting: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM pg_stat_activity
                         WHERE datname=current_database() AND wait_event_type='Lock'",
                    )
                    .fetch_one(&pool)
                    .await
                    .unwrap();
                    if waiting >= 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }) => result.is_ok(),
            _ = &mut resets => false,
        };
        blocker.rollback().await.unwrap();
        assert!(
            overlapped,
            "both reset transitions must reach the account lock"
        );
        let (first_result, second_result) =
            tokio::time::timeout(std::time::Duration::from_secs(10), resets)
                .await
                .expect("both reset transitions finish after lock release");
        let first_won = first_result.unwrap().is_some();
        let second_won = second_result.unwrap().is_some();
        assert_ne!(first_won, second_won, "exactly one reset may commit");

        let stored_hash: String = sqlx::query_scalar(
            "SELECT credential_hash FROM authentication_identities
             WHERE user_id=$1 AND provider='password'",
        )
        .bind(user_id.0)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            stored_hash,
            if first_won {
                "first-hash"
            } else {
                "second-hash"
            }
        );
        assert!(
            sessions
                .resolve(&session, Utc::now())
                .await
                .unwrap()
                .is_none()
        );
        let unused: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM password_reset_tokens WHERE user_id=$1 AND used_at IS NULL",
        )
        .bind(user_id.0)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(unused, 0, "the winner invalidates every competing reset");
        let audit_count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM audit_events
             WHERE target_id=$1 AND action='auth.password_changed' AND result='success'",
        )
        .bind(user_id.0.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(audit_count, 1);
        let (job_count, notification_id): (i64, Option<String>) = sqlx::query_as(
            "SELECT count(*), max(payload->>'notification_id')
             FROM background_job WHERE mail_account_id=$1 AND mail_purpose='password_changed'",
        )
        .bind(user_id.0)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(job_count, 1);
        assert_eq!(
            notification_id.as_deref(),
            Some(
                if first_won {
                    &first_notice_id
                } else {
                    &second_notice_id
                }
                .as_str()
            )
        );
    });
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
            complete_reset(&db, &token, "new-hash", now)
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
        complete_reset(&db, &expired, "new-hash", now + Duration::hours(2))
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
        complete_reset(&db, &no_identity, "new-hash", now).await,
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
        complete_reset(&db, &token, "new-hash", now).await,
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
        complete_reset(&db, &token, "new-hash", now).await.unwrap(),
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

        let reset_db = db.clone();
        let reset_task = tokio::spawn(async move {
            complete_reset(&reset_db, &token, "new-hash", supplied_at).await
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
fn committed_deletion_wins_against_waiting_admin_transitions() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let accounts = SqlxAccountRepository::new(db.clone());
        let tokens = SqlxTokenStore::new(db.clone());
        let sessions = SqlxSessionStore::new(db.clone());
        let actor_email = UserEmail::parse(&unique_email("delete-race-actor")).unwrap();
        let actor = accounts
            .create(bikesnest_application::NewAccount {
                email: &actor_email,
                display_name: None,
                password_hash: "hash",
                state: AccountState::Active,
                locale: bikesnest_domain::LocaleCode::PtBr,
            })
            .await
            .unwrap();

        for (label, initial_state, operation) in [
            ("delete-race-suspend", AccountState::Active, "suspend"),
            ("delete-race-restore", AccountState::Suspended, "restore"),
        ] {
            let email = UserEmail::parse(&unique_email(label)).unwrap();
            let target = accounts
                .create(bikesnest_application::NewAccount {
                    email: &email,
                    display_name: None,
                    password_hash: "hash",
                    state: AccountState::Active,
                    locale: bikesnest_domain::LocaleCode::PtBr,
                })
                .await
                .unwrap();
            let now = Utc::now();
            let marker = if operation == "suspend" { 201 } else { 204 };
            let reset = VerificationToken::new([marker; 32]);
            assert!(tokens.issue_reset(target, &reset, now).await.unwrap());
            let verification = VerificationToken::new([marker + 3; 32]);
            assert!(
                tokens
                    .issue_verification(
                        target,
                        email.as_str(),
                        &verification,
                        now,
                        AccountState::Active,
                    )
                    .await
                    .unwrap()
            );
            let session = SessionId::new([marker + 1; 32]);
            sessions
                .create(target, &session, &CsrfToken::new([marker + 2; 32]), now)
                .await
                .unwrap();
            if initial_state == AccountState::Suspended {
                accounts.set_state(target, initial_state).await.unwrap();
            }
            let mut deletion = pool.begin().await.unwrap();
            // This narrowly simulates the canonical deletion-state write under
            // the same users-row lock; it is not a privacy-repository test.
            sqlx::query(
                "UPDATE users SET account_state = 'DELETED',
                     suspended_at = TIMESTAMPTZ '2003-04-05 06:07:08+00',
                     deleted_at = TIMESTAMPTZ '2004-05-06 07:08:09+00',
                     updated_at = TIMESTAMPTZ '2005-06-07 08:09:10+00'
                 WHERE id = $1",
            )
            .bind(target.0)
            .execute(&mut *deletion)
            .await
            .unwrap();
            let waiter_repo = SqlxAccountRepository::new(db.clone());
            let waiter = tokio::spawn(async move {
                if operation == "suspend" {
                    waiter_repo.suspend_by_admin(target, actor).await
                } else {
                    waiter_repo.restore_by_admin(target, actor).await
                }
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
            assert!(observed_wait, "{operation} never waited on deletion");
            deletion.commit().await.unwrap();
            let committed_deletion = persisted_account_state(&db, target).await;
            assert!(!waiter.await.unwrap().unwrap());
            assert_eq!(
                persisted_account_state(&db, target).await,
                committed_deletion
            );
            assert_eq!(committed_deletion.0, "DELETED");
            assert_eq!(
                committed_deletion.1,
                Some("2003-04-05T06:07:08Z".parse::<DateTime<Utc>>().unwrap())
            );
            assert_eq!(
                committed_deletion.2,
                Some("2004-05-06T07:08:09Z".parse::<DateTime<Utc>>().unwrap())
            );
            assert_eq!(
                committed_deletion.3,
                "2005-06-07T08:09:10Z".parse::<DateTime<Utc>>().unwrap()
            );
            assert!(sessions.resolve(&session, now).await.unwrap().is_some());
            assert_eq!(unused_verification_tokens(&db, target).await, 1);
            assert_eq!(
                tokens.consume_reset(&reset, now).await.unwrap(),
                Some(target)
            );
            let audits: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM audit_events
                 WHERE target_id = $1 AND action = $2",
            )
            .bind(target.0.to_string())
            .bind(if operation == "suspend" {
                "user.suspended"
            } else {
                "user.restored"
            })
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(audits, 0);
        }
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
async fn audit_insert_round_trip(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let audit = SqlxAuditLog::new(db.clone());
    let email = marker_email("audit");

    let (user_id,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&mut *db.acquire().await.unwrap())
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
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(count, 1);
}

/// `resolve` runs on every authenticated request, so its `last_seen_at` write
/// is throttled to at most once per five minutes. The 30-day idle window
/// is unaffected: the column may lag by five minutes, which is immaterial
/// against 30 days.
#[db_test]
async fn resolve_throttles_the_last_seen_write(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let store = SqlxSessionStore::new(db.clone());
    let email = marker_email("session-throttle");

    let (uid,): (i64,) = sqlx::query_as("INSERT INTO users (email) VALUES ($1) RETURNING id")
        .bind(&email)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let user_id = bikesnest_domain::UserId(uid);

    let raw = SessionId::new([31u8; 32]);
    let csrf = CsrfToken::new([32u8; 32]);
    let now = Utc::now();
    store.create(user_id, &raw, &csrf, now).await.unwrap();

    async fn last_seen(db: &Db, uid: i64) -> chrono::DateTime<Utc> {
        sqlx::query_scalar("SELECT last_seen_at FROM sessions WHERE user_id = $1")
            .bind(uid)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap()
    }

    // Two resolves a minute apart: inside the throttle window, so the column is
    // left exactly as `create` wrote it.
    let before = last_seen(&db, uid).await;
    assert!(store.resolve(&raw, now).await.unwrap().is_some());
    assert!(
        store
            .resolve(&raw, now + Duration::minutes(1))
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        last_seen(&db, uid).await,
        before,
        "last_seen_at must not be rewritten inside the throttle window"
    );

    // Age the row past the throttle: the next resolve does write.
    sqlx::query("UPDATE sessions SET last_seen_at = $2 WHERE user_id = $1")
        .bind(uid)
        .bind(now - Duration::minutes(10))
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let stale = last_seen(&db, uid).await;
    let at = now + Duration::seconds(1);
    let session = store.resolve(&raw, at).await.unwrap().expect("still valid");
    // The row is returned as *read* — the update lands in the same statement,
    // under the same snapshot.
    assert_eq!(session.last_seen_at, stale);
    let refreshed = last_seen(&db, uid).await;
    assert!(
        refreshed > stale,
        "a stale last_seen_at must be refreshed: {refreshed} vs {stale}"
    );
    assert_eq!(refreshed.timestamp(), at.timestamp());
}

// ---------------------------------------------------------------------------
// The admin user list is a searched, bounded page with batched
// counters, instead of "load every account and render it".
// ---------------------------------------------------------------------------

#[db_test]
async fn search_users_matches_email_or_name_and_pages_by_keyset(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let repo = SqlxAccountRepository::new(db.clone());
    let needle = format!("adminneedle{}", std::process::id());
    let mut ids = Vec::new();
    for n in 0..3 {
        let email = format!("{needle}-{n}@bikesnest.test");

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
        .execute(&mut *db.acquire().await.unwrap())
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
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let labels = repo.labels_for(&[ids[1]]).await.unwrap();
    assert!(
        labels[&ids[1]].contains(&needle),
        "a whitespace-only name falls back too: {:?}",
        labels[&ids[1]]
    );

    assert!(repo.labels_for(&[]).await.unwrap().is_empty());
}

#[db_test]
async fn activity_for_reports_last_seen_and_a_contribution_total(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let repo = SqlxAccountRepository::new(db.clone());
    let email = marker_email("admin-activity");

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
    .execute(&mut *db.acquire().await.unwrap())
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
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status) \
         VALUES ($1, $2, 1, 'change_existence', '{\"existence\":\"removed\"}'::jsonb, 'PENDING')",
    )
    .bind(loc)
    .bind(id)
    .execute(&mut *db.acquire().await.unwrap())
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
}

/// `users.locale` round-trips: the registration locale is persisted, every read
/// model carries it, and the language toggle updates it. This column is the
/// only thing a background job can read to know which language to write in.
#[db_test]
async fn account_locale_is_persisted_and_updatable(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    use bikesnest_domain::LocaleCode;

    let repo = SqlxAccountRepository::new(db.clone());
    let email = marker_email("locale");

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
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(stored, "pt-BR");
}
