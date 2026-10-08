//! Accounts & authentication: ports, read models and the use-case service
//! Infrastructure implements the ports; the web layer
//! calls [`AuthService`] for every auth/account/role action.

use crate::audit::{AuditEvent, AuditLog};
use crate::email::{EmailKind, EmailMessage};
use crate::rate_limit::{RateLimitError, RateLimiter};
use async_trait::async_trait;
use bikesnest_domain::{
    AccountState, AuthenticationProvider, CsrfToken, LocaleCode, Password, PasswordPolicy,
    ProviderIdentity, Role, SessionId, User, UserEmail, UserId, VerificationToken,
};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("current password is incorrect")]
    InvalidCurrentPassword,
    #[error("email already registered")]
    EmailTaken,
    #[error("password does not meet the policy")]
    WeakPassword,
    #[error("invalid email")]
    InvalidEmail,
    #[error("verification token has expired")]
    TokenExpired,
    #[error("verification token has already been used")]
    TokenUsed,
    #[error("invalid verification token")]
    TokenInvalid,
    #[error("too many attempts, try again later")]
    RateLimited,
    #[error("account suspended")]
    AccountSuspended,
    #[error("account deleted")]
    AccountDeleted,
    #[error("identity provider failed")]
    ProviderFailed,
    #[error("you are not permitted to perform this action")]
    Unauthorized,
    /// The revoke would leave the system with no ADMIN at all — refused
    /// whether the actor is demoting themselves or another admin.
    #[error("the system must keep at least one admin")]
    RefuseAdminSelfRevoke,
    /// Storage refused a duplicate, or a concurrent writer won the race
    /// (unique violation, serialization failure, deadlock).
    #[error("that change conflicts with an existing record")]
    Conflict,
    /// Storage is unreachable or overloaded; the same request may work shortly.
    #[error("service temporarily unavailable")]
    Unavailable,
    #[error("internal error")]
    Internal,
}

impl From<RateLimitError> for AuthError {
    fn from(_: RateLimitError) -> Self {
        AuthError::RateLimited
    }
}

impl From<crate::email::EmailError> for AuthError {
    fn from(_: crate::email::EmailError) -> Self {
        AuthError::Internal
    }
}

impl From<crate::audit::AuditError> for AuthError {
    fn from(_: crate::audit::AuditError) -> Self {
        AuthError::Internal
    }
}

impl From<bikesnest_domain::DomainError> for AuthError {
    fn from(e: bikesnest_domain::DomainError) -> Self {
        match e {
            bikesnest_domain::DomainError::WeakPassword => AuthError::WeakPassword,
            bikesnest_domain::DomainError::EmptyEmail
            | bikesnest_domain::DomainError::InvalidEmail(_) => AuthError::InvalidEmail,
            bikesnest_domain::DomainError::InvalidRole(_)
            | bikesnest_domain::DomainError::InvalidState(_)
            | bikesnest_domain::DomainError::Invalid(_) => AuthError::Internal,
        }
    }
}

// ---------------------------------------------------------------------------
// Ports: password hashing, token generation, clock
// ---------------------------------------------------------------------------

/// Port: hash / verify a password (argon2id in production).
#[async_trait]
pub trait PasswordHasher: Send + Sync {
    async fn hash(&self, pw: &Password) -> Result<String, AuthError>;
    async fn verify(&self, pw: &Password, hash: &str) -> Result<bool, AuthError>;
}

/// Port: cryptographically secure random bytes for tokens/sessions.
pub trait TokenGenerator: Send + Sync {
    fn generate(&self) -> [u8; 32];
}

/// Port: the current time. All expiry logic goes through this (never an inline
/// `Utc::now()`) so tests stay deterministic.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

// ---------------------------------------------------------------------------
// Ports: persistence
// ---------------------------------------------------------------------------

/// One login method row (`authentication_identities`).
#[derive(Debug, Clone)]
pub struct IdentityRecord {
    pub id: i64,
    pub user_id: UserId,
    pub provider: AuthenticationProvider,
    pub provider_subject: String,
    pub credential_hash: Option<String>,
}

/// Result of an atomic email confirmation and any resulting old-address notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailConfirmationOutcome {
    pub user_id: UserId,
    pub email_changed: bool,
    pub mail: Option<AdmittedAuthMail>,
}

/// Compatibility alias for adapters and tests while confirmation ownership
/// moves into the transactional auth outbox.
pub type EmailVerificationOutcome = EmailConfirmationOutcome;

/// A new account to create (user + password identity + baseline USER role).
#[derive(Debug)]
pub struct NewAccount<'a> {
    pub email: &'a UserEmail,
    pub display_name: Option<&'a str>,
    pub password_hash: &'a str,
    pub state: AccountState,
    /// The language the signup happened in. Persisted on the row so the
    /// verification mail — and every later message, sent by a background job
    /// with no request in scope — is written in it.
    pub locale: LocaleCode,
}

/// Exact currently-effective terms displayed by the registration form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TermsAcceptance {
    pub policy_version_id: i64,
    pub version: String,
    pub shown_locale: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmittedAuthMail {
    pub job_id: i64,
    pub message: EmailMessage,
}

#[async_trait]
pub trait AuthOutbox: Send + Sync {
    async fn register(
        &self,
        new: NewAccount<'_>,
        token: &VerificationToken,
        at: DateTime<Utc>,
        message: EmailMessage,
        terms: Option<&TermsAcceptance>,
    ) -> Result<Option<AdmittedAuthMail>, AuthError>;
    // The explicit transition inputs keep the atomic persistence port from
    // accepting partially populated or ambiguous verification commands.
    #[allow(clippy::too_many_arguments)]
    async fn issue_verification(
        &self,
        user_id: UserId,
        email: &str,
        token: &VerificationToken,
        at: DateTime<Utc>,
        expected_state: AccountState,
        message: EmailMessage,
        audit_action: Option<&'static str>,
    ) -> Result<Option<AdmittedAuthMail>, AuthError>;
    async fn issue_reset(
        &self,
        user_id: UserId,
        token: &VerificationToken,
        at: DateTime<Utc>,
        message: EmailMessage,
    ) -> Result<Option<AdmittedAuthMail>, AuthError>;
    async fn confirm_email(
        &self,
        token: &VerificationToken,
        at: DateTime<Utc>,
        old_address_notice: EmailMessage,
    ) -> Result<Option<EmailConfirmationOutcome>, AuthError>;
    async fn complete_password_reset(
        &self,
        token: &VerificationToken,
        password_hash: &str,
        at: DateTime<Utc>,
        notice: EmailMessage,
    ) -> Result<Option<AdmittedAuthMail>, AuthError>;
    #[allow(clippy::too_many_arguments)]
    async fn change_password(
        &self,
        user_id: UserId,
        expected_hash: &str,
        password_hash: &str,
        current_session: &SessionId,
        at: DateTime<Utc>,
        notice: EmailMessage,
    ) -> Result<Option<AdmittedAuthMail>, AuthError>;
}

#[async_trait]
pub trait AuthMailDispatcher: Send + Sync {
    async fn dispatch(&self, mail: AdmittedAuthMail) -> Result<(), AuthError>;
}

/// Activity counters the admin user list shows next to each account, so a
/// suspend/grant decision is not made from an email address alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UserActivity {
    /// Newest `sessions.last_seen_at` for the account; `None` for an account
    /// that has never signed in (or whose sessions have been purged).
    pub last_active_at: Option<DateTime<Utc>>,
    /// Locations added, edits, proposals, reviews, verifications and photos —
    /// the same events the contribution feed lists, counted.
    pub contributions: i64,
}

/// One page of the admin user list, `limit`-capped, newest account first.
#[derive(Debug, Clone, Default)]
pub struct UserSearch<'a> {
    /// Case-insensitive substring of email or display name. `None` lists all.
    pub query: Option<&'a str>,
    /// Keyset cursor: the smallest id already shown (the list runs `id DESC`,
    /// so the next page is `id < after_id`).
    pub after_id: Option<i64>,
    pub limit: i64,
}

/// Port: account + role persistence. `Account` is the domain `User` with its
/// roles already loaded.
#[async_trait]
pub trait AccountRepository: Send + Sync {
    async fn find_by_email(&self, email: &UserEmail) -> Result<Option<User>, AuthError>;
    async fn find_by_id(&self, id: UserId) -> Result<Option<User>, AuthError>;
    async fn create(&self, new: NewAccount<'_>) -> Result<UserId, AuthError>;
    async fn set_state(&self, id: UserId, state: AccountState) -> Result<(), AuthError>;
    async fn mark_email_verified(&self, id: UserId, at: DateTime<Utc>) -> Result<(), AuthError>;
    async fn update_canonical_email(&self, id: UserId, email: &UserEmail) -> Result<(), AuthError>;
    /// Atomically suspend an eligible account, revoke its sessions plus all
    /// outstanding verification and password-reset tokens, and write the
    /// administrator audit event while holding the account row lock. Deleted
    /// and already-suspended accounts are unchanged and return `false`.
    async fn suspend_by_admin(&self, id: UserId, actor: UserId) -> Result<bool, AuthError>;
    /// Atomically restore only a suspended account and write its administrator
    /// audit event. A verified account becomes active; an unverified account
    /// returns to pending email verification. Other states are unchanged.
    async fn restore_by_admin(&self, id: UserId, actor: UserId) -> Result<bool, AuthError>;
    /// Persist the account's reading language (the header language toggle, for
    /// a signed-in user). Transactional email is rendered from this value.
    async fn set_locale(&self, id: UserId, locale: LocaleCode) -> Result<(), AuthError>;
    /// Public attribution is opt-in; unsupported adapters fail closed.
    async fn public_contribution_name(&self, _id: UserId) -> Result<bool, AuthError> {
        Ok(false)
    }
    /// Disabling must also permanently revoke attribution on old contributions.
    async fn set_public_contribution_name(
        &self,
        _id: UserId,
        _enabled: bool,
    ) -> Result<(), AuthError> {
        Err(AuthError::Internal)
    }
    async fn link_identity(
        &self,
        user_id: UserId,
        provider: AuthenticationProvider,
        subject: &str,
        hash: Option<&str>,
    ) -> Result<(), AuthError>;
    async fn find_identity(
        &self,
        provider: AuthenticationProvider,
        subject: &str,
    ) -> Result<Option<IdentityRecord>, AuthError>;
    async fn roles(&self, id: UserId) -> Result<Vec<Role>, AuthError>;
    /// How many accounts currently hold ADMIN. Backs the "never zero admins"
    /// guard, which a per-user role list cannot answer.
    async fn count_admins(&self) -> Result<i64, AuthError>;
    async fn grant_role(&self, id: UserId, role: Role, by: UserId) -> Result<(), AuthError>;
    /// Revoke `role` from `id`, refusing any revoke that would leave the system
    /// with zero administrators. Returns whether a row was removed.
    ///
    /// The guard and the delete are **one transaction** that locks the ADMIN
    /// rows: counting admins and then deleting as two statements is a race, and
    /// two admins demoting each other at the same moment both pass a count of
    /// two. Refusal is [`AuthError::RefuseAdminSelfRevoke`].
    async fn revoke_role_guarded(&self, id: UserId, role: Role) -> Result<bool, AuthError>;
    /// All accounts (for the admin user list).
    async fn list_users(&self) -> Result<Vec<User>, AuthError>;
    /// One keyset page of the admin user list, optionally filtered by a
    /// search term. Replaces loading every account to render a table.
    async fn search_users(&self, search: UserSearch<'_>) -> Result<Vec<User>, AuthError>;
    /// Display labels ("Ada Lovelace", else the email) for a batch of ids —
    /// **one** query for a whole page of audit rows or privacy requests, not
    /// one per row. Ids that no longer exist are absent from the map.
    async fn labels_for(&self, ids: &[i64]) -> Result<HashMap<i64, String>, AuthError>;
    /// Last-seen + contribution counters for a batch of ids (one query).
    async fn activity_for(&self, ids: &[i64]) -> Result<HashMap<i64, UserActivity>, AuthError>;
}

/// A resolved server-side session.
#[derive(Debug, Clone)]
pub struct Session {
    pub user_id: UserId,
    pub csrf_token: CsrfToken,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

/// Port: server-side session store. The cookie carries the raw id; the
/// store persists its SHA-256 hash. `resolve` applies idle + absolute expiry
/// and refreshes `last_seen_at`.
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create(
        &self,
        user_id: UserId,
        raw: &SessionId,
        csrf: &CsrfToken,
        now: DateTime<Utc>,
    ) -> Result<(), AuthError>;
    async fn resolve(
        &self,
        raw: &SessionId,
        now: DateTime<Utc>,
    ) -> Result<Option<Session>, AuthError>;
    async fn revoke(&self, raw: &SessionId) -> Result<(), AuthError>;
    async fn revoke_all_for_user_except(
        &self,
        user_id: UserId,
        keep: &SessionId,
    ) -> Result<(), AuthError>;
    /// Revoke every session for a user (the deletion path's "invalidate
    /// sessions" — no session is kept).
    async fn revoke_all_for_user(&self, user_id: UserId) -> Result<(), AuthError>;
}

/// Port: single-use verification / reset token store.
#[async_trait]
pub trait TokenStore: Send + Sync {
    async fn issue_verification(
        &self,
        user_id: UserId,
        email: &str,
        raw: &VerificationToken,
        now: DateTime<Utc>,
        expected_state: AccountState,
    ) -> Result<bool, AuthError>;
    async fn consume_verification(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<(UserId, String)>, AuthError>;
    /// Non-consuming lookup used for the application-layer eligibility gate.
    /// The account repository must still repeat the guard atomically when it
    /// consumes and confirms the token.
    async fn find_verification(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError>;
    async fn issue_reset(
        &self,
        user_id: UserId,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<bool, AuthError>;
    async fn consume_reset(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError>;
    /// Non-consuming lookup; the outbox repeats expiry and account guards
    /// atomically after password hashing.
    async fn find_reset(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError>;
}

/// Port: OAuth provider. The implementation is a dev stub.
#[async_trait]
pub trait OAuthProvider: Send + Sync {
    fn authorize_url(&self, state: &str) -> String;
    async fn exchange(&self, code: &str) -> Result<ProviderIdentity, AuthError>;
}

// ---------------------------------------------------------------------------
// Read models for the web layer
// ---------------------------------------------------------------------------

/// An authenticated principal derived from a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedUser {
    pub id: UserId,
    pub email: UserEmail,
    pub display_name: Option<String>,
    pub account_state: AccountState,
    pub is_verified: bool,
    pub roles: Vec<Role>,
}

impl AuthenticatedUser {
    pub fn from_user(u: &User) -> Self {
        Self {
            id: u.id,
            email: u.email.clone(),
            display_name: u.display_name.clone(),
            account_state: u.account_state,
            is_verified: u.is_verified(),
            roles: u.roles.clone(),
        }
    }

    /// The single authorization check used by handlers.
    pub fn has_role(&self, role: Role) -> bool {
        self.roles.contains(&role)
    }
}

/// A session resolved from a cookie, carrying the user + CSRF token.
pub struct ResolvedSession {
    pub user: AuthenticatedUser,
    pub csrf_token: CsrfToken,
}

/// Outcome of a successful sign-in: the raw session id (for the cookie), its
/// CSRF token (for the page) and the authenticated user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginOutcome {
    pub session: SessionId,
    pub csrf: CsrfToken,
    pub user: AuthenticatedUser,
}

// ---------------------------------------------------------------------------
// Rate-limit defaults
// ---------------------------------------------------------------------------

/// A dummy argon2id PHC string used to equalize login timing when an identity
/// does not exist (the verify call still runs argon2).
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

// ---------------------------------------------------------------------------
// AuthService
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub struct AuthService {
    accounts: Box<dyn AccountRepository>,
    sessions: Box<dyn SessionStore>,
    tokens: Box<dyn TokenStore>,
    hasher: Box<dyn PasswordHasher>,
    tokens_gen: Box<dyn TokenGenerator>,
    clock: Box<dyn Clock>,
    outbox: Box<dyn AuthOutbox>,
    mail_dispatcher: Box<dyn AuthMailDispatcher>,
    oauth: Box<dyn OAuthProvider>,
    rate_limiter: Box<dyn RateLimiter>,
    audit: Box<dyn AuditLog>,
    base_url: String,
    password_policy: PasswordPolicy,
}

#[allow(clippy::too_many_arguments)]
impl AuthService {
    pub fn new(
        accounts: Box<dyn AccountRepository>,
        sessions: Box<dyn SessionStore>,
        tokens: Box<dyn TokenStore>,
        hasher: Box<dyn PasswordHasher>,
        tokens_gen: Box<dyn TokenGenerator>,
        clock: Box<dyn Clock>,
        outbox: Box<dyn AuthOutbox>,
        mail_dispatcher: Box<dyn AuthMailDispatcher>,
        oauth: Box<dyn OAuthProvider>,
        rate_limiter: Box<dyn RateLimiter>,
        audit: Box<dyn AuditLog>,
        base_url: String,
    ) -> Self {
        Self {
            accounts,
            sessions,
            tokens,
            hasher,
            tokens_gen,
            clock,
            outbox,
            mail_dispatcher,
            oauth,
            rate_limiter,
            audit,
            base_url,
            password_policy: PasswordPolicy::default(),
        }
    }

    fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    fn verification_link(&self, token: &VerificationToken) -> String {
        format!(
            "{}/verify-email?token={}",
            self.base_url.trim_end_matches('/'),
            token.to_base64url()
        )
    }

    fn reset_link(&self, token: &VerificationToken) -> String {
        format!(
            "{}/password-reset/new?token={}",
            self.base_url.trim_end_matches('/'),
            token.to_base64url()
        )
    }

    fn account_link(&self) -> String {
        format!("{}/login", self.base_url.trim_end_matches('/'))
    }

    /// Describe the verification email for a token link. Subject and body are
    /// *not* built here: the message names its kind and locale, and the
    /// provider renders both from the catalog when it sends.
    fn verification_email(
        &self,
        account_id: UserId,
        to: &UserEmail,
        locale: LocaleCode,
        token: &VerificationToken,
        expires_at: DateTime<Utc>,
    ) -> EmailMessage {
        EmailMessage::linked(
            account_id,
            to.as_str(),
            locale,
            EmailKind::VerifyEmail {
                link: self.verification_link(token),
                expires_at: Some(expires_at),
            },
        )
    }

    /// Describe the "confirm your new address" email of an email change.
    fn change_email_message(
        &self,
        account_id: UserId,
        to: &UserEmail,
        locale: LocaleCode,
        token: &VerificationToken,
        expires_at: DateTime<Utc>,
    ) -> EmailMessage {
        EmailMessage::linked(
            account_id,
            to.as_str(),
            locale,
            EmailKind::ConfirmEmailChange {
                link: self.verification_link(token),
                expires_at: Some(expires_at),
            },
        )
    }

    /// Describe the password-reset email for a token link.
    fn reset_email(
        &self,
        account_id: UserId,
        to: &UserEmail,
        locale: LocaleCode,
        token: &VerificationToken,
        expires_at: DateTime<Utc>,
    ) -> EmailMessage {
        EmailMessage::linked(
            account_id,
            to.as_str(),
            locale,
            EmailKind::ResetPassword {
                link: self.reset_link(token),
                expires_at: Some(expires_at),
            },
        )
    }

    fn security_notice(
        &self,
        account_id: UserId,
        to: &UserEmail,
        locale: LocaleCode,
        email_changed: bool,
    ) -> EmailMessage {
        let notification_id = VerificationToken::new(self.tokens_gen.generate()).to_base64url();
        let account_link = self.account_link();
        let kind = if email_changed {
            EmailKind::EmailAddressChanged {
                account_link,
                notification_id,
            }
        } else {
            EmailKind::PasswordChanged {
                account_link,
                notification_id,
            }
        };
        EmailMessage::linked(account_id, to.as_str(), locale, kind)
    }

    /// The security transition and durable notice already committed. An inline
    /// delivery attempt may fail, but reporting the transition itself as failed
    /// would invite an impossible/unsafe replay with an old credential or spent
    /// token. The durable row retains its queue outcome, whether retryable or
    /// terminal.
    async fn dispatch_committed_notice(&self, mail: AdmittedAuthMail) {
        let account_id = mail.message.account_id;
        let kind = mail.message.kind.code();
        if self.mail_dispatcher.dispatch(mail).await.is_err() {
            let _ = self
                .audit
                .record(AuditEvent::failure(
                    Some(UserId(account_id)),
                    "auth.security_notice_dispatch_failed",
                    "email_kind",
                    kind,
                ))
                .await;
        }
    }

    async fn allowed(
        &self,
        key: &str,
        limit: u32,
        window: std::time::Duration,
    ) -> Result<(), AuthError> {
        if self
            .rate_limiter
            .check_sensitive(key, limit, window)
            .await?
        {
            Ok(())
        } else {
            Err(AuthError::RateLimited)
        }
    }

    // -----------------------------------------------------------------------
    // Register → verify → resend
    // -----------------------------------------------------------------------

    /// Register an account. Returning `Ok` whether the email is taken or not
    /// (no-existence-leak). Re-registering a still-pending address replaces
    /// its password with this submission, revokes its sessions, retires its
    /// earlier verification links and sends a fresh one: an unverified
    /// registration proves nothing about who owns the mailbox. Other existing
    /// states stay neutral and the caller renders the same response either way.
    pub async fn register(
        &self,
        ip: &str,
        raw_email: &str,
        display_name: Option<&str>,
        raw_password: &str,
        locale: LocaleCode,
    ) -> Result<(), AuthError> {
        self.register_accepting_terms(ip, raw_email, display_name, raw_password, locale, None)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn register_accepting_terms(
        &self,
        ip: &str,
        raw_email: &str,
        display_name: Option<&str>,
        raw_password: &str,
        locale: LocaleCode,
        terms: Option<TermsAcceptance>,
    ) -> Result<(), AuthError> {
        self.allowed(
            &format!("register:ip:{ip}"),
            REGISTER_IP_LIMIT,
            std::time::Duration::from_secs(60 * 60),
        )
        .await?;

        let email = UserEmail::parse(raw_email).map_err(|_| AuthError::InvalidEmail)?;
        self.password_policy.validate(raw_password)?;
        let password = Password::new(raw_password);

        let now = self.now();
        // Always pay the same hash cost. The outbox transaction authoritatively
        // decides whether this is a new account or neutral recovery; no
        // precheck can race into creating a dummy credential.
        let hash = self.hasher.hash(&password).await?;
        let token = VerificationToken::new(self.tokens_gen.generate());
        let message =
            self.verification_email(UserId(0), &email, locale, &token, now + Duration::hours(24));
        if let Some(mail) = self
            .outbox
            .register(
                NewAccount {
                    email: &email,
                    display_name,
                    password_hash: &hash,
                    state: AccountState::PendingEmailVerification,
                    locale,
                },
                &token,
                now,
                message,
                terms.as_ref(),
            )
            .await?
        {
            self.mail_dispatcher.dispatch(mail).await?;
        }
        Ok(())
    }

    /// Verify an email via a single-use token. Handles both registration
    /// (token email == account email → set verified + Active) and change-email
    /// (token email != account email → switch canonical email + verify).
    pub async fn verify_email(&self, raw_token: &str) -> Result<(), AuthError> {
        let now = self.now();
        let token = decode_token(raw_token).ok_or(AuthError::TokenInvalid)?;
        let Some(user_id) = self.tokens.find_verification(&token, now).await? else {
            return Err(AuthError::TokenInvalid);
        };
        let Some(user) = self.accounts.find_by_id(user_id).await? else {
            return Err(AuthError::TokenInvalid);
        };
        if !matches!(
            user.account_state,
            AccountState::PendingEmailVerification | AccountState::Active
        ) {
            return Err(AuthError::TokenInvalid);
        }
        let notice = self.security_notice(user.id, &user.email, user.locale, true);
        let Some(outcome) = self
            .outbox
            .confirm_email(&token, now, notice)
            .await
            .map_err(|e| match e {
                AuthError::Conflict => AuthError::EmailTaken,
                other => other,
            })?
        else {
            return Err(AuthError::TokenInvalid);
        };
        if let Some(mail) = outcome.mail {
            self.dispatch_committed_notice(mail).await;
        }
        Ok(())
    }

    /// Resend a verification email. Neutral even when no such account exists.
    pub async fn resend_verification(&self, ip: &str, email: &UserEmail) -> Result<(), AuthError> {
        let Some(user) = self.accounts.find_by_email(email).await? else {
            return Ok(());
        };
        if user.account_state != AccountState::PendingEmailVerification {
            return Ok(());
        }
        self.allowed(
            &format!("verif:user:{}", user.id.0),
            VERIFY_RESEND_USER_LIMIT,
            std::time::Duration::from_secs(60 * 60),
        )
        .await?;
        self.allowed(
            &format!("verif:ip:{ip}"),
            VERIFY_RESEND_IP_LIMIT,
            std::time::Duration::from_secs(60 * 60),
        )
        .await?;

        let token = VerificationToken::new(self.tokens_gen.generate());
        let now = self.now();
        let message = self.verification_email(
            user.id,
            email,
            user.locale,
            &token,
            now + Duration::hours(24),
        );
        let admitted = self
            .outbox
            .issue_verification(
                user.id,
                email.as_str(),
                &token,
                now,
                AccountState::PendingEmailVerification,
                message,
                None,
            )
            .await?;
        if let Some(mail) = admitted {
            self.mail_dispatcher.dispatch(mail).await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Login / logout
    // -----------------------------------------------------------------------

    /// Sign in by email + password. Returns the same error for bad credentials,
    /// suspended and deleted accounts (no-existence / account-state leak).
    /// Always audits the attempt.
    pub async fn login(
        &self,
        ip: &str,
        raw_email: &str,
        raw_password: &str,
    ) -> Result<LoginOutcome, AuthError> {
        let email = UserEmail::parse(raw_email).map_err(|_| AuthError::InvalidCredentials)?;
        self.allowed(
            &format!("login:{ip}:{}", email.as_str()),
            LOGIN_LIMIT,
            std::time::Duration::from_secs(15 * 60),
        )
        .await?;
        self.allowed(
            &format!("login:ip:{ip}"),
            LOGIN_IP_LIMIT,
            std::time::Duration::from_secs(15 * 60),
        )
        .await?;

        let now = self.now();
        let password = Password::new(raw_password);
        let identity_key = email.as_str();

        let Some(identity) = self
            .accounts
            .find_identity(AuthenticationProvider::Password, identity_key)
            .await?
        else {
            // Not found: still run a dummy verify to equalise timing.
            let _ = self.hasher.verify(&password, DUMMY_HASH).await;
            self.audit
                .record(AuditEvent::failure(
                    None,
                    "auth.login",
                    "user",
                    identity_key,
                ))
                .await?;
            return Err(AuthError::InvalidCredentials);
        };

        let Some(hash) = identity.credential_hash.as_deref() else {
            self.audit
                .record(AuditEvent::failure(
                    None,
                    "auth.login",
                    "user",
                    identity_key,
                ))
                .await?;
            return Err(AuthError::InvalidCredentials);
        };
        let ok = self.hasher.verify(&password, hash).await?;
        if !ok {
            self.audit
                .record(AuditEvent::failure(
                    None,
                    "auth.login",
                    "user",
                    identity_key,
                ))
                .await?;
            return Err(AuthError::InvalidCredentials);
        }

        let Some(user) = self.accounts.find_by_id(identity.user_id).await? else {
            self.audit
                .record(AuditEvent::failure(
                    None,
                    "auth.login",
                    "user",
                    identity_key,
                ))
                .await?;
            return Err(AuthError::InvalidCredentials);
        };
        // Suspended / deleted (and any future non-login state) are blocked *at
        // login*, with the generic message — the caller can never tell why.
        if !user.account_state.can_log_in() {
            self.audit
                .record(AuditEvent::failure(
                    None,
                    "auth.login",
                    "user",
                    identity_key,
                ))
                .await?;
            return Err(AuthError::InvalidCredentials);
        }

        let session = SessionId::new(self.tokens_gen.generate());
        let csrf = CsrfToken::new(self.tokens_gen.generate());
        self.sessions.create(user.id, &session, &csrf, now).await?;
        self.audit
            .record(AuditEvent::success(
                Some(user.id),
                "auth.login",
                "user",
                user.id.0.to_string(),
            ))
            .await?;
        Ok(LoginOutcome {
            session,
            csrf,
            user: AuthenticatedUser::from_user(&user),
        })
    }

    /// Revoke the current session.
    pub async fn logout(&self, session: &SessionId) -> Result<(), AuthError> {
        self.sessions.revoke(session).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Password reset
    // -----------------------------------------------------------------------

    /// Request a password reset. Neutral (no email) when no such account exists.
    pub async fn request_password_reset(
        &self,
        ip: &str,
        email: &UserEmail,
    ) -> Result<(), AuthError> {
        self.allowed(
            &format!("reset:ip:{ip}"),
            RESET_IP_LIMIT,
            std::time::Duration::from_secs(60 * 60),
        )
        .await?;
        self.allowed(
            &format!("reset:email:{}", email.as_str()),
            RESET_EMAIL_LIMIT,
            std::time::Duration::from_secs(60 * 60),
        )
        .await?;

        let Some(user) = self.accounts.find_by_email(email).await? else {
            return Ok(());
        };
        if !user.account_state.can_log_in() {
            return Ok(());
        }
        if self
            .accounts
            .find_identity(AuthenticationProvider::Password, email.as_str())
            .await?
            .is_none()
        {
            return Ok(());
        }
        let token = VerificationToken::new(self.tokens_gen.generate());
        let now = self.now();
        let message = self.reset_email(
            user.id,
            email,
            user.locale,
            &token,
            now + Duration::hours(1),
        );
        if let Some(mail) = self
            .outbox
            .issue_reset(user.id, &token, now, message)
            .await?
        {
            self.mail_dispatcher.dispatch(mail).await?;
        }
        Ok(())
    }

    /// Reset the password via a single-use, expiring token; revokes *all*
    /// sessions.
    pub async fn reset_password(
        &self,
        raw_token: &str,
        raw_password: &str,
    ) -> Result<(), AuthError> {
        let token = decode_token(raw_token).ok_or(AuthError::TokenInvalid)?;
        self.password_policy.validate(raw_password)?;
        let lookup_now = self.now();
        let Some(user_id) = self.tokens.find_reset(&token, lookup_now).await? else {
            return Err(AuthError::TokenInvalid);
        };
        let Some(user) = self.accounts.find_by_id(user_id).await? else {
            return Err(AuthError::TokenInvalid);
        };
        let hash = self.hasher.hash(&Password::new(raw_password)).await?;
        // Expiry is judged after the potentially slow password hash, at the
        // instant the atomic persistence transition begins.
        let now = self.now();
        let notice = self.security_notice(user.id, &user.email, user.locale, false);
        let Some(mail) = self
            .outbox
            .complete_password_reset(&token, &hash, now, notice)
            .await?
        else {
            return Err(AuthError::TokenInvalid);
        };
        self.dispatch_committed_notice(mail).await;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Authenticated settings
    // -----------------------------------------------------------------------

    /// Change the password. Requires the current password; revokes all *other*
    /// sessions (keeps `current`).
    pub async fn change_password(
        &self,
        user_id: UserId,
        current: &str,
        new: &str,
        current_session: &SessionId,
    ) -> Result<(), AuthError> {
        let Some(user) = self.accounts.find_by_id(user_id).await? else {
            return Err(AuthError::InvalidCredentials);
        };
        let identity = self
            .accounts
            .find_identity(AuthenticationProvider::Password, user.email.as_str())
            .await?
            .ok_or(AuthError::InvalidCredentials)?;
        let Some(hash) = identity.credential_hash.as_deref() else {
            return Err(AuthError::InvalidCredentials);
        };
        if !self.hasher.verify(&Password::new(current), hash).await? {
            return Err(AuthError::InvalidCurrentPassword);
        }
        self.password_policy.validate(new)?;
        let new_hash = self.hasher.hash(&Password::new(new)).await?;
        let notice = self.security_notice(user.id, &user.email, user.locale, false);
        let Some(mail) = self
            .outbox
            .change_password(
                user_id,
                hash,
                &new_hash,
                current_session,
                self.now(),
                notice,
            )
            .await?
        else {
            return Err(AuthError::InvalidCurrentPassword);
        };
        self.dispatch_committed_notice(mail).await;
        Ok(())
    }

    /// Request an email change. Verifies the current password, then issues a
    /// verification token for the *new* address. The actual switch happens in
    /// [`AuthService::verify_email`] when that token is consumed.
    pub async fn change_email(
        &self,
        user_id: UserId,
        current_password: &str,
        new_email: &UserEmail,
    ) -> Result<(), AuthError> {
        let Some(user) = self.accounts.find_by_id(user_id).await? else {
            return Err(AuthError::InvalidCredentials);
        };
        let identity = self
            .accounts
            .find_identity(AuthenticationProvider::Password, user.email.as_str())
            .await?
            .ok_or(AuthError::InvalidCredentials)?;
        let Some(hash) = identity.credential_hash.as_deref() else {
            return Err(AuthError::InvalidCredentials);
        };
        if !self
            .hasher
            .verify(&Password::new(current_password), hash)
            .await?
        {
            return Err(AuthError::InvalidCurrentPassword);
        }
        // Reject a taken address here rather than at confirm time: the unique
        // index would otherwise fire only after the mail was sent and the
        // single-use token spent, leaving the user with no way forward.
        if self.accounts.find_by_email(new_email).await?.is_some() {
            return Err(AuthError::EmailTaken);
        }
        let token = VerificationToken::new(self.tokens_gen.generate());
        let now = self.now();
        let message = self.change_email_message(
            user.id,
            new_email,
            user.locale,
            &token,
            now + Duration::hours(24),
        );
        let Some(mail) = self
            .outbox
            .issue_verification(
                user_id,
                new_email.as_str(),
                &token,
                now,
                AccountState::Active,
                message,
                Some("auth.email_change_requested"),
            )
            .await?
        else {
            return Err(AuthError::InvalidCredentials);
        };
        self.mail_dispatcher.dispatch(mail).await?;
        Ok(())
    }

    /// Persist the language a signed-in user just chose with the header
    /// toggle. The cookie already switched the interface; this is what makes
    /// the *next* transactional email — rendered by a background job, with no
    /// request and no `Accept-Language` in scope — speak the same language.
    pub async fn set_locale(&self, user_id: UserId, locale: LocaleCode) -> Result<(), AuthError> {
        self.accounts.set_locale(user_id, locale).await
    }

    pub async fn public_contribution_name(&self, user_id: UserId) -> Result<bool, AuthError> {
        self.accounts.public_contribution_name(user_id).await
    }

    pub async fn set_public_contribution_name(
        &self,
        user_id: UserId,
        enabled: bool,
    ) -> Result<(), AuthError> {
        self.accounts
            .set_public_contribution_name(user_id, enabled)
            .await
    }

    // -----------------------------------------------------------------------
    // OAuth
    // -----------------------------------------------------------------------

    pub fn oauth_authorize_url(&self, state: &str) -> String {
        self.oauth.authorize_url(state)
    }

    pub async fn oauth_callback(&self, code: &str) -> Result<LoginOutcome, AuthError> {
        let identity = self.oauth.exchange(code).await?;
        let now = self.now();

        // 1) Match by (provider, subject) → existing identity → log in.
        if let Some(rec) = self
            .accounts
            .find_identity(identity.provider, &identity.subject)
            .await?
        {
            let user = self
                .accounts
                .find_by_id(rec.user_id)
                .await?
                .ok_or(AuthError::ProviderFailed)?;
            return self.sign_in(user, now).await;
        }

        // 2) Match by a *verified* email → link identity to that existing account.
        if identity.email_verified
            && let Some(existing) = self.accounts.find_by_email(&identity.email).await?
        {
            self.accounts
                .link_identity(existing.id, identity.provider, &identity.subject, None)
                .await?;
            return self.sign_in(existing, now).await;
        }

        // 3) No match → create a new account (Active only if the provider
        //    asserts a verified email; else pending), then link the identity.
        let state = if identity.email_verified {
            AccountState::Active
        } else {
            AccountState::PendingEmailVerification
        };
        let user_id = self
            .accounts
            .create(NewAccount {
                email: &identity.email,
                display_name: None,
                password_hash: "",
                state,
                // No page was rendered for this signup (it is a provider
                // callback), so the account starts on the product default and
                // the language toggle updates it on the first visit.
                locale: LocaleCode::default(),
            })
            .await?;
        self.accounts
            .link_identity(user_id, identity.provider, &identity.subject, None)
            .await?;
        if identity.email_verified {
            self.accounts.mark_email_verified(user_id, now).await?;
        }
        let user = self
            .accounts
            .find_by_id(user_id)
            .await?
            .ok_or(AuthError::ProviderFailed)?;
        self.audit
            .record(AuditEvent::success(
                Some(user_id),
                "auth.oauth_linked",
                "user",
                user_id.0.to_string(),
            ))
            .await?;
        self.sign_in(user, now).await
    }

    async fn sign_in(&self, user: User, now: DateTime<Utc>) -> Result<LoginOutcome, AuthError> {
        // Uniform gate for every OAuth path (match-by-identity, email-link,
        // fresh account): a non-login state cannot establish a session.
        if !user.account_state.can_log_in() {
            return Err(AuthError::InvalidCredentials);
        }
        let session = SessionId::new(self.tokens_gen.generate());
        let csrf = CsrfToken::new(self.tokens_gen.generate());
        self.sessions.create(user.id, &session, &csrf, now).await?;
        self.audit
            .record(AuditEvent::success(
                Some(user.id),
                "auth.login",
                "user",
                user.id.0.to_string(),
            ))
            .await?;
        Ok(LoginOutcome {
            session,
            csrf,
            user: AuthenticatedUser::from_user(&user),
        })
    }

    // -----------------------------------------------------------------------
    // Role management
    // -----------------------------------------------------------------------

    /// Grant a role. Requires an ADMIN actor; refuses to revoke the actor's own
    /// last ADMIN.
    pub async fn grant_role(
        &self,
        actor: &AuthenticatedUser,
        target: UserId,
        role: Role,
    ) -> Result<(), AuthError> {
        if !actor.has_role(Role::Admin) {
            return Err(AuthError::Unauthorized);
        }
        if role == Role::User {
            return Err(AuthError::Unauthorized);
        }
        self.accounts.grant_role(target, role, actor.id).await?;
        self.audit
            .record(AuditEvent::success(
                Some(actor.id),
                "role.granted",
                "user",
                target.0.to_string(),
            ))
            .await?;
        Ok(())
    }

    /// Revoke a role. Requires an ADMIN actor; refuses any revoke that would
    /// leave the system without an admin — self-demotion and demoting another
    /// admin alike. Self-demotion is allowed whenever a second admin remains.
    /// The last-admin check is atomic with the delete (see
    /// [`AccountRepository::revoke_role_guarded`]).
    pub async fn revoke_role(
        &self,
        actor: &AuthenticatedUser,
        target: UserId,
        role: Role,
    ) -> Result<(), AuthError> {
        if !actor.has_role(Role::Admin) {
            return Err(AuthError::Unauthorized);
        }
        if role == Role::User {
            return Err(AuthError::Unauthorized);
        }
        // The "never zero admins" rule is enforced by the repository, in the
        // same transaction as the delete and holding a lock on the ADMIN rows.
        // Counting here and deleting after would let two admins demote each
        // other concurrently: both read a count of two, both delete.
        let removed = self.accounts.revoke_role_guarded(target, role).await?;
        if removed {
            self.audit
                .record(AuditEvent::success(
                    Some(actor.id),
                    "role.revoked",
                    "user",
                    target.0.to_string(),
                ))
                .await?;
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Suspend / restore — ADMIN-only. Suspension revokes every active
    // session so it takes effect immediately, not just at the next login.
    // -----------------------------------------------------------------------

    /// Suspend an account: set `Suspended`, revoke all sessions, audit.
    pub async fn suspend_user(
        &self,
        actor: &AuthenticatedUser,
        target: UserId,
    ) -> Result<(), AuthError> {
        if !actor.has_role(Role::Admin) {
            return Err(AuthError::Unauthorized);
        }
        self.accounts.suspend_by_admin(target, actor.id).await?;
        Ok(())
    }

    /// Restore a suspended account to its verification-appropriate state.
    pub async fn restore_user(
        &self,
        actor: &AuthenticatedUser,
        target: UserId,
    ) -> Result<(), AuthError> {
        if !actor.has_role(Role::Admin) {
            return Err(AuthError::Unauthorized);
        }
        self.accounts.restore_by_admin(target, actor.id).await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Session resolution for middleware
    // -----------------------------------------------------------------------

    /// Resolve a raw session id (from the cookie) to an authenticated principal
    /// and the CSRF token. `None` for a missing / invalid / expired / revoked session
    /// (deny-by-default).
    /// All accounts with roles for the admin user list.
    pub async fn list_users(&self) -> Result<Vec<AuthenticatedUser>, AuthError> {
        let users = self.accounts.list_users().await?;
        Ok(users.iter().map(AuthenticatedUser::from_user).collect())
    }

    /// One searched, keyset-paginated page of the admin user list. `limit` is
    /// clamped by the repository.
    pub async fn search_users(
        &self,
        query: Option<&str>,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<AuthenticatedUser>, AuthError> {
        let query = query.map(str::trim).filter(|q| !q.is_empty());
        let users = self
            .accounts
            .search_users(UserSearch {
                query,
                after_id,
                limit,
            })
            .await?;
        Ok(users.iter().map(AuthenticatedUser::from_user).collect())
    }

    /// Display labels for a batch of user ids (audit rows, privacy requests).
    pub async fn user_labels(&self, ids: &[i64]) -> Result<HashMap<i64, String>, AuthError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        self.accounts.labels_for(ids).await
    }

    /// Last-active + contribution counters for a batch of user ids.
    pub async fn user_activity(
        &self,
        ids: &[i64],
    ) -> Result<HashMap<i64, UserActivity>, AuthError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        self.accounts.activity_for(ids).await
    }

    pub async fn resolve_session(
        &self,
        raw: &SessionId,
    ) -> Result<Option<ResolvedSession>, AuthError> {
        let now = self.now();
        let Some(session) = self.sessions.resolve(raw, now).await? else {
            return Ok(None);
        };
        let Some(user) = self.accounts.find_by_id(session.user_id).await? else {
            return Ok(None);
        };
        if !user.account_state.can_access_account() {
            return Ok(None);
        }
        Ok(Some(ResolvedSession {
            user: AuthenticatedUser::from_user(&user),
            csrf_token: session.csrf_token,
        }))
    }
}

fn decode_token(raw: &str) -> Option<VerificationToken> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(raw)
        .ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&bytes);
    Some(VerificationToken::new(arr))
}

const REGISTER_IP_LIMIT: u32 = 3;
const LOGIN_LIMIT: u32 = 5;
const LOGIN_IP_LIMIT: u32 = 10;
const RESET_IP_LIMIT: u32 = 3;
const RESET_EMAIL_LIMIT: u32 = 3;
const VERIFY_RESEND_USER_LIMIT: u32 = 3;
const VERIFY_RESEND_IP_LIMIT: u32 = 5;
