//! Atomic authentication transitions into the account-linked mail outbox.

use crate::Db;
use crate::auth::hash::sha256_hex;
use crate::email::idempotency_key;
use crate::job::repo::{MailCredential, MailEnqueue, enqueue_mail_on};
use async_trait::async_trait;
use bikesnest_application::{
    AdmittedAuthMail, AuthError, AuthOutbox, EmailConfirmationOutcome, EmailMessage, NewAccount,
    TermsAcceptance,
};
use bikesnest_domain::{AccountState, LocaleCode, SessionId, UserEmail, UserId, VerificationToken};
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
    expires_at: DateTime<Utc>,
    max_attempts: i32,
) -> Result<i64, crate::job::JobRepoError> {
    // `None` remains readable/admissible for rows produced before expiry was
    // serialized into the payload. Every current application constructor
    // supplies `Some(expires_at)` and must match the authoritative token row.
    if message
        .kind
        .expires_at()
        .is_some_and(|message_expiry| message_expiry != expires_at)
    {
        return Err(crate::job::JobRepoError::InvalidMailCredential);
    }
    let payload = serde_json::to_value(message)
        .map_err(|error| crate::job::JobRepoError::Db(sqlx::Error::Encode(Box::new(error))))?;
    let key = idempotency_key(message);
    enqueue_mail_on(
        &mut *tx,
        MailEnqueue {
            payload: &payload,
            account_id: message.account_id,
            credential: MailCredential::Token { hash: token_hash },
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

async fn enqueue_notice(
    tx: &mut Transaction<'_, Postgres>,
    message: &EmailMessage,
    audit_id: i64,
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
            credential: MailCredential::SecurityNotice { audit_id },
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
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("INSERT INTO audit_events(actor_user_id,action,target_type,target_id,result,metadata) VALUES($1,$2,'user',$3,'success','{}') RETURNING id")
        .bind(user_id).bind(action).bind(user_id.to_string()).fetch_one(&mut **tx).await
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

async fn validate_registration_terms(
    conn: &mut sqlx::PgConnection,
    terms: Option<&TermsAcceptance>,
) -> Result<Option<DateTime<Utc>>, AuthError> {
    let Some(terms) = terms else {
        return Ok(None);
    };
    sqlx::query("SELECT pg_advisory_xact_lock(726_159_001)")
        .execute(&mut *conn)
        .await
        .map_err(|e| db_err("auth_outbox.register", e))?;
    let decision_at: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| db_err("auth_outbox.register", e))?;
    let valid = sqlx::query_scalar::<_, bool>(
        r#"SELECT kind='terms' AND id=$1 AND version=$2 AND locale=$3
           AND effective_at<=$4
           AND (superseded_at IS NULL OR superseded_at>$4)
           FROM policy_version WHERE id=$1 FOR UPDATE"#,
    )
    .bind(terms.policy_version_id)
    .bind(&terms.version)
    .bind(&terms.shown_locale)
    .bind(decision_at)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|e| db_err("auth_outbox.register", e))?
    .unwrap_or(false);
    if !valid {
        return Err(AuthError::Conflict);
    }
    Ok(Some(decision_at))
}

#[async_trait]
impl AuthOutbox for SqlxAuthOutbox {
    async fn register(
        &self,
        new: NewAccount<'_>,
        token: &VerificationToken,
        at: DateTime<Utc>,
        mut message: EmailMessage,
        terms: Option<&TermsAcceptance>,
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
            validate_registration_terms(&mut tx, terms).await?;
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
                   mail_recipient_hash=NULL,mail_transition_audit_id=NULL,
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
        let terms_decision_at = validate_registration_terms(&mut tx, terms).await?;
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
        if let (Some(terms), Some(decision_at)) = (terms, terms_decision_at) {
            sqlx::query("INSERT INTO terms_acknowledgement(user_id,policy_version_id,terms_version,shown_locale,acknowledged_at,source) VALUES($1,$2,$3,$4,$5,'signup')")
                .bind(user_id).bind(terms.policy_version_id).bind(&terms.version).bind(&terms.shown_locale)
                .bind(decision_at)
                .execute(&mut *tx).await.map_err(|e| db_err("auth_outbox.register", e))?;
        }
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

    async fn confirm_email(
        &self,
        token: &VerificationToken,
        at: DateTime<Utc>,
        old_address_notice: EmailMessage,
    ) -> Result<Option<EmailConfirmationOutcome>, AuthError> {
        let token_hash = sha256_hex(token.as_bytes());
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        let token_row: Option<(i64, String)> = sqlx::query_as(
            "SELECT user_id,email FROM email_verification_tokens
             WHERE token_hash=$1 AND used_at IS NULL
               AND expires_at>GREATEST($2,clock_timestamp())",
        )
        .bind(&token_hash)
        .bind(at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        let Some((user_id, new_email)) = token_row else {
            return Ok(None);
        };
        let user: Option<(String, String, String)> = sqlx::query_as(
            "SELECT email,account_state::text,locale FROM users WHERE id=$1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        let Some((old_email, state, locale)) = user else {
            return Ok(None);
        };
        if !matches!(state.as_str(), "PENDING_EMAIL_VERIFICATION" | "ACTIVE") {
            return Ok(None);
        }
        let locale = LocaleCode::parse(&locale).ok_or(AuthError::Internal)?;
        if old_address_notice.account_id != user_id
            || !old_address_notice.to.eq_ignore_ascii_case(&old_email)
            || old_address_notice.locale != locale
            || old_address_notice.kind.code() != "email_changed"
        {
            return Err(AuthError::Conflict);
        }
        let changed = !old_email.eq_ignore_ascii_case(&new_email);
        if changed && state != "ACTIVE" {
            return Ok(None);
        }
        let parsed_email = UserEmail::parse(&new_email).map_err(|_| AuthError::Internal)?;
        let consumed = sqlx::query(
            "UPDATE email_verification_tokens SET used_at=$2
             WHERE token_hash=$1 AND user_id=$3 AND used_at IS NULL
               AND expires_at>GREATEST($2,clock_timestamp())",
        )
        .bind(&token_hash)
        .bind(at)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        if consumed.rows_affected() != 1 {
            return Ok(None);
        }
        sqlx::query(
            "UPDATE users SET email=$2,email_verified_at=$3,account_state='ACTIVE',updated_at=now()
             WHERE id=$1",
        )
        .bind(user_id)
        .bind(parsed_email.as_str())
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        sqlx::query(
            "UPDATE authentication_identities SET provider_subject=$2
             WHERE user_id=$1 AND provider='password'",
        )
        .bind(user_id)
        .bind(parsed_email.as_str())
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        if changed {
            sqlx::query(
                "UPDATE sessions SET revoked_at=$2 WHERE user_id=$1 AND revoked_at IS NULL",
            )
            .bind(user_id)
            .bind(at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        }
        let action = if changed {
            "auth.email_changed"
        } else {
            "auth.email_verified"
        };
        let audit_id = audit(&mut tx, user_id, action)
            .await
            .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        let mail = if changed {
            let job_id = enqueue_notice(&mut tx, &old_address_notice, audit_id, self.max_attempts)
                .await
                .map_err(|e| admission_err("auth_outbox.confirm_email", e))?;
            Some(AdmittedAuthMail {
                job_id,
                message: old_address_notice,
            })
        } else {
            None
        };
        tx.commit()
            .await
            .map_err(|e| db_err("auth_outbox.confirm_email", e))?;
        Ok(Some(EmailConfirmationOutcome {
            user_id: UserId(user_id),
            email_changed: changed,
            mail,
        }))
    }

    async fn complete_password_reset(
        &self,
        token: &VerificationToken,
        password_hash: &str,
        at: DateTime<Utc>,
        notice: EmailMessage,
    ) -> Result<Option<AdmittedAuthMail>, AuthError> {
        let token_hash = sha256_hex(token.as_bytes());
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        let user_id: Option<i64> = sqlx::query_scalar(
            "SELECT user_id FROM password_reset_tokens
             WHERE token_hash=$1 AND used_at IS NULL
               AND expires_at>GREATEST($2,clock_timestamp())",
        )
        .bind(&token_hash)
        .bind(at)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        let Some(user_id) = user_id else {
            return Ok(None);
        };
        let user: Option<(String, String, String)> = sqlx::query_as(
            "SELECT email,account_state::text,locale FROM users WHERE id=$1 FOR UPDATE",
        )
        .bind(user_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        let Some((email, state, locale)) = user else {
            return Ok(None);
        };
        if !matches!(state.as_str(), "PENDING_EMAIL_VERIFICATION" | "ACTIVE") {
            return Ok(None);
        }
        let locale = LocaleCode::parse(&locale).ok_or(AuthError::Internal)?;
        if notice.account_id != user_id
            || !notice.to.eq_ignore_ascii_case(&email)
            || notice.locale != locale
            || notice.kind.code() != "password_changed"
        {
            return Err(AuthError::Conflict);
        }
        let consumed = sqlx::query(
            "UPDATE password_reset_tokens SET used_at=$2
             WHERE token_hash=$1 AND user_id=$3 AND used_at IS NULL
               AND expires_at>GREATEST($2,clock_timestamp())",
        )
        .bind(&token_hash)
        .bind(at)
        .bind(user_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        if consumed.rows_affected() != 1 {
            return Ok(None);
        }
        let updated = sqlx::query(
            "UPDATE authentication_identities SET credential_hash=$2
             WHERE user_id=$1 AND provider='password'",
        )
        .bind(user_id)
        .bind(password_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        if updated.rows_affected() != 1 {
            return Err(AuthError::Internal);
        }
        sqlx::query("UPDATE sessions SET revoked_at=$2 WHERE user_id=$1 AND revoked_at IS NULL")
            .bind(user_id)
            .bind(at)
            .execute(&mut *tx)
            .await
            .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at=$2 WHERE user_id=$1 AND used_at IS NULL",
        )
        .bind(user_id)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        let audit_id = audit(&mut tx, user_id, "auth.password_changed")
            .await
            .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        let job_id = enqueue_notice(&mut tx, &notice, audit_id, self.max_attempts)
            .await
            .map_err(|e| admission_err("auth_outbox.complete_password_reset", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("auth_outbox.complete_password_reset", e))?;
        Ok(Some(AdmittedAuthMail {
            job_id,
            message: notice,
        }))
    }

    async fn change_password(
        &self,
        user_id: UserId,
        expected_hash: &str,
        password_hash: &str,
        current_session: &SessionId,
        at: DateTime<Utc>,
        notice: EmailMessage,
    ) -> Result<Option<AdmittedAuthMail>, AuthError> {
        let mut conn = self
            .db
            .acquire()
            .await
            .map_err(|e| db_err("auth_outbox.change_password", e))?;
        let mut tx = conn
            .begin()
            .await
            .map_err(|e| db_err("auth_outbox.change_password", e))?;
        let row: Option<(String, String, String, Option<String>)> = sqlx::query_as(
            "SELECT u.email,u.account_state::text,u.locale,i.credential_hash
             FROM users u JOIN authentication_identities i
               ON i.user_id=u.id AND i.provider='password'
             WHERE u.id=$1 FOR UPDATE OF u,i",
        )
        .bind(user_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.change_password", e))?;
        let Some((email, state, locale, current_hash)) = row else {
            return Ok(None);
        };
        let locale = LocaleCode::parse(&locale).ok_or(AuthError::Internal)?;
        let keep_hash = sha256_hex(current_session.as_bytes());
        // This row lock is the linearization boundary with logout/revocation:
        // a session cannot become revoked between authorization and commit.
        let current_session = sqlx::query_as::<_, (Option<DateTime<Utc>>, DateTime<Utc>)>(
            "SELECT revoked_at,expires_at FROM sessions
             WHERE token_hash=$1 AND user_id=$2 FOR UPDATE",
        )
        .bind(&keep_hash)
        .bind(user_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.change_password", e))?;
        let db_now: DateTime<Utc> = sqlx::query_scalar("SELECT clock_timestamp()")
            .fetch_one(&mut *tx)
            .await
            .map_err(|e| db_err("auth_outbox.change_password", e))?;
        let current_session_valid = current_session.is_some_and(|(revoked_at, expires_at)| {
            revoked_at.is_none() && expires_at > at.max(db_now)
        });
        if state != "ACTIVE"
            || current_hash.as_deref() != Some(expected_hash)
            || !current_session_valid
            || notice.account_id != user_id.0
            || !notice.to.eq_ignore_ascii_case(&email)
            || notice.locale != locale
            || notice.kind.code() != "password_changed"
        {
            return Ok(None);
        }
        let updated = sqlx::query(
            "UPDATE authentication_identities SET credential_hash=$2
             WHERE user_id=$1 AND provider='password' AND credential_hash=$3",
        )
        .bind(user_id.0)
        .bind(password_hash)
        .bind(expected_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.change_password", e))?;
        if updated.rows_affected() != 1 {
            return Ok(None);
        }
        sqlx::query(
            "UPDATE sessions SET revoked_at=$2
             WHERE user_id=$1 AND token_hash<>$3 AND revoked_at IS NULL",
        )
        .bind(user_id.0)
        .bind(at)
        .bind(keep_hash)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.change_password", e))?;
        sqlx::query(
            "UPDATE password_reset_tokens SET used_at=$2
             WHERE user_id=$1 AND used_at IS NULL",
        )
        .bind(user_id.0)
        .bind(at)
        .execute(&mut *tx)
        .await
        .map_err(|e| db_err("auth_outbox.change_password", e))?;
        let audit_id = audit(&mut tx, user_id.0, "auth.password_changed")
            .await
            .map_err(|e| db_err("auth_outbox.change_password", e))?;
        let job_id = enqueue_notice(&mut tx, &notice, audit_id, self.max_attempts)
            .await
            .map_err(|e| admission_err("auth_outbox.change_password", e))?;
        tx.commit()
            .await
            .map_err(|e| db_err("auth_outbox.change_password", e))?;
        Ok(Some(AdmittedAuthMail {
            job_id,
            message: notice,
        }))
    }
}
