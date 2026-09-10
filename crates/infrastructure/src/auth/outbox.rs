//! Atomic authentication transitions into the account-linked mail outbox.

use crate::Db;
use crate::auth::hash::sha256_hex;
use crate::email::idempotency_key;
use crate::job::repo::{MailEnqueue, enqueue_mail_on};
use async_trait::async_trait;
use bikesnest_application::{AdmittedAuthMail, AuthError, AuthOutbox, EmailMessage, NewAccount};
use bikesnest_domain::{AccountState, UserId, VerificationToken};
use chrono::{DateTime, Duration, Utc};
use sqlx::{PgConnection, Postgres, Transaction};

pub struct SqlxAuthOutbox {
    db: Db,
    max_attempts: i32,
}

impl SqlxAuthOutbox {
    pub fn new(db: Db, max_attempts: i32) -> Self {
        Self { db, max_attempts }
    }
}

async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    message: &EmailMessage,
    token_hash: &str,
    _expires_at: DateTime<Utc>,
    max_attempts: i32,
) -> Result<i64, crate::job::JobRepoError> {
    let payload = serde_json::to_value(message)
        .map_err(|error| crate::job::JobRepoError::Db(sqlx::Error::Encode(Box::new(error))))?;
    let key = idempotency_key(message);
    enqueue_mail_on(
        &mut *tx,
        MailEnqueue {
            payload: &payload,
            account_id: message.account_id,
            token_hash,
            purpose: message.kind.code(),
            recipient: &message.to,
            run_at: Utc::now(),
            max_attempts,
            idempotency_key: &key,
        },
    )
    .await?
    .ok_or(crate::job::JobRepoError::InvalidMailCredential)
}

async fn audit(
    tx: &mut Transaction<'_, Postgres>,
    user_id: i64,
    action: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO audit_events(actor_user_id,action,target_type,target_id,result,metadata) VALUES($1,$2,'user',$3,'success','{}')")
        .bind(user_id).bind(action).bind(user_id.to_string()).execute(&mut **tx).await?;
    Ok(())
}

async fn recover_registration(
    conn: &mut PgConnection,
    user_id: i64,
) -> Result<Option<AdmittedAuthMail>, AuthError> {
    let row: Option<(i64, serde_json::Value)> = sqlx::query_as(
        r#"SELECT j.id,j.payload FROM background_job j
        JOIN email_verification_tokens t ON t.user_id=j.mail_account_id AND t.token_hash=j.mail_token_hash
        WHERE j.kind='email.send' AND j.mail_account_id=$1 AND j.mail_purpose='verify'
          AND j.payload <> '{}'::jsonb AND t.used_at IS NULL AND t.expires_at>clock_timestamp()
          AND ((j.state='running' AND j.lease_expires_at>clock_timestamp())
               OR (j.state IN ('pending','running') AND j.attempts<j.max_attempts))
        ORDER BY j.id DESC LIMIT 1"#,
    ).bind(user_id).fetch_optional(conn).await.map_err(|error| db_err("auth_outbox.recover", error))?;
    row.map(|(job_id, payload)| {
        serde_json::from_value(payload)
            .map(|message| AdmittedAuthMail { job_id, message })
            .map_err(|_| AuthError::Internal)
    })
    .transpose()
}

fn db_err(context: &'static str, error: sqlx::Error) -> AuthError {
    crate::db_error::classify_and_log(context, error).into()
}

fn admission_err(context: &'static str, error: crate::job::JobRepoError) -> AuthError {
    match error {
        crate::job::JobRepoError::Db(error) => db_err(context, error),
        _ => AuthError::Internal,
    }
}

#[async_trait]
impl AuthOutbox for SqlxAuthOutbox {
    async fn register(
        &self,
        new: NewAccount<'_>,
        token: &VerificationToken,
        at: DateTime<Utc>,
        mut message: EmailMessage,
    ) -> Result<Option<AdmittedAuthMail>, AuthError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("auth_outbox.register", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("auth_outbox.register", e))?;
        let inserted: Option<i64> = sqlx::query_scalar(
            "INSERT INTO users(email,display_name,account_state,locale,updated_at) VALUES($1,$2,$3,$4,now()) ON CONFLICT(lower(email)) DO NOTHING RETURNING id",
        ).bind(new.email.as_str()).bind(new.display_name).bind(new.state.as_code()).bind(new.locale.as_str())
            .fetch_optional(&mut *tx).await.map_err(|e| db_err("auth_outbox.register", e))?;
        let Some(user_id) = inserted else {
            let existing: Option<(i64, String, String)> = sqlx::query_as(
                "SELECT id,account_state::text,locale FROM users WHERE email=$1 FOR UPDATE",
            )
            .bind(new.email.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|e| db_err("auth_outbox.register", e))?;
            let Some((user_id, state, locale)) = existing else {
                return Err(AuthError::Conflict);
            };
            if state != "PENDING_EMAIL_VERIFICATION" {
                tx.rollback()
                    .await
                    .map_err(|e| db_err("auth_outbox.register", e))?;
                return Ok(None);
            }
            if let Some(mail) = recover_registration(&mut tx, user_id).await? {
                tx.commit()
                    .await
                    .map_err(|e| db_err("auth_outbox.register", e))?;
                return Ok(Some(mail));
            }
            sqlx::query(
                r#"UPDATE background_job SET state='failed',payload='{}',
                   payload_redacted_at=COALESCE(payload_redacted_at,clock_timestamp()),
                   finished_at=COALESCE(finished_at,clock_timestamp()),claimed_by=NULL,
                   lease_expires_at=NULL,heartbeat_at=NULL,
                   last_error='mail delivery attempt budget exhausted'
                   WHERE kind='email.send' AND mail_account_id=$1 AND mail_purpose='verify'
                     AND state IN ('pending','running') AND attempts>=max_attempts
                     AND (state='pending' OR lease_expires_at<=clock_timestamp())"#,
            )
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("auth_outbox.register", e))?;
            message.account_id = user_id;
            message.locale =
                bikesnest_domain::LocaleCode::parse(&locale).ok_or(AuthError::Internal)?;
            let hash = sha256_hex(token.as_bytes());
            let expires = at + Duration::hours(24);
            sqlx::query("INSERT INTO email_verification_tokens(token_hash,user_id,email,expires_at) VALUES($1,$2,$3,$4)")
                .bind(&hash).bind(user_id).bind(new.email.as_str()).bind(expires).execute(&mut *tx).await.map_err(|e| db_err("auth_outbox.register", e))?;
            let job_id = enqueue(&mut tx, &message, &hash, expires, self.max_attempts)
                .await
                .map_err(|e| admission_err("auth_outbox.register", e))?;
            tx.commit()
                .await
                .map_err(|e| db_err("auth_outbox.register", e))?;
            return Ok(Some(AdmittedAuthMail { job_id, message }));
        };
        message.account_id = user_id;
        sqlx::query("INSERT INTO authentication_identities(user_id,provider,provider_subject,credential_hash) VALUES($1,'password',$2,$3)")
            .bind(user_id).bind(new.email.as_str()).bind(new.password_hash).execute(&mut *tx).await.map_err(|e| db_err("auth_outbox.register", e))?;
        sqlx::query("INSERT INTO user_roles(user_id,role,granted_by) VALUES($1,'USER',NULL)")
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("auth_outbox.register", e))?;
        let hash = sha256_hex(token.as_bytes());
        let expires = at + Duration::hours(24);
        sqlx::query("INSERT INTO email_verification_tokens(token_hash,user_id,email,expires_at) VALUES($1,$2,$3,$4)").bind(&hash).bind(user_id).bind(new.email.as_str()).bind(expires).execute(&mut *tx).await.map_err(|e| db_err("auth_outbox.register", e))?;
        let job_id = enqueue(&mut tx, &message, &hash, expires, self.max_attempts)
            .await
            .map_err(|e| admission_err("auth_outbox.register", e))?;
        audit(&mut tx, user_id, "auth.register")
            .await
            .map_err(|e| db_err("auth_outbox.register", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("auth_outbox.register", e))?;
        Ok(Some(AdmittedAuthMail { job_id, message }))
    }

    async fn issue_verification(
        &self,
        user_id: UserId,
        email: &str,
        token: &VerificationToken,
        at: DateTime<Utc>,
        expected_state: AccountState,
        message: EmailMessage,
        audit_action: Option<&'static str>,
    ) -> Result<Option<AdmittedAuthMail>, AuthError> {
        if !matches!(
            expected_state,
            AccountState::PendingEmailVerification | AccountState::Active
        ) {
            return Ok(None);
        }
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("auth_outbox.verify", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("auth_outbox.verify", e))?;
        let eligible = sqlx::query_scalar::<_, bool>(
            "SELECT account_state=$2 FROM users WHERE id=$1 FOR UPDATE",
        )
        .bind(user_id.0)
        .bind(expected_state.as_code())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.verify", e))?
        .unwrap_or(false);
        if !eligible {
            return Ok(None);
        }
        let hash = sha256_hex(token.as_bytes());
        let expires = at + Duration::hours(24);
        sqlx::query("INSERT INTO email_verification_tokens(token_hash,user_id,email,expires_at) VALUES($1,$2,$3,$4)").bind(&hash).bind(user_id.0).bind(email).bind(expires).execute(&mut *tx).await.map_err(|e|db_err("auth_outbox.verify",e))?;
        let job_id = enqueue(&mut tx, &message, &hash, expires, self.max_attempts)
            .await
            .map_err(|e| admission_err("auth_outbox.verify", e))?;
        if let Some(action) = audit_action {
            audit(&mut tx, user_id.0, action)
                .await
                .map_err(|e| db_err("auth_outbox.verify", e))?;
        }
        tx.commit()
            .await
            .map_err(|e| db_err("auth_outbox.verify", e))?;
        Ok(Some(AdmittedAuthMail { job_id, message }))
    }

    async fn issue_reset(
        &self,
        user_id: UserId,
        token: &VerificationToken,
        at: DateTime<Utc>,
        message: EmailMessage,
    ) -> Result<Option<AdmittedAuthMail>, AuthError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("auth_outbox.reset", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("auth_outbox.reset", e))?;
        let eligible=sqlx::query_scalar::<_,bool>("SELECT account_state IN ('PENDING_EMAIL_VERIFICATION','ACTIVE') FROM users WHERE id=$1 FOR UPDATE").bind(user_id.0).fetch_optional(&mut *tx).await.map_err(|e|db_err("auth_outbox.reset",e))?.unwrap_or(false);
        if !eligible {
            return Ok(None);
        }
        let hash = sha256_hex(token.as_bytes());
        let expires = at + Duration::hours(1);
        sqlx::query(
            "INSERT INTO password_reset_tokens(token_hash,user_id,expires_at) VALUES($1,$2,$3)",
        )
        .bind(&hash)
        .bind(user_id.0)
        .bind(expires)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.reset", e))?;
        let job_id = enqueue(&mut tx, &message, &hash, expires, self.max_attempts)
            .await
            .map_err(|e| admission_err("auth_outbox.reset", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("auth_outbox.reset", e))?;
        Ok(Some(AdmittedAuthMail { job_id, message }))
    }
}
