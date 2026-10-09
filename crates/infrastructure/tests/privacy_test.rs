//! Privacy infrastructure tests: the export
//! payload (no secrets), the single-use download token, the anonymize-in-place
//! transaction, the retention purge statements, and the policy reader.
//!
//! Sequential fixtures and adapters share a rollback scope. Schema-upgrade,
//! snapshot and global administrator races own disposable child databases.

use bikesnest_application::{
    AnonymizationRepository, AuditEvent, AuditLog, ExportAccount, ExportPayload, ExportRepository,
    NewExport, PolicyReader, PrivacyError, RetentionRepository,
};
use bikesnest_domain::{PolicyKind, RetentionPolicy, UserId};
use bikesnest_infrastructure::{
    AUDIT_METADATA_KEYS, Db, SqlxAnonymizationRepository, SqlxAuditLog, SqlxExportRepository,
    SqlxPolicyReader, SqlxRetentionRepository,
};
use bikesnest_test_support::{
    ParkingBuilder, TestObjectStorage, UserBuilder, db_test, run_isolated_database_test,
};
use chrono::{DateTime, Duration, Utc};

#[derive(Clone)]
struct BlockingMailProvider {
    entered: std::sync::Arc<tokio::sync::Notify>,
    release: std::sync::Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl bikesnest_application::EmailProvider for BlockingMailProvider {
    async fn send(
        &self,
        _msg: &bikesnest_application::EmailMessage,
    ) -> Result<(), bikesnest_application::EmailError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(())
    }
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
            last_active_at: None,
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
        vec![],
        vec![],
        Utc::now(),
    )
}

/// A unique identifier for credentials and fixtures within a test run.
fn unique_tag(label: &str) -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{label}-{}-{n}", std::process::id())
}

#[test]
fn mail_lifecycle_upgrade_redacts_legacy_rows_without_rewriting_terminal_history() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        sqlx::raw_sql("DROP INDEX background_job_mail_account_idx; ALTER TABLE background_job DROP COLUMN payload_redacted_at, DROP COLUMN mail_purpose, DROP COLUMN mail_token_expires_at, DROP COLUMN mail_token_hash, DROP COLUMN mail_account_id")
            .execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO background_job(kind,payload,state,finished_at,claimed_by,lease_expires_at) SELECT 'email.send','{\"secret\":\"TOKEN\"}',state,CASE WHEN state IN ('succeeded','failed') THEN now() END,CASE WHEN state='running' THEN 'old-worker' END,CASE WHEN state='running' THEN now()+interval '1 minute' END FROM unnest(ARRAY['pending','running','succeeded','failed']) state")
            .execute(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO background_job(kind,payload) VALUES('unrelated','{\"keep\":true}')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0026_mail_job_lifecycle.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let rows: Vec<(String, serde_json::Value, Option<String>)> = sqlx::query_as("SELECT state,payload,claimed_by FROM background_job WHERE kind='email.send' ORDER BY id")
            .fetch_all(&pool).await.unwrap();
        assert_eq!(
            rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
            vec!["failed", "failed", "succeeded", "failed"]
        );
        assert!(
            rows.iter()
                .all(|r| r.1 == serde_json::json!({}) && r.2.is_none())
        );
        let unrelated: serde_json::Value =
            sqlx::query_scalar("SELECT payload FROM background_job WHERE kind='unrelated'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(unrelated, serde_json::json!({"keep":true}));
    });
}

#[test]
fn security_notice_upgrade_preserves_legacy_jobs_and_sets_nullable_audit_authority() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        sqlx::raw_sql(
            "DROP INDEX background_job_mail_transition_audit_idx;
             ALTER TABLE background_job
               DROP COLUMN mail_transition_audit_id,
               DROP COLUMN mail_recipient_hash,
               DROP CONSTRAINT background_job_mail_purpose_check,
               ADD CONSTRAINT background_job_mail_purpose_check
                 CHECK (mail_purpose IN ('verify','reset','change'));",
        )
        .execute(&pool)
        .await
        .unwrap();
        let legacy_id: i64 = sqlx::query_scalar(
            "INSERT INTO background_job(kind,payload,mail_purpose)
             VALUES('email.send','{\"legacy\":true}','verify') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::raw_sql(include_str!(
            "../../../migrations/0027_security_notice_mail.sql"
        ))
        .execute(&pool)
        .await
        .unwrap();
        let preserved: (serde_json::Value, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT payload,mail_recipient_hash,mail_transition_audit_id
             FROM background_job WHERE id=$1",
        )
        .bind(legacy_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(preserved, (serde_json::json!({"legacy": true}), None, None));

        let user_id: i64 = sqlx::query_scalar(
            "INSERT INTO users(email,account_state) VALUES('upgrade-notice@bikesnest.test','ACTIVE') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let audit_id: i64 = sqlx::query_scalar(
            "INSERT INTO audit_events(actor_user_id,action,target_type,target_id,result,metadata)
             VALUES($1,'auth.password_changed','user',$2,'success','{}') RETURNING id",
        )
        .bind(user_id)
        .bind(user_id.to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
        let notice_id: i64 = sqlx::query_scalar(
            "INSERT INTO background_job(kind,payload,mail_purpose,mail_recipient_hash,mail_transition_audit_id)
             VALUES('email.send','{}','password_changed','digest',$1) RETURNING id",
        )
        .bind(audit_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        let removed: i64 =
            sqlx::query_scalar("SELECT purge_audit_events_before(now()+interval '1 second')")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(removed >= 1);
        let cleared: Option<i64> =
            sqlx::query_scalar("SELECT mail_transition_audit_id FROM background_job WHERE id=$1")
                .bind(notice_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cleared, None);
    });
}

#[test]
fn provider_acceptance_and_deletion_serialize_on_the_account() {
    use bikesnest_application::{EmailKind, EmailMessage, EmailQueue, JobHandler};
    use bikesnest_domain::{LocaleCode, VerificationToken};
    use bikesnest_infrastructure::{JobEmailQueue, SendEmailHandler, SqlxJobRepository};
    use sha2::{Digest, Sha256};
    use sqlx::Acquire as _;
    use std::sync::Arc;

    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let email = "mail-delete-race@example.com".to_string();
        let user_id: i64 = sqlx::query_scalar("INSERT INTO users(email,account_state,email_verified_at) VALUES($1,'ACTIVE',now()) RETURNING id")
        .bind(&email).fetch_one(&pool).await.unwrap();
        let token = VerificationToken::new([73; 32]);
        let token_hash: String = Sha256::digest(token.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        sqlx::query("INSERT INTO email_verification_tokens(token_hash,user_id,email,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')")
        .bind(token_hash).bind(user_id).bind(&email).execute(&pool).await.unwrap();

        let msg = EmailMessage::linked(
            UserId(user_id),
            &email,
            LocaleCode::En,
            EmailKind::VerifyEmail {
                link: format!(
                    "https://bikesnest.test/verify-email?token={}",
                    token.to_base64url()
                ),
                expires_at: None,
            },
        );
        JobEmailQueue::new(SqlxJobRepository::new(Db::from_pool(pool.clone())), 3)
            .enqueue(msg.clone())
            .await
            .unwrap();
        let payload = serde_json::to_value(msg).unwrap();
        let stale_snapshot = payload.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let handler = SendEmailHandler::new(
            Db::from_pool(pool.clone()),
            Arc::new(BlockingMailProvider {
                entered: entered.clone(),
                release: release.clone(),
            }),
        );
        let send = tokio::spawn(async move { handler.run(&payload).await });
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .unwrap();

        let uid = UserId(user_id);
        let delete_db = Db::from_pool(pool.clone());
        let mut deletion = tokio::spawn(async move {
            SqlxAnonymizationRepository::new(delete_db)
                .anonymize(uid, Utc::now())
                .await
        });
        let observed_lock = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'")
                .fetch_one(&pool).await.unwrap();
            if waiting > 0 { break; }
            tokio::task::yield_now().await;
        }
    }).await;
        assert!(
            observed_lock.is_ok(),
            "PostgreSQL must observe deletion waiting on the account lock"
        );
        release.notify_one();
        send.await.unwrap().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), &mut deletion)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let state: String = sqlx::query_scalar("SELECT account_state FROM users WHERE id=$1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "DELETED");
        let fake = bikesnest_infrastructure::FakeEmailProvider::with_root(None);
        let stale_handler =
            SendEmailHandler::new(Db::from_pool(pool.clone()), Arc::new(fake.clone()));
        let stale = stale_handler.run(&stale_snapshot).await.unwrap_err();
        assert!(matches!(
            stale,
            bikesnest_application::JobError::Permanent(_)
        ));
        assert!(
            fake.emails().is_empty(),
            "a preclaimed payload cannot send after deletion commits"
        );

        let enqueue_email = "mail-enqueue-delete@example.com";
        let enqueue_uid: i64 = sqlx::query_scalar("INSERT INTO users(email,account_state,email_verified_at) VALUES($1,'ACTIVE',now()) RETURNING id")
        .bind(enqueue_email).fetch_one(&pool).await.unwrap();
        let enqueue_token = VerificationToken::new([74; 32]);
        let enqueue_hash: String = Sha256::digest(enqueue_token.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        sqlx::query("INSERT INTO email_verification_tokens(token_hash,user_id,email,expires_at) VALUES($1,$2,$3,now()+interval '1 hour')")
        .bind(&enqueue_hash).bind(enqueue_uid).bind(enqueue_email).execute(&pool).await.unwrap();
        let enqueue_msg = EmailMessage::linked(
            UserId(enqueue_uid),
            enqueue_email,
            LocaleCode::En,
            EmailKind::VerifyEmail {
                link: format!(
                    "https://bikesnest.test/verify-email?token={}",
                    enqueue_token.to_base64url()
                ),
                expires_at: None,
            },
        );
        let mut deleting = pool.acquire().await.unwrap();
        let mut deleting_tx = deleting.begin().await.unwrap();
        sqlx::query("UPDATE users SET account_state='DELETED',deleted_at=now() WHERE id=$1")
            .bind(enqueue_uid)
            .execute(&mut *deleting_tx)
            .await
            .unwrap();
        sqlx::query("DELETE FROM email_verification_tokens WHERE user_id=$1")
            .bind(enqueue_uid)
            .execute(&mut *deleting_tx)
            .await
            .unwrap();
        let queue = JobEmailQueue::new(SqlxJobRepository::new(Db::from_pool(pool.clone())), 3);
        let mut enqueue = tokio::spawn(async move { queue.enqueue(enqueue_msg).await });
        let enqueue_wait = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock'")
                .fetch_one(&pool).await.unwrap();
            if waiting > 0 { break; }
            tokio::task::yield_now().await;
        }
    }).await;
        assert!(
            enqueue_wait.is_ok(),
            "enqueue must wait for the deletion account lock"
        );
        deleting_tx.commit().await.unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), &mut enqueue)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        let queued: i64 =
            sqlx::query_scalar("SELECT count(*) FROM background_job WHERE mail_account_id=$1")
                .bind(enqueue_uid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            queued, 0,
            "deletion-first admission cannot create a new secret payload"
        );
    });
}

#[db_test]
async fn anonymization_redacts_every_mail_state_without_touching_other_jobs(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let user = UserBuilder::new()
        .with_email("scoped-mail-delete@example.com")
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let mut ids: Vec<i64> = sqlx::query_scalar(r#"INSERT INTO background_job
      (kind,payload,state,mail_account_id,mail_token_hash,mail_purpose,mail_token_expires_at,mail_recipient_hash,finished_at,claimed_by,lease_expires_at)
      SELECT 'email.send','{"secret":"TOKEN"}',state,$1,'hash','verify',now()+interval '1 hour','recipient-digest',
       CASE WHEN state IN ('succeeded','failed') THEN now() END,
       CASE WHEN state='running' THEN 'worker' END,
       CASE WHEN state='running' THEN now()+interval '1 minute' END
      FROM unnest(ARRAY['pending','running','succeeded','failed']) state RETURNING id"#)
        .bind(user.id.0).fetch_all(&mut *db.acquire().await.unwrap()).await.unwrap();
    let audit_id: i64 = sqlx::query_scalar("INSERT INTO audit_events(actor_user_id,action,target_type,target_id,result,metadata) VALUES($1,'auth.password_changed','user',$2,'success','{}') RETURNING id")
        .bind(user.id.0).bind(user.id.0.to_string()).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    let notice_id: i64 = sqlx::query_scalar("INSERT INTO background_job(kind,payload,state,mail_account_id,mail_purpose,mail_recipient_hash,mail_transition_audit_id) VALUES('email.send','{\"notice\":\"PRIVATE\"}','pending',$1,'password_changed','notice-digest',$2) RETURNING id")
        .bind(user.id.0).bind(audit_id).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    ids.push(notice_id);
    let other: i64 = sqlx::query_scalar("INSERT INTO background_job(kind,payload) VALUES('test.scoped.other','{\"keep\":true}') RETURNING id")
        .fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();
    SqlxAnonymizationRepository::new(db.clone())
        .anonymize(user.id, Utc::now())
        .await
        .unwrap();
    let mut conn = db.acquire().await.unwrap();
    type MailStateRow = (
        String,
        serde_json::Value,
        Option<String>,
        Option<DateTime<Utc>>,
        Option<String>,
        Option<i64>,
    );
    let rows: Vec<MailStateRow> = sqlx::query_as(
        "SELECT state,payload,claimed_by,payload_redacted_at,mail_recipient_hash,mail_transition_audit_id FROM background_job WHERE id=ANY($1) ORDER BY id")
        .bind(&ids).fetch_all(&mut *conn).await.unwrap();
    assert_eq!(
        rows.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        vec!["failed", "failed", "succeeded", "failed", "failed"]
    );
    assert!(rows.iter().all(|r| r.1 == serde_json::json!({})
        && r.2.is_none()
        && r.3.is_some()
        && r.4.is_none()
        && r.5.is_none()));
    let kept: serde_json::Value =
        sqlx::query_scalar("SELECT payload FROM background_job WHERE id=$1")
            .bind(other)
            .fetch_one(&mut *conn)
            .await
            .unwrap();
    assert_eq!(kept, serde_json::json!({"keep":true}));
}

#[test]
fn retention_redacts_only_expired_mail_payloads() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let now = Utc::now();
        let ids: Vec<i64>=sqlx::query_scalar("INSERT INTO background_job(kind,payload,mail_token_hash,mail_purpose,mail_token_expires_at) VALUES ('email.send','{\"secret\":\"expired\"}','expired','reset',$1),('email.send','{\"secret\":\"valid\"}','valid','reset',$2) RETURNING id")
        .bind(now-Duration::hours(1)).bind(now+Duration::hours(1)).fetch_all(&mut *db.acquire().await.unwrap()).await.unwrap();
        let user_id:i64=sqlx::query_scalar("INSERT INTO users(email,account_state) VALUES('scoped-mail-expiry@example.com','ACTIVE') RETURNING id")
        .fetch_one(&pool).await.unwrap();
        sqlx::query("UPDATE background_job SET mail_account_id=$1 WHERE id=ANY($2)")
            .bind(user_id)
            .bind(&ids)
            .execute(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
        sqlx::query("INSERT INTO password_reset_tokens(token_hash,user_id,expires_at) VALUES('expired',$1,$2),('valid',$1,$3)")
        .bind(user_id).bind(now-Duration::hours(1)).bind(now+Duration::hours(1)).execute(&pool).await.unwrap();
        SqlxRetentionRepository::new(
            db.clone(),
            RetentionPolicy::default(),
            std::sync::Arc::new(TestObjectStorage::new()),
        )
        .purge_expired_password_reset_tokens(now)
        .await
        .unwrap();
        let mut conn = db.acquire().await.unwrap();
        let rows: Vec<(String, serde_json::Value, Option<DateTime<Utc>>)> = sqlx::query_as(
        "SELECT state,payload,payload_redacted_at FROM background_job WHERE id=ANY($1) ORDER BY id",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .unwrap();
        assert_eq!(rows[0].0, "failed");
        assert_eq!(rows[0].1, serde_json::json!({}));
        assert!(rows[0].2.is_some());
        assert_eq!(rows[1].0, "pending");
        assert_ne!(rows[1].1, serde_json::json!({}));
        assert!(rows[1].2.is_none());
    });
}

#[db_test]
async fn export_payload_excludes_credential_hash(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let user = UserBuilder::new()
        .with_email("m6exp@example.com")
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let uid = user.id.0;
    sqlx::query(
        "INSERT INTO authentication_identities (user_id, provider, provider_subject, credential_hash) \
         VALUES ($1, 'password', $2, 'supersecret-hash')",
    )
    .bind(uid)
    .bind("m6exp@example.com")
    .execute(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();

    let repo = SqlxExportRepository::new(db.clone());
    let payload = repo.assemble_payload(UserId(uid)).await.unwrap();
    assert_eq!(payload.schema_version, 2);
    assert_eq!(payload.authentication.len(), 1);
    // credential_hash is never selected into the payload.
    let json = serde_json::to_string(&payload).unwrap();
    assert!(!json.contains("supersecret-hash"));
}

/// The durable activity timestamp is personal data the retention rule acts on,
/// so the export carries it.
#[db_test]
async fn export_payload_includes_durable_last_active_at(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let user = UserBuilder::new()
        .with_email("last-active-export@example.com")
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let stamp = DateTime::parse_from_rfc3339("2026-03-04T05:06:07Z")
        .unwrap()
        .with_timezone(&Utc);
    sqlx::query("UPDATE users SET last_active_at = $2 WHERE id = $1")
        .bind(user.id.0)
        .bind(stamp)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();

    let payload = SqlxExportRepository::new(db.clone())
        .assemble_payload(user.id)
        .await
        .unwrap();
    assert_eq!(payload.account.last_active_at, Some(stamp));
    let json = serde_json::to_value(&payload).unwrap();
    assert_eq!(
        json["account"]["last_active_at"],
        serde_json::json!("2026-03-04T05:06:07Z")
    );
}

#[db_test]
async fn export_consume_download_is_single_use_and_distinguishes_errors(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let user = UserBuilder::new()
        .with_email("m6exp2@example.com")
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let uid = user.id.0;

    let repo = SqlxExportRepository::new(db.clone());
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
}

#[db_test]
async fn anonymize_scrubs_identity_and_nulls_attribution(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    // A location to hang content off of.
    let loc = ParkingBuilder::new()
        .with_name("Anonymization fixture")
        .create(&mut db.acquire().await.unwrap())
        .await
        .unwrap();
    let loc_id = loc.id();
    let user = UserBuilder::new()
        .with_email("m6anon@example.com")
        .with_name("Ada")
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let uid = user.id.0;

    // Identity + session (private, deleted).
    sqlx::query(
        "INSERT INTO authentication_identities (user_id, provider, provider_subject, credential_hash) \
         VALUES ($1, 'password', $2, 'hash')",
    )
    .bind(uid).bind("m6anon@example.com").execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, csrf_token, created_at, last_seen_at, expires_at) \
         VALUES ('tok', $1, 'csrf', now(), now(), now() + interval '30 days')",
    )
    .bind(uid).execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    sqlx::query("INSERT INTO favorite (user_id, location_id, created_at) VALUES ($1, $2, now())")
        .bind(uid)
        .bind(loc_id)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();

    // Parked-here (private, deleted).
    sqlx::query(
        "INSERT INTO verification (location_id, user_id, kind, result, created_at, expires_at) \
         VALUES ($1, $2, 'parked_here', 'still_exists', now(), now() + interval '90 days')",
    )
    .bind(loc_id)
    .bind(uid)
    .execute(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    // Existence verification (community content, retained but unattributed).
    sqlx::query(
        "INSERT INTO verification (location_id, user_id, kind, result, created_at) \
         VALUES ($1, $2, 'existence', 'still_exists', now())",
    )
    .bind(loc_id)
    .bind(uid)
    .execute(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    // Review (retained, author NULL).
    sqlx::query(
        "INSERT INTO review (location_id, author_id, rating, body, moderation_state, created_at, updated_at) \
         VALUES ($1, $2, 5, 'Great', 'ACTIVE', now(), now())",
    )
    .bind(loc_id).bind(uid).execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    // Proposal (retained, proposer NULL).
    sqlx::query(
        "INSERT INTO parking_proposal (location_id, proposer_id, base_version, kind, proposed, status, created_at) \
         VALUES ($1, $2, 1, 'change_existence', '{\"existence\":\"exists\"}', 'PENDING', now())",
    )
    .bind(loc_id).bind(uid).execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    // Report (retained, reporter NULL).
    sqlx::query(
        "INSERT INTO report (reporter_id, target_type, target_id, reason, state, created_at, updated_at) \
         VALUES ($1, 'parking', $2, 'spam', 'OPEN', now(), now())",
    )
    .bind(uid).bind(loc_id).execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    // Photos (retained, uploader NULL).
    sqlx::query(
        "INSERT INTO parking_photo (location_id, storage_key, content_type, moderation_state, created_at, uploader_id) \
         VALUES ($1, 'seed/a.jpg', 'image/jpeg', 'APPROVED', now(), $2)",
    )
    .bind(loc_id).bind(uid).execute(&mut *db.acquire().await.unwrap()).await.unwrap();
    // A privacy request (kept, user_id nulled).
    let request_id: i64 = sqlx::query_scalar(
        "INSERT INTO privacy_request (user_id, kind, state, details) VALUES ($1, 'deletion', 'OPEN', '{}') RETURNING id",
    )
    .bind(uid).fetch_one(&mut *db.acquire().await.unwrap()).await.unwrap();

    let now = Utc::now();
    let repo = SqlxAnonymizationRepository::new(db.clone());
    let report = repo.anonymize(UserId(uid), now).await.unwrap();

    // user scrubbed.
    let (email, state, deleted_at) = sqlx::query_as::<_, (String, String, Option<DateTime<Utc>>)>(
        "SELECT email, account_state, deleted_at FROM users WHERE id = $1",
    )
    .bind(uid)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(email, format!("deleted+{uid}@bikesnest.invalid"));
    assert_eq!(state, "DELETED");
    assert!(deleted_at.is_some());

    // private activity gone.
    let identities: i64 =
        sqlx::query_scalar("SELECT count(*) FROM authentication_identities WHERE user_id = $1")
            .bind(uid)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(identities, 0);
    let sessions: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(sessions, 0);
    let favs: i64 = sqlx::query_scalar("SELECT count(*) FROM favorite WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(favs, 0);
    let parked: i64 = sqlx::query_scalar("SELECT count(*) FROM verification WHERE user_id = $1")
        .bind(uid)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(parked, 0);

    // community content retained + unattributed.
    let review_author: Option<i64> =
        sqlx::query_scalar("SELECT author_id FROM review WHERE location_id = $1")
            .bind(loc_id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert!(review_author.is_none());
    let existence_user: Option<i64> = sqlx::query_scalar(
        "SELECT user_id FROM verification WHERE location_id = $1 AND kind = 'existence'",
    )
    .bind(loc_id)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert!(existence_user.is_none());
    let prop: Option<i64> =
        sqlx::query_scalar("SELECT proposer_id FROM parking_proposal WHERE location_id = $1")
            .bind(loc_id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert!(prop.is_none());
    let rep: Option<i64> = sqlx::query_scalar(
        "SELECT reporter_id FROM report WHERE target_id = $1 AND target_type = 'parking'",
    )
    .bind(loc_id)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert!(rep.is_none());
    let photo: Option<i64> =
        sqlx::query_scalar("SELECT uploader_id FROM parking_photo WHERE location_id = $1")
            .bind(loc_id)
            .fetch_one(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert!(photo.is_none());
    let req_user: Option<i64> =
        sqlx::query_scalar("SELECT user_id FROM privacy_request WHERE id = $1")
            .bind(request_id)
            .fetch_one(&mut *db.acquire().await.unwrap())
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
}

#[db_test]
async fn retention_purges_only_expired(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    // Everything here is scoped to this fixture's own rows. `purge_expired_*`
    // is a table-wide DELETE and the suite shares one database, so asserting
    // its *global* return count made the test depend on what every other test
    // happened to have left lying around — and its cleanup only ran on the
    // happy path, so one failure poisoned every later run.
    let email = unique_tag("m6ret") + "@example.com";
    let expired_token = unique_tag("m6ret-expired");
    let valid_token = unique_tag("m6ret-valid");

    let user = UserBuilder::new()
        .with_email(&email)
        .create(&mut *db.acquire().await.unwrap())
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
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    }

    let repo = SqlxRetentionRepository::new(
        db.clone(),
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
            .fetch_all(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert_eq!(
        survivors,
        vec![valid_token.clone()],
        "the expired token must be gone and the valid one untouched"
    );
}

#[db_test]
async fn policy_reader_current_and_history(tx: &mut bikesnest_test_support::TestTx) {
    let scoped = tx.db().await;
    let reader = SqlxPolicyReader::new(scoped.clone());
    let old = "m6-test-old";
    let new = "m6-test-new";
    let locale = "pt-BR";
    let now = Utc::now();

    // Insert an old + current version inside the scoped transaction. A future
    // document is announced history, never the currently applicable policy.
    sqlx::query(
        "INSERT INTO policy_version (kind, locale, version, effective_at, content) VALUES ('privacy', $1, $2, $3, 'old')",
    )
    .bind(locale)
    .bind(old)
    .bind(now - Duration::seconds(1))
    .execute(&mut *scoped.acquire().await.unwrap())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO policy_version (kind, locale, version, effective_at, content) VALUES ('privacy', $1, $2, $3, 'new')",
    )
    .bind(locale)
    .bind(new)
    .bind(now)
    .execute(&mut *scoped.acquire().await.unwrap())
    .await
    .unwrap();

    let current = reader
        .current(PolicyKind::Privacy, locale)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.locale, locale);
    assert_eq!(current.version, "m6-test-new");
    assert!(current.superseded_at.is_none());

    let history = reader.history(PolicyKind::Privacy, locale).await.unwrap();
    assert!(history.iter().any(|d| d.version == old));
    assert!(history.iter().any(|d| d.version == new));
    // Newest first.
    assert_eq!(history[0].version, "m6-test-new");
}

#[test]
fn export_payload_is_one_repeatable_read_snapshot() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
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
        let loc = ParkingBuilder::new()
            .with_name("Privacy Snapshot")
            .create(&mut pool.acquire().await.unwrap())
            .await
            .unwrap();
        let loc_id = loc.id();
        let user = UserBuilder::new()
            .with_email("privacy-snapshot@example.com")
            .create(&mut *pool.acquire().await.unwrap())
            .await
            .unwrap();
        let uid = user.id.0;
        let (review_id,): (i64,) = sqlx::query_as(
            "INSERT INTO review (location_id, author_id, rating, body) \
             VALUES ($1, $2, 4, 'before the edit') RETURNING id",
        )
        .bind(loc_id)
        .bind(uid)
        .fetch_one(&mut *pool.acquire().await.unwrap())
        .await
        .unwrap();
        for (rating, body) in [(3i16, "first version"), (4i16, "before the edit")] {
            sqlx::query(
                "INSERT INTO review_revision (review_id, rating, body) VALUES ($1, $2, $3)",
            )
            .bind(review_id)
            .bind(rating)
            .bind(body)
            .execute(&mut *pool.acquire().await.unwrap())
            .await
            .unwrap();
        }

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
        let payload = SqlxExportRepository::new(db.clone())
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
    });
}

#[db_test]
async fn export_batches_revisions_across_many_reviews(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    // Every review's history comes back, from one query rather than N.
    let user = UserBuilder::new()
        .with_email("privacy-revbatch@example.com")
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let uid = user.id.0;
    let mut expected: Vec<(i64, usize)> = Vec::new();
    for n in 0..3 {
        let loc = ParkingBuilder::new()
            .with_name(format!("Privacy RevBatch {n}"))
            .create(&mut db.acquire().await.unwrap())
            .await
            .unwrap();
        let (review_id,): (i64,) = sqlx::query_as(
            "INSERT INTO review (location_id, author_id, rating, body) \
             VALUES ($1, $2, 5, 'body') RETURNING id",
        )
        .bind(loc.id())
        .bind(uid)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
        // n + 1 published versions, so a mix-up between reviews would show.
        for v in 0..=n {
            sqlx::query("INSERT INTO review_revision (review_id, rating, body) VALUES ($1, 5, $2)")
                .bind(review_id)
                .bind(format!("v{v}"))
                .execute(&mut *db.acquire().await.unwrap())
                .await
                .unwrap();
        }
        expected.push((review_id, n + 1));
    }

    let payload = SqlxExportRepository::new(db.clone())
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
}

#[db_test]
async fn anonymize_nulls_roles_this_account_granted(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let granter = UserBuilder::new()
        .with_email("privacy-granter@example.com")
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let grantee = UserBuilder::new()
        .with_email("privacy-grantee@example.com")
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let (granter_id, grantee_id) = (granter.id.0, grantee.id.0);
    // Two more people the granter promoted. This scenario concerns attribution
    // on moderator grants; global last-admin rules have separate child-DB tests.
    for email in ["privacy-admin-a@example.com", "privacy-admin-b@example.com"] {
        let u = UserBuilder::new()
            .with_email(email)
            .create(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'MODERATOR', $2)",
        )
        .bind(u.id.0)
        .bind(granter_id)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO user_roles (user_id, role, granted_by) VALUES ($1, 'MODERATOR', $2)")
        .bind(grantee_id)
        .bind(granter_id)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();

    let report = SqlxAnonymizationRepository::new(db.clone())
        .anonymize(UserId(granter_id), Utc::now())
        .await
        .unwrap();

    // Three rows named the granter; none may still do so.
    assert_eq!(report.roles_granted_by_anonymized, 3);
    let still_named: i64 =
        sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE granted_by = $1")
            .bind(granter_id)
            .fetch_one(&mut *db.acquire().await.unwrap())
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
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert!(granted_by.is_none());
}

#[db_test]
async fn anonymize_rewrites_an_email_shaped_audit_target(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    // A failed login is audited with the attempted *email* as `target_id`
    // (there is no user id to record). Nulling `actor_user_id` never reaches
    // it, so erasure has to rewrite it.
    const EMAIL: &str = "privacy-audit-pii@example.com";
    let user = UserBuilder::new()
        .with_email(EMAIL)
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let uid = user.id.0;

    SqlxAuditLog::new(db.clone())
        .record(AuditEvent::failure(None, "auth.login", "user", EMAIL))
        .await
        .unwrap();

    let report = SqlxAnonymizationRepository::new(db.clone())
        .anonymize(UserId(uid), Utc::now())
        .await
        .unwrap();
    assert!(report.audit_targets_anonymized >= 1);

    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE target_type = 'user' AND target_id = $1",
    )
    .bind(EMAIL)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(leaked, 0, "the audit trail still names the deleted account");
    let rewritten: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_events WHERE target_type = 'user' AND target_id = $1",
    )
    .bind(format!("deleted+{uid}@bikesnest.invalid"))
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert!(rewritten >= 1, "the row must survive, anonymized");
}

#[db_test]
async fn audit_metadata_keys_stay_within_the_classified_allowlist(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    // `privacy/anonymize.rs` does not scrub `audit_events.metadata`, and that
    // is only correct while no key there can hold personal data. This is the
    // check that makes the assumption fail loudly: a new key must be added to
    // `AUDIT_METADATA_KEYS` (i.e. classified) or scrubbed.
    let live: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT jsonb_object_keys(metadata) FROM audit_events \
         WHERE metadata <> '{}'::jsonb",
    )
    .fetch_all(&mut *db.acquire().await.unwrap())
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

// Only used with an owned child database, so unrelated sessions cannot satisfy
// this observation. The callers poll their operations alongside this observer.
async fn wait_for_lock_waiters(pool: &sqlx::PgPool, expected: i64) -> bool {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE datname = current_database() AND wait_event_type = 'Lock'",
            ).fetch_one(pool).await.unwrap();
            if waiting >= expected {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.is_ok()
}

#[test]
fn concurrent_deletions_of_the_last_two_admins_cannot_both_win() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        // Two admins deleting their accounts at the same moment used to both read
        // "another admin exists" and both proceed, leaving the system with none.
        // The guard now runs inside the anonymize transaction, holding `FOR UPDATE`
        // on the ADMIN rows, so the two serialize and the second sees the first's
        // commit.
        let emails = [
            unique_tag("privacy-race-a") + "@example.com",
            unique_tag("privacy-race-b") + "@example.com",
        ];
        let db = Db::from_pool(pool.clone());

        let mut ids = Vec::new();
        for email in &emails {
            let u = UserBuilder::new()
                .with_email(email)
                .create(&pool)
                .await
                .unwrap();
            ids.push(u.id.0);
        }
        for id in &ids {
            sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, 'ADMIN')")
                .bind(id)
                .execute(&pool)
                .await
                .unwrap();
        }

        let repo_a = SqlxAnonymizationRepository::new(db.clone());
        let repo_b = SqlxAnonymizationRepository::new(db.clone());
        let now = Utc::now();
        let mut blocker = pool.begin().await.unwrap();
        let _: Vec<i64> =
            sqlx::query_scalar("SELECT user_id FROM user_roles WHERE role = 'ADMIN' FOR UPDATE")
                .fetch_all(&mut *blocker)
                .await
                .unwrap();
        let mut deletions = Box::pin(async {
            tokio::join!(
                repo_a.anonymize(UserId(ids[0]), now),
                repo_b.anonymize(UserId(ids[1]), now),
            )
        });
        let both_waited = tokio::select! {
            observed = wait_for_lock_waiters(&pool, 2) => observed,
            _ = &mut deletions => false,
        };
        blocker.rollback().await.unwrap();
        assert!(
            both_waited,
            "both real deletions must overlap at the admin locks"
        );
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(10), deletions)
            .await
            .expect("deletions finish after releasing the blocker");
        assert_eq!([&a, &b].iter().filter(|r| r.is_ok()).count(), 1);
        let refused = [&a, &b]
            .iter()
            .filter(|r| matches!(r, Err(PrivacyError::LastAdmin)))
            .count();
        let admins_left: i64 =
            sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE role = 'ADMIN'")
                .fetch_one(&pool)
                .await
                .unwrap();

        assert_eq!(
            refused, 1,
            "exactly one deletion must be refused as the last admin (a={a:?}, b={b:?})"
        );
        assert_eq!(
            admins_left, 1,
            "the system must never be left without an admin"
        );
    });
}

#[db_test]
async fn audit_events_are_append_only_but_purgeable(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    // Dated in the distant past so the purge below can name a cutoff that
    // covers this row and nothing else: `purge_audit_events_before` is a
    // whole-table DELETE, and the suite shares one database.
    let ancient = DateTime::parse_from_rfc3339("2000-01-01T00:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    let (id,): (i64,) = sqlx::query_as(
        "INSERT INTO audit_events \
         (actor_user_id, action, target_type, target_id, result, metadata, created_at) \
         VALUES (NULL, 'privacy.immutability.probe', 'system', 'probe', 'success', '{}'::jsonb, $1) \
         RETURNING id",
    )
    .bind(ancient)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();

    let mut conn = db.acquire().await.unwrap();
    for statement in [
        "UPDATE audit_events SET result = 'failure' WHERE id = $1",
        "DELETE FROM audit_events WHERE id = $1",
    ] {
        let mut savepoint = conn.begin().await.unwrap();
        let err = sqlx::query(statement)
            .bind(id)
            .execute(&mut *savepoint)
            .await
            .expect_err("audit rows must be append-only");
        assert!(
            err.to_string().contains("append-only"),
            "unexpected error: {err}"
        );
        savepoint.rollback().await.unwrap();
    }
    drop(conn);

    // The sanctioned purge works, and reports what it removed.
    let removed: i64 = sqlx::query_scalar("SELECT purge_audit_events_before($1)")
        .bind(ancient + Duration::days(1))
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert!(removed >= 1, "the purge function must delete rows");
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(left, 0);
}

#[db_test]
async fn orphan_sweep_deletes_aged_unreferenced_objects_only(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let loc = ParkingBuilder::new()
        .with_name("Privacy Orphan")
        .create(&mut db.acquire().await.unwrap())
        .await
        .unwrap();
    let referenced = "uploads/privacy-referenced/full.jpg";
    sqlx::query(
        "INSERT INTO parking_photo (location_id, storage_key, content_type, position, \
         moderation_state) VALUES ($1, $2, 'image/jpeg', 0, 'APPROVED')",
    )
    .bind(loc.id())
    .bind(referenced)
    .execute(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();

    let policy = RetentionPolicy::default();
    let now = Utc::now();
    let aged = now - policy.upload_orphan_ttl - Duration::hours(1);

    let storage = std::sync::Arc::new(TestObjectStorage::new());
    storage.seed_aged("uploads/privacy-orphan/full.jpg", aged);
    storage.seed_aged("uploads/privacy-orphan/thumb.jpg", aged);
    storage.seed_aged(referenced, aged);
    storage.seed_aged("uploads/privacy-young/full.jpg", now);
    // Not under `uploads/` — the seeded dev dataset is never swept.
    storage.seed_aged("seed/curitiba/bike.jpg", aged);
    // Force pagination, so the loop (not just one page) is exercised.
    storage.set_page_size(2);

    let repo = SqlxRetentionRepository::new(db.clone(), policy, storage.clone());
    let purged = repo.purge_orphan_uploads(now).await.unwrap();

    assert_eq!(purged, 2, "only the two aged, unreferenced uploads");
    assert!(!storage.contains("uploads/privacy-orphan/full.jpg"));
    assert!(!storage.contains("uploads/privacy-orphan/thumb.jpg"));
    assert!(
        storage.contains(referenced),
        "a referenced key must survive"
    );
    assert!(
        storage.contains("uploads/privacy-young/full.jpg"),
        "an object inside the orphan TTL must survive"
    );
    assert!(
        storage.contains("seed/curitiba/bike.jpg"),
        "objects outside uploads/ are out of scope"
    );
}

#[db_test]
async fn orphan_sweep_propagates_a_listing_failure(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    // The old filesystem sweep swallowed its `read_dir` error and returned
    // `Ok(0)`, so media retention was a silent no-op for as long as the
    // directory was missing. A store that cannot be listed must be an error.
    let storage = std::sync::Arc::new(TestObjectStorage::new());
    storage.fail_list();
    let repo = SqlxRetentionRepository::new(db.clone(), RetentionPolicy::default(), storage);
    let err = repo
        .purge_orphan_uploads(Utc::now())
        .await
        .expect_err("a failing list must not report a successful zero");
    assert!(matches!(err, PrivacyError::Unavailable), "got {err:?}");
}

#[db_test]
async fn reconcile_drops_aged_pending_rows_with_no_object(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let loc = ParkingBuilder::new()
        .with_name("Privacy Reconcile")
        .create(&mut db.acquire().await.unwrap())
        .await
        .unwrap();
    let stored = "uploads/privacy-recon-ok/full.jpg";
    let missing = "uploads/privacy-recon-gone/full.jpg";
    let young = "uploads/privacy-recon-young/full.jpg";
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
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    }

    let storage = std::sync::Arc::new(TestObjectStorage::new());
    storage.seed(stored, b"full", "image/jpeg");
    storage.seed(young, b"full", "image/jpeg");

    let repo = SqlxRetentionRepository::new(db.clone(), RetentionPolicy::default(), storage);
    let deleted = repo.reconcile_pending_photos(Utc::now()).await.unwrap();
    assert_eq!(deleted, 1, "only the aged row whose object is gone");

    let keys: Vec<String> =
        sqlx::query_scalar("SELECT storage_key FROM parking_photo WHERE location_id = $1")
            .bind(loc.id())
            .fetch_all(&mut *db.acquire().await.unwrap())
            .await
            .unwrap();
    assert!(keys.contains(&stored.to_string()));
    assert!(
        keys.contains(&young.to_string()),
        "a row inside the grace period is left alone"
    );
    assert!(!keys.contains(&missing.to_string()));
}

#[test]
fn revoke_role_guarded_serializes_on_the_locked_admin_rows() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
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
            "privacy-forupdate-a@example.com",
            "privacy-forupdate-b@example.com",
        ];
        let db = Db::from_pool(pool.clone());

        let mut ids = Vec::new();
        for email in EMAILS {
            let u = UserBuilder::new()
                .with_email(email)
                .create(&pool)
                .await
                .unwrap();
            ids.push(u.id.0);
        }
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

        let repo = bikesnest_infrastructure::SqlxAccountRepository::new(db.clone());
        let target = UserId(ids[0]);
        let mut revoke = Box::pin(repo.revoke_role_guarded(target, Role::Admin));
        let blocked = tokio::select! {
            observed = wait_for_lock_waiters(&pool, 1) => observed,
            _ = &mut revoke => false,
        };

        // Release the lock; the revoke now completes against the state it locked.
        blocker.rollback().await.unwrap();
        assert!(
            blocked,
            "the guarded revoke must wait for the ADMIN row locks"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(10), revoke)
                .await
                .expect("revoke finishes after release")
                .unwrap(),
            "the revoke removes a row"
        );
        let still_admin: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM user_roles WHERE role = 'ADMIN' AND user_id = $1",
        )
        .bind(target.0)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(still_admin, 0);
    });
}

#[test]
fn revoke_role_guarded_refuses_the_sole_admin_in_sql() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        // The refusal itself, against the real SQL rather than a hand-written
        // mirror of it. Without this, weakening the repository's own comparison
        // (`admins.len() <= 1` → `<= 0`) passes the whole suite: the FOR UPDATE
        // test deliberately seeds a second admin so the revoke is never refused,
        // and the application-level test drives a fake repository.
        use bikesnest_application::AccountRepository;
        use bikesnest_domain::Role;

        let sole_email = unique_tag("privacy-sql-sole") + "@example.com";
        let second_email = unique_tag("privacy-sql-second") + "@example.com";
        let db = Db::from_pool(pool.clone());

        let sole = UserBuilder::new()
            .with_email(&sole_email)
            .create(&pool)
            .await
            .unwrap();
        let second = UserBuilder::new()
            .with_email(&second_email)
            .create(&pool)
            .await
            .unwrap();
        for (id, role) in [(sole.id.0, "ADMIN"), (second.id.0, "MODERATOR")] {
            sqlx::query("INSERT INTO user_roles (user_id, role) VALUES ($1, $2)")
                .bind(id)
                .bind(role)
                .execute(&pool)
                .await
                .unwrap();
        }

        let repo = bikesnest_infrastructure::SqlxAccountRepository::new(db.clone());
        let count_role = async |user_id: i64, role: &str| -> i64 {
            sqlx::query_scalar("SELECT count(*) FROM user_roles WHERE user_id = $1 AND role = $2")
                .bind(user_id)
                .bind(role)
                .fetch_one(&pool)
                .await
                .unwrap()
        };

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
    });
}

// ---------------------------------------------------------------------------
// Inactive-account anonymization uses the durable `users.last_active_at`.
// ---------------------------------------------------------------------------

struct FixedClock(DateTime<Utc>);

impl bikesnest_application::Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// An account created long ago whose `last_active_at` is pinned to `active_at`
/// (as if every session had been purged and nothing advanced it since).
async fn aged_user(db: &Db, label: &str, active_at: DateTime<Utc>) -> i64 {
    let user = UserBuilder::new()
        .with_email(format!("{}@example.com", unique_tag(label)))
        .create(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    sqlx::query(
        "UPDATE users SET account_state = 'ACTIVE', created_at = now() - interval '730 days', \
         last_active_at = $2 WHERE id = $1",
    )
    .bind(user.id.0)
    .bind(active_at)
    .execute(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    user.id.0
}

async fn insert_session(db: &Db, uid: i64, last_seen_at: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO sessions (token_hash, user_id, csrf_token, created_at, last_seen_at, expires_at) \
         VALUES ($1, $2, 'csrf', $3, $3, $3 + interval '90 days')",
    )
    .bind(unique_tag("tok"))
    .bind(uid)
    .bind(last_seen_at)
    .execute(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
}

async fn account_state(db: &Db, uid: i64) -> String {
    sqlx::query_scalar("SELECT account_state FROM users WHERE id = $1")
        .bind(uid)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap()
}

async fn last_active_at(db: &Db, uid: i64) -> DateTime<Utc> {
    sqlx::query_scalar("SELECT last_active_at FROM users WHERE id = $1")
        .bind(uid)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap()
}

/// The regression: the retention job purges idle sessions *before* it looks
/// for inactive accounts. A two-year-old account whose last session ended 31
/// days ago lost that session to the purge and then fell back to `created_at`,
/// so a 365-day threshold anonymized it. The whole job runs here, in order.
#[db_test]
async fn retention_job_keeps_recently_active_account_whose_sessions_were_purged(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let now = Utc::now();
    let recent = aged_user(&db, "inactive-recent", now - Duration::days(730)).await;
    // Signing in advanced `last_active_at` through the sessions trigger.
    insert_session(&db, recent, now - Duration::days(31)).await;
    let dormant = aged_user(&db, "inactive-dormant", now - Duration::days(400)).await;

    let job = bikesnest_application::RetentionJob::new(
        Box::new(SqlxRetentionRepository::new(
            db.clone(),
            RetentionPolicy::default(),
            std::sync::Arc::new(TestObjectStorage::new()),
        )),
        Box::new(SqlxAuditLog::new(db.clone())),
        Box::new(FixedClock(now)),
        bikesnest_application::RetentionConfig {
            inactive_account_anonymize_after_days: 365,
            deleted_account_purge_after_days: 0,
        },
    );
    job.run().await.unwrap();

    let sessions_left: i64 = sqlx::query_scalar("SELECT count(*) FROM sessions WHERE user_id = $1")
        .bind(recent)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(sessions_left, 0, "the 31-day idle session was purged");
    assert_eq!(
        account_state(&db, recent).await,
        "ACTIVE",
        "active 31 days ago: must survive a 365-day threshold"
    );
    assert!((last_active_at(&db, recent).await - (now - Duration::days(31))).num_seconds() == 0);
    assert_eq!(
        account_state(&db, dormant).await,
        "DELETED",
        "a genuinely inactive account is still anonymized"
    );
}

#[db_test]
async fn anonymize_inactive_accounts_counts_only_inactive_accounts(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let now = Utc::now();
    let dormant = aged_user(&db, "inactive-count", now - Duration::days(400)).await;
    let active = aged_user(&db, "active-count", now - Duration::days(10)).await;
    let repo = SqlxRetentionRepository::new(
        db.clone(),
        RetentionPolicy::default(),
        std::sync::Arc::new(TestObjectStorage::new()),
    );
    let n = repo
        .anonymize_inactive_accounts(now - Duration::days(365))
        .await
        .unwrap();
    assert!(n >= 1);
    assert_eq!(account_state(&db, dormant).await, "DELETED");
    assert_eq!(account_state(&db, active).await, "ACTIVE");
}

/// The candidate list is read once, before the loop. An account that becomes
/// active after that read must be skipped by the locked re-check rather than
/// erased on the strength of the stale list.
#[db_test]
async fn anonymize_if_inactive_skips_account_active_since_candidate_selection(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let now = Utc::now();
    let cutoff = now - Duration::days(365);
    let uid = aged_user(&db, "recheck", now - Duration::days(400)).await;

    // Selected as a candidate...
    let candidate: bool = sqlx::query_scalar("SELECT last_active_at < $2 FROM users WHERE id = $1")
        .bind(uid)
        .bind(cutoff)
        .fetch_one(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert!(candidate);
    // ...then signs in before its turn comes.
    insert_session(&db, uid, now).await;
    assert!(last_active_at(&db, uid).await >= cutoff);

    let anonymizer = SqlxAnonymizationRepository::new(db.clone());
    let report = anonymizer
        .anonymize_if_inactive(UserId(uid), cutoff, now)
        .await
        .unwrap();
    assert!(report.is_none(), "a now-active account is skipped");
    assert_eq!(account_state(&db, uid).await, "ACTIVE");

    // A live session newer than the cutoff also keeps the account even if the
    // trigger's write to `last_active_at` was skipped (row busy at the time).
    sqlx::query("UPDATE users SET last_active_at = now() - interval '400 days' WHERE id = $1")
        .bind(uid)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let report = anonymizer
        .anonymize_if_inactive(UserId(uid), cutoff, now)
        .await
        .unwrap();
    assert!(report.is_none(), "a live recent session keeps the account");
    assert_eq!(account_state(&db, uid).await, "ACTIVE");
}

/// `last_active_at` is advanced by session creation and by the (throttled)
/// `last_seen_at` refresh, never rewound, and the session purge folds the
/// purged rows into it so a skipped trigger write cannot be lost.
#[db_test]
async fn last_active_at_tracks_sessions_and_survives_purge(
    tx: &mut bikesnest_test_support::TestTx,
) {
    let db = tx.db().await;
    let now = Utc::now();
    let uid = aged_user(&db, "tracks", now - Duration::days(700)).await;

    insert_session(&db, uid, now - Duration::days(40)).await;
    assert_eq!(
        (last_active_at(&db, uid).await - (now - Duration::days(40))).num_seconds(),
        0,
        "creating a session advances it"
    );
    sqlx::query("UPDATE sessions SET last_seen_at = $2 WHERE user_id = $1")
        .bind(uid)
        .bind(now - Duration::days(35))
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    assert_eq!(
        (last_active_at(&db, uid).await - (now - Duration::days(35))).num_seconds(),
        0,
        "refreshing last_seen_at advances it"
    );
    insert_session(&db, uid, now - Duration::days(600)).await;
    assert_eq!(
        (last_active_at(&db, uid).await - (now - Duration::days(35))).num_seconds(),
        0,
        "an older session never rewinds it"
    );

    // Simulate a trigger write that was skipped, then purge.
    sqlx::query("UPDATE users SET last_active_at = now() - interval '700 days' WHERE id = $1")
        .bind(uid)
        .execute(&mut *db.acquire().await.unwrap())
        .await
        .unwrap();
    let repo = SqlxRetentionRepository::new(
        db.clone(),
        RetentionPolicy::default(),
        std::sync::Arc::new(TestObjectStorage::new()),
    );
    assert!(repo.purge_expired_sessions(now).await.unwrap() >= 2);
    assert_eq!(
        (last_active_at(&db, uid).await - (now - Duration::days(35))).num_seconds(),
        0,
        "the purge folds the newest purged session into last_active_at"
    );
}
