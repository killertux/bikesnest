//! Application-layer auth tests with in-memory fakes. These validate the
//! security-critical use-case behaviour without a database.

use async_trait::async_trait;
use bikesnest_application::{
    AccountRepository, AdmittedAuthMail, AuditEvent, AuditLog, AuthError, AuthMailDispatcher,
    AuthOutbox, AuthService, AuthenticatedUser, Clock, EmailKind, EmailMessage,
    EmailVerificationOutcome, IdentityRecord, LoginOutcome, NewAccount, OAuthProvider,
    PasswordHasher, RateLimitError, RateLimiter, Session, SessionStore, TokenGenerator, TokenStore,
    UserActivity, UserSearch,
};
use bikesnest_domain::{
    AccountState, AuthenticationProvider, CsrfToken, LocaleCode, Password, ProviderIdentity, Role,
    SessionId, User, UserEmail, UserId, VerificationToken,
};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Shared in-memory "database" + fakes
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FakeDb {
    users: Vec<User>,
    identities: Vec<IdentityRecord>,
    sessions: Vec<(String, Session)>, // keyed by raw session id hex
    verification: Vec<(String, UserId, String, bool)>, // (token hex, user, email, used)
    reset: Vec<(String, UserId, DateTime<Utc>, bool)>,
    audits: Vec<AuditEvent>,
    /// What the use cases handed to the email *queue*. Nothing here was
    /// delivered: `AuthService` can no longer reach a provider at all, which is
    /// the point — a broken ESP cannot fail a registration any more.
    emails: Vec<EmailMessage>,
    outbox: Vec<AdmittedAuthMail>,
    dispatched: Vec<i64>,
    /// Set to make `FakeQueue::enqueue` fail, standing in for "the database
    /// that holds the job queue is unreachable".
    queue_broken: bool,
    dispatch_broken: bool,
    hash_broken: bool,
    next_id: i64,
}

#[derive(Clone)]
struct FakeRepo {
    db: Arc<Mutex<FakeDb>>,
}

impl FakeRepo {
    fn new(arc: Arc<Mutex<FakeDb>>) -> Self {
        Self { db: arc }
    }
}

// --- AccountRepository ---
#[async_trait]
impl AccountRepository for FakeRepo {
    async fn find_by_email(&self, email: &UserEmail) -> Result<Option<User>, AuthError> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .users
            .iter()
            .find(|u| u.email == *email)
            .cloned())
    }
    async fn find_by_id(&self, id: UserId) -> Result<Option<User>, AuthError> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .users
            .iter()
            .find(|u| u.id == id)
            .cloned())
    }
    async fn create(&self, new: NewAccount<'_>) -> Result<UserId, AuthError> {
        let mut db = self.db.lock().unwrap();
        db.next_id += 1;
        let id = UserId(db.next_id);
        let mut user = User::new(id, new.email.clone(), new.display_name.map(str::to_string));
        user.account_state = new.state;
        user.locale = new.locale;
        db.users.push(user);
        if !new.password_hash.is_empty() {
            db.identities.push(IdentityRecord {
                id: id.0,
                user_id: id,
                provider: AuthenticationProvider::Password,
                provider_subject: new.email.as_str().to_string(),
                credential_hash: Some(new.password_hash.to_string()),
            });
        }
        Ok(id)
    }
    async fn set_locale(&self, id: UserId, locale: LocaleCode) -> Result<(), AuthError> {
        if let Some(u) = self
            .db
            .lock()
            .unwrap()
            .users
            .iter_mut()
            .find(|u| u.id == id)
        {
            u.locale = locale;
        }
        Ok(())
    }
    async fn set_state(&self, id: UserId, state: AccountState) -> Result<(), AuthError> {
        if let Some(u) = self
            .db
            .lock()
            .unwrap()
            .users
            .iter_mut()
            .find(|u| u.id == id)
        {
            u.account_state = state;
        }
        Ok(())
    }
    async fn mark_email_verified(&self, id: UserId, at: DateTime<Utc>) -> Result<(), AuthError> {
        if let Some(u) = self
            .db
            .lock()
            .unwrap()
            .users
            .iter_mut()
            .find(|u| u.id == id)
        {
            u.email_verified_at = Some(at);
        }
        Ok(())
    }
    async fn update_canonical_email(&self, id: UserId, email: &UserEmail) -> Result<(), AuthError> {
        let mut db = self.db.lock().unwrap();
        if let Some(u) = db.users.iter_mut().find(|u| u.id == id) {
            u.email = email.clone();
        }
        for i in db.identities.iter_mut() {
            if i.user_id == id && i.provider == AuthenticationProvider::Password {
                i.provider_subject = email.as_str().to_string();
            }
        }
        Ok(())
    }
    async fn suspend_by_admin(&self, id: UserId, actor: UserId) -> Result<bool, AuthError> {
        let mut db = self.db.lock().unwrap();
        // Mirrors the SQL guard: never suspend the last active ADMIN.
        let other_active_admins = db
            .users
            .iter()
            .filter(|u| {
                u.id != id
                    && u.roles.contains(&Role::Admin)
                    && u.account_state == AccountState::Active
            })
            .count();
        let Some(user) = db.users.iter_mut().find(|u| u.id == id) else {
            return Ok(false);
        };
        if !matches!(
            user.account_state,
            AccountState::Active | AccountState::PendingEmailVerification
        ) {
            return Ok(false);
        }
        if user.account_state == AccountState::Active
            && user.roles.contains(&Role::Admin)
            && other_active_admins == 0
        {
            return Err(AuthError::LastActiveAdmin);
        }
        user.account_state = AccountState::Suspended;
        for (_, session) in &mut db.sessions {
            if session.user_id == id {
                session.revoked_at = Some(Utc::now());
            }
        }
        for (_, user_id, _, used) in &mut db.verification {
            if *user_id == id {
                *used = true;
            }
        }
        for (_, user_id, _, used) in &mut db.reset {
            if *user_id == id {
                *used = true;
            }
        }
        db.audits.push(AuditEvent::success(
            Some(actor),
            "user.suspended",
            "user",
            id.0.to_string(),
        ));
        Ok(true)
    }
    async fn restore_by_admin(&self, id: UserId, actor: UserId) -> Result<bool, AuthError> {
        let mut db = self.db.lock().unwrap();
        let Some(user) = db.users.iter_mut().find(|u| u.id == id) else {
            return Ok(false);
        };
        if user.account_state != AccountState::Suspended {
            return Ok(false);
        }
        user.account_state = if user.email_verified_at.is_some() {
            AccountState::Active
        } else {
            AccountState::PendingEmailVerification
        };
        db.audits.push(AuditEvent::success(
            Some(actor),
            "user.restored",
            "user",
            id.0.to_string(),
        ));
        Ok(true)
    }
    async fn link_identity(
        &self,
        user_id: UserId,
        provider: AuthenticationProvider,
        subject: &str,
        hash: Option<&str>,
    ) -> Result<(), AuthError> {
        let mut db = self.db.lock().unwrap();
        if let Some(i) = db
            .identities
            .iter_mut()
            .find(|i| i.user_id == user_id && i.provider == provider)
        {
            i.provider_subject = subject.to_string();
            i.credential_hash = hash.map(str::to_string);
        } else {
            let id = db.next_id;
            db.identities.push(IdentityRecord {
                id,
                user_id,
                provider,
                provider_subject: subject.to_string(),
                credential_hash: hash.map(str::to_string),
            });
        }
        Ok(())
    }
    async fn find_identity(
        &self,
        provider: AuthenticationProvider,
        subject: &str,
    ) -> Result<Option<IdentityRecord>, AuthError> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .identities
            .iter()
            .find(|i| i.provider == provider && i.provider_subject == subject)
            .cloned())
    }
    async fn roles(&self, id: UserId) -> Result<Vec<Role>, AuthError> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .users
            .iter()
            .find(|u| u.id == id)
            .map(|u| u.roles.clone())
            .unwrap_or_default())
    }
    async fn count_admins(&self) -> Result<i64, AuthError> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .users
            .iter()
            .filter(|u| u.roles.contains(&Role::Admin))
            .count() as i64)
    }
    async fn grant_role(&self, id: UserId, role: Role, _by: UserId) -> Result<(), AuthError> {
        if let Some(u) = self
            .db
            .lock()
            .unwrap()
            .users
            .iter_mut()
            .find(|u| u.id == id)
            && !u.roles.contains(&role)
        {
            u.roles.push(role);
        }
        Ok(())
    }
    /// Mirrors the SQL repository: the last-admin refusal and the delete are
    /// one indivisible step (here, one `Mutex` guard) — the guard is the
    /// repository's job, not the service's.
    async fn revoke_role_guarded(&self, id: UserId, role: Role) -> Result<bool, AuthError> {
        let mut db = self.db.lock().unwrap();
        let admins: Vec<UserId> = db
            .users
            .iter()
            .filter(|u| u.roles.contains(&Role::Admin))
            .map(|u| u.id)
            .collect();
        if role == Role::Admin && admins.len() <= 1 && admins.contains(&id) {
            return Err(AuthError::RefuseAdminSelfRevoke);
        }
        if let Some(u) = db.users.iter_mut().find(|u| u.id == id) {
            let before = u.roles.len();
            u.roles.retain(|r| *r != role);
            Ok(u.roles.len() < before)
        } else {
            Ok(false)
        }
    }
    async fn list_users(&self) -> Result<Vec<User>, AuthError> {
        Ok(self.db.lock().unwrap().users.clone())
    }
    async fn search_users(&self, search: UserSearch<'_>) -> Result<Vec<User>, AuthError> {
        let needle = search.query.map(str::to_lowercase);
        let mut users: Vec<User> = self
            .db
            .lock()
            .unwrap()
            .users
            .iter()
            .filter(|u| match &needle {
                Some(n) => {
                    u.email.as_str().to_lowercase().contains(n)
                        || u.display_name
                            .as_deref()
                            .is_some_and(|d| d.to_lowercase().contains(n))
                }
                None => true,
            })
            .filter(|u| search.after_id.is_none_or(|after| u.id.0 < after))
            .cloned()
            .collect();
        users.sort_by_key(|u| std::cmp::Reverse(u.id.0));
        users.truncate(search.limit.clamp(1, 200) as usize);
        Ok(users)
    }
    async fn labels_for(&self, ids: &[i64]) -> Result<HashMap<i64, String>, AuthError> {
        Ok(self
            .db
            .lock()
            .unwrap()
            .users
            .iter()
            .filter(|u| ids.contains(&u.id.0))
            .map(|u| {
                (
                    u.id.0,
                    u.display_name
                        .clone()
                        .unwrap_or_else(|| u.email.as_str().to_string()),
                )
            })
            .collect())
    }
    async fn activity_for(&self, ids: &[i64]) -> Result<HashMap<i64, UserActivity>, AuthError> {
        Ok(ids
            .iter()
            .map(|id| (*id, UserActivity::default()))
            .collect())
    }
}

// --- SessionStore ---
#[async_trait]
impl SessionStore for FakeRepo {
    async fn create(
        &self,
        user_id: UserId,
        raw: &SessionId,
        csrf: &CsrfToken,
        now: DateTime<Utc>,
    ) -> Result<(), AuthError> {
        self.db.lock().unwrap().sessions.push((
            raw.to_hex(),
            Session {
                user_id,
                csrf_token: csrf.clone(),
                created_at: now,
                last_seen_at: now,
                expires_at: now + chrono::Duration::days(90),
                revoked_at: None,
            },
        ));
        Ok(())
    }
    async fn resolve(
        &self,
        raw: &SessionId,
        now: DateTime<Utc>,
    ) -> Result<Option<Session>, AuthError> {
        let mut db = self.db.lock().unwrap();
        let key = raw.to_hex();
        if let Some((_, s)) = db.sessions.iter_mut().find(|(k, _)| *k == key) {
            if s.revoked_at.is_some()
                || now > s.expires_at
                || now - s.last_seen_at > chrono::Duration::days(30)
            {
                return Ok(None);
            }
            s.last_seen_at = now;
            return Ok(Some(s.clone()));
        }
        Ok(None)
    }
    async fn revoke(&self, raw: &SessionId) -> Result<(), AuthError> {
        let mut db = self.db.lock().unwrap();
        let key = raw.to_hex();
        if let Some((_, s)) = db.sessions.iter_mut().find(|(k, _)| *k == key) {
            s.revoked_at = Some(Utc::now());
        }
        Ok(())
    }
    async fn revoke_all_for_user_except(
        &self,
        user_id: UserId,
        keep: &SessionId,
    ) -> Result<(), AuthError> {
        let mut db = self.db.lock().unwrap();
        let keep = keep.to_hex();
        for (k, s) in db.sessions.iter_mut() {
            if s.user_id == user_id && *k != keep {
                s.revoked_at = Some(Utc::now());
            }
        }
        Ok(())
    }
    async fn revoke_all_for_user(&self, user_id: UserId) -> Result<(), AuthError> {
        let mut db = self.db.lock().unwrap();
        for (_, s) in db.sessions.iter_mut() {
            if s.user_id == user_id {
                s.revoked_at = Some(Utc::now());
            }
        }
        Ok(())
    }
}

// --- TokenStore ---
#[async_trait]
impl TokenStore for FakeRepo {
    async fn find_reset(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError> {
        let db = self.db.lock().unwrap();
        let key = raw.to_hex();
        Ok(db
            .reset
            .iter()
            .find(|(candidate, _, expires_at, used)| {
                *candidate == key && !*used && *expires_at > now
            })
            .map(|(_, user_id, _, _)| *user_id))
    }
    async fn issue_verification(
        &self,
        user_id: UserId,
        email: &str,
        raw: &VerificationToken,
        _now: DateTime<Utc>,
        expected_state: AccountState,
    ) -> Result<bool, AuthError> {
        if !matches!(
            expected_state,
            AccountState::PendingEmailVerification | AccountState::Active
        ) {
            return Ok(false);
        }
        let mut db = self.db.lock().unwrap();
        if !db
            .users
            .iter()
            .any(|user| user.id == user_id && user.account_state == expected_state)
        {
            return Ok(false);
        }
        db.verification
            .push((raw.to_hex(), user_id, email.to_string(), false));
        Ok(true)
    }
    async fn consume_verification(
        &self,
        raw: &VerificationToken,
        _now: DateTime<Utc>,
    ) -> Result<Option<(UserId, String)>, AuthError> {
        let mut db = self.db.lock().unwrap();
        let key = raw.to_hex();
        if let Some((_, u, e, used)) = db
            .verification
            .iter_mut()
            .find(|(k, _, _, used)| *k == key && !*used)
        {
            *used = true;
            return Ok(Some((*u, e.clone())));
        }
        Ok(None)
    }
    async fn find_verification(
        &self,
        raw: &VerificationToken,
        _now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError> {
        let db = self.db.lock().unwrap();
        let key = raw.to_hex();
        Ok(db
            .verification
            .iter()
            .find(|(k, _, _, used)| *k == key && !*used)
            .map(|(_, user_id, _, _)| *user_id))
    }
    async fn issue_reset(
        &self,
        user_id: UserId,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<bool, AuthError> {
        let mut db = self.db.lock().unwrap();
        if !db
            .users
            .iter()
            .any(|user| user.id == user_id && user.account_state.can_log_in())
        {
            return Ok(false);
        }
        db.reset.push((
            raw.to_hex(),
            user_id,
            now + chrono::Duration::hours(1),
            false,
        ));
        Ok(true)
    }
    async fn consume_reset(
        &self,
        raw: &VerificationToken,
        now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError> {
        let mut db = self.db.lock().unwrap();
        let key = raw.to_hex();
        if let Some((_, u, _, used)) = db
            .reset
            .iter_mut()
            .find(|(k, _, expires_at, used)| *k == key && !*used && *expires_at > now)
        {
            *used = true;
            return Ok(Some(*u));
        }
        Ok(None)
    }
}

// --- PasswordHasher ---
#[derive(Clone)]
struct FakeHasher {
    db: Arc<Mutex<FakeDb>>,
}
#[async_trait]
impl PasswordHasher for FakeHasher {
    async fn hash(&self, pw: &Password) -> Result<String, AuthError> {
        if self.db.lock().unwrap().hash_broken {
            return Err(AuthError::Internal);
        }
        Ok(format!("h:{}", pw.as_str()))
    }
    async fn verify(&self, pw: &Password, hash: &str) -> Result<bool, AuthError> {
        Ok(hash == format!("h:{}", pw.as_str()))
    }
}

// --- TokenGenerator (deterministic) ---
#[derive(Clone)]
struct FakeTokens {
    n: Arc<Mutex<u64>>,
}
impl TokenGenerator for FakeTokens {
    fn generate(&self) -> [u8; 32] {
        let mut n = self.n.lock().unwrap();
        *n += 1;
        [*n as u8; 32]
    }
}

// --- Clock (mutable) ---
#[derive(Clone)]
struct FakeClock {
    t: Arc<Mutex<DateTime<Utc>>>,
}
impl FakeClock {
    fn new(t: DateTime<Utc>) -> Self {
        Self {
            t: Arc::new(Mutex::new(t)),
        }
    }
}
impl Clock for FakeClock {
    fn now(&self) -> DateTime<Utc> {
        *self.t.lock().unwrap()
    }
}

// --- Atomic auth outbox + post-commit dispatcher ---
#[derive(Clone)]
struct FakeQueue {
    db: Arc<Mutex<FakeDb>>,
}
#[async_trait]
impl AuthMailDispatcher for FakeQueue {
    async fn dispatch(&self, mail: AdmittedAuthMail) -> Result<(), AuthError> {
        let mut db = self.db.lock().unwrap();
        if db.dispatch_broken {
            return Err(AuthError::Unavailable);
        }
        if !db.dispatched.contains(&mail.job_id) {
            db.emails.push(mail.message);
            db.dispatched.push(mail.job_id);
        }
        Ok(())
    }
}

#[async_trait]
impl AuthOutbox for FakeRepo {
    async fn register(
        &self,
        new: NewAccount<'_>,
        token: &VerificationToken,
        at: DateTime<Utc>,
        mut message: EmailMessage,
        _terms: Option<&bikesnest_application::TermsAcceptance>,
    ) -> Result<Option<AdmittedAuthMail>, AuthError> {
        let mut db = self.db.lock().unwrap();
        if db.queue_broken {
            return Err(AuthError::Unavailable);
        }
        // Mirrors `SqlxAuthOutbox::register`: an existing address is neutral
        // unless it is still pending, in which case the latest submission
        // replaces the credential and display name, revokes every session,
        // retires every earlier verification link and queues a fresh one.
        if let Some((id, state, locale)) = db
            .users
            .iter()
            .find(|u| u.email == *new.email)
            .map(|u| (u.id, u.account_state, u.locale))
        {
            if state != AccountState::PendingEmailVerification {
                return Ok(None);
            }
            let Some(identity) = db.identities.iter_mut().find(|identity| {
                identity.user_id == id && identity.provider == AuthenticationProvider::Password
            }) else {
                return Err(AuthError::Internal);
            };
            identity.credential_hash = Some(new.password_hash.into());
            if let Some(user) = db.users.iter_mut().find(|u| u.id == id) {
                user.display_name = new.display_name.map(str::to_string);
            }
            for (_, session) in &mut db.sessions {
                if session.user_id == id {
                    session.revoked_at = Some(at);
                }
            }
            for (_, user_id, _, used) in &mut db.verification {
                if *user_id == id {
                    *used = true;
                }
            }
            db.verification
                .push((token.to_hex(), id, new.email.as_str().into(), false));
            message.account_id = id.0;
            message.locale = locale;
            db.next_id += 1;
            let mail = AdmittedAuthMail {
                job_id: db.next_id,
                message,
            };
            db.outbox.push(mail.clone());
            return Ok(Some(mail));
        }
        db.next_id += 1;
        let id = UserId(db.next_id);
        let mut user = User::new(id, new.email.clone(), new.display_name.map(str::to_string));
        user.account_state = new.state;
        user.locale = new.locale;
        db.users.push(user);
        db.identities.push(IdentityRecord {
            id: id.0,
            user_id: id,
            provider: AuthenticationProvider::Password,
            provider_subject: new.email.as_str().into(),
            credential_hash: Some(new.password_hash.into()),
        });
        db.verification
            .push((token.to_hex(), id, new.email.as_str().into(), false));
        db.audits.push(AuditEvent::success(
            Some(id),
            "auth.register",
            "user",
            id.0.to_string(),
        ));
        message.account_id = id.0;
        let mail = AdmittedAuthMail {
            job_id: id.0,
            message,
        };
        db.outbox.push(mail.clone());
        Ok(Some(mail))
    }

    async fn issue_verification(
        &self,
        user_id: UserId,
        email: &str,
        token: &VerificationToken,
        _at: DateTime<Utc>,
        expected_state: AccountState,
        message: EmailMessage,
        audit_action: Option<&'static str>,
    ) -> Result<Option<AdmittedAuthMail>, AuthError> {
        let mut db = self.db.lock().unwrap();
        if db.queue_broken {
            return Err(AuthError::Unavailable);
        }
        if !db
            .users
            .iter()
            .any(|u| u.id == user_id && u.account_state == expected_state)
        {
            return Ok(None);
        }
        db.verification
            .push((token.to_hex(), user_id, email.into(), false));
        if let Some(action) = audit_action {
            db.audits.push(AuditEvent::success(
                Some(user_id),
                action,
                "user",
                user_id.0.to_string(),
            ));
        }
        db.next_id += 1;
        let mail = AdmittedAuthMail {
            job_id: db.next_id,
            message,
        };
        db.outbox.push(mail.clone());
        Ok(Some(mail))
    }

    async fn issue_reset(
        &self,
        user_id: UserId,
        token: &VerificationToken,
        at: DateTime<Utc>,
        message: EmailMessage,
    ) -> Result<Option<AdmittedAuthMail>, AuthError> {
        let mut db = self.db.lock().unwrap();
        if db.queue_broken {
            return Err(AuthError::Unavailable);
        }
        if !db
            .users
            .iter()
            .any(|u| u.id == user_id && u.account_state.can_log_in())
        {
            return Ok(None);
        }
        db.reset.push((
            token.to_hex(),
            user_id,
            at + chrono::Duration::hours(1),
            false,
        ));
        db.next_id += 1;
        let mail = AdmittedAuthMail {
            job_id: db.next_id,
            message,
        };
        db.outbox.push(mail.clone());
        Ok(Some(mail))
    }

    async fn confirm_email(
        &self,
        token: &VerificationToken,
        at: DateTime<Utc>,
        old_address_notice: EmailMessage,
        proven_credential_hash: Option<&str>,
    ) -> Result<Option<EmailVerificationOutcome>, AuthError> {
        let mut db = self.db.lock().unwrap();
        let key = token.to_hex();
        let Some(position) = db
            .verification
            .iter()
            .position(|(candidate, _, _, used)| *candidate == key && !*used)
        else {
            return Ok(None);
        };
        let (_, id, email, _) = db.verification[position].clone();
        let Some(user_position) = db.users.iter().position(|user| user.id == id) else {
            return Ok(None);
        };
        if !matches!(
            db.users[user_position].account_state,
            AccountState::PendingEmailVerification | AccountState::Active
        ) {
            return Ok(None);
        }
        let parsed_email = UserEmail::parse(&email).map_err(|_| AuthError::Internal)?;
        let changed = db.users[user_position].email != parsed_email;
        let pending =
            db.users[user_position].account_state == AccountState::PendingEmailVerification;
        if changed && pending {
            return Ok(None);
        }
        // Activation only for the credential the caller proved, as the SQL
        // adapter re-checks under the account lock.
        if pending {
            let stored = db
                .identities
                .iter()
                .find(|identity| {
                    identity.user_id == id && identity.provider == AuthenticationProvider::Password
                })
                .and_then(|identity| identity.credential_hash.as_deref());
            if proven_credential_hash.is_none() || stored != proven_credential_hash {
                return Ok(None);
            }
        }
        db.users[user_position].email = parsed_email;
        db.users[user_position].email_verified_at = Some(at);
        db.users[user_position].account_state = AccountState::Active;
        for identity in &mut db.identities {
            if identity.user_id == id && identity.provider == AuthenticationProvider::Password {
                identity.provider_subject = email.clone();
            }
        }
        // An address change, or the first verification of a pending account,
        // ends every session; every outstanding link is retired.
        if changed || pending {
            for (_, session) in &mut db.sessions {
                if session.user_id == id {
                    session.revoked_at = Some(at);
                }
            }
        }
        for (_, user_id, _, used) in &mut db.verification {
            if *user_id == id {
                *used = true;
            }
        }
        db.audits.push(AuditEvent::success(
            Some(id),
            if changed {
                "auth.email_changed"
            } else {
                "auth.email_verified"
            },
            "user",
            id.0.to_string(),
        ));
        let mail = if changed {
            db.next_id += 1;
            let admitted = AdmittedAuthMail {
                job_id: db.next_id,
                message: old_address_notice,
            };
            db.outbox.push(admitted.clone());
            Some(admitted)
        } else {
            None
        };
        Ok(Some(EmailVerificationOutcome {
            user_id: id,
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
        let mut db = self.db.lock().unwrap();
        let key = token.to_hex();
        let Some((_, user_id, _, _)) = db
            .reset
            .iter()
            .find(|(candidate, _, expires_at, used)| {
                *candidate == key && !*used && *expires_at > at
            })
            .cloned()
        else {
            return Ok(None);
        };
        let Some(identity) = db.identities.iter_mut().find(|identity| {
            identity.user_id == user_id && identity.provider == AuthenticationProvider::Password
        }) else {
            return Err(AuthError::Internal);
        };
        identity.credential_hash = Some(password_hash.to_string());
        for (_, session) in &mut db.sessions {
            if session.user_id == user_id {
                session.revoked_at = Some(at);
            }
        }
        for (_, reset_user_id, _, used) in &mut db.reset {
            if *reset_user_id == user_id {
                *used = true;
            }
        }
        db.audits.push(AuditEvent::success(
            Some(user_id),
            "auth.password_changed",
            "user",
            user_id.0.to_string(),
        ));
        db.next_id += 1;
        let mail = AdmittedAuthMail {
            job_id: db.next_id,
            message: notice,
        };
        db.outbox.push(mail.clone());
        Ok(Some(mail))
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
        let mut db = self.db.lock().unwrap();
        let Some(identity) = db.identities.iter_mut().find(|identity| {
            identity.user_id == user_id
                && identity.provider == AuthenticationProvider::Password
                && identity.credential_hash.as_deref() == Some(expected_hash)
        }) else {
            return Ok(None);
        };
        identity.credential_hash = Some(password_hash.to_string());
        for (raw, session) in &mut db.sessions {
            if session.user_id == user_id && raw != &current_session.to_hex() {
                session.revoked_at = Some(at);
            }
        }
        db.audits.push(AuditEvent::success(
            Some(user_id),
            "auth.password_changed",
            "user",
            user_id.0.to_string(),
        ));
        db.next_id += 1;
        let mail = AdmittedAuthMail {
            job_id: db.next_id,
            message: notice,
        };
        db.outbox.push(mail.clone());
        Ok(Some(mail))
    }
}

// --- OAuthProvider ---
#[derive(Clone)]
struct FakeOauth {
    email: String,
    subject: String,
}
#[async_trait]
impl OAuthProvider for FakeOauth {
    fn authorize_url(&self, state: &str) -> String {
        format!("/oauth?state={state}")
    }
    async fn exchange(&self, _code: &str) -> Result<ProviderIdentity, AuthError> {
        Ok(ProviderIdentity {
            provider: AuthenticationProvider::Google,
            subject: self.subject.clone(),
            email: UserEmail::parse(&self.email).unwrap(),
            email_verified: true,
        })
    }
}

// --- RateLimiter (sliding window, shares a store) ---
#[derive(Clone)]
struct FakeRate {
    buckets: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
}
#[async_trait]
impl RateLimiter for FakeRate {
    async fn check(&self, key: &str, limit: u32, window: Duration) -> Result<bool, RateLimitError> {
        let now = Instant::now();
        let mut buckets = self
            .buckets
            .lock()
            .map_err(|_| RateLimitError::Unavailable)?;
        let q = buckets.entry(key.to_string()).or_default();
        while let Some(&front) = q.first() {
            if now.duration_since(front) >= window {
                q.remove(0);
            } else {
                break;
            }
        }
        if q.len() >= limit as usize {
            return Ok(false);
        }
        q.push(now);
        Ok(true)
    }
}

// --- AuditLog ---
#[derive(Clone)]
struct FakeAudit {
    db: Arc<Mutex<FakeDb>>,
}
#[async_trait]
impl AuditLog for FakeAudit {
    async fn record(&self, event: AuditEvent) -> Result<(), bikesnest_application::AuditError> {
        self.db.lock().unwrap().audits.push(event);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

const BASE: &str = "http://localhost:8080";

fn make_service(db: Arc<Mutex<FakeDb>>) -> AuthService {
    let repo = FakeRepo::new(db.clone());
    AuthService::new(
        Box::new(repo.clone()),
        Box::new(repo.clone()),
        Box::new(repo.clone()),
        Box::new(FakeHasher { db: db.clone() }),
        Box::new(FakeTokens {
            n: Arc::new(Mutex::new(0)),
        }),
        Box::new(FakeClock::new(Utc::now())),
        Box::new(repo.clone()),
        Box::new(FakeQueue { db: db.clone() }),
        Box::new(FakeOauth {
            email: "oauth.user@example.com".into(),
            subject: "sub-1".into(),
        }),
        Box::new(FakeRate {
            buckets: Arc::new(Mutex::new(HashMap::new())),
        }),
        Box::new(FakeAudit { db }),
        BASE.to_string(),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn login_success_creates_session_and_csrf() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "a@example.com", "correct-horse");
    let auth = make_service(db);
    let outcome = auth
        .login("1.2.3.4", "a@example.com", "correct-horse")
        .await;
    assert!(outcome.is_ok(), "login should succeed");
    let LoginOutcome {
        session,
        csrf,
        user,
    } = outcome.unwrap();
    assert_eq!(user.id, UserId(1));
    assert!(!session.to_hex().is_empty());
    assert!(!csrf.to_base64url().is_empty());
    let resolved = auth.resolve_session(&session).await.unwrap();
    assert_eq!(resolved.unwrap().user.id, UserId(1));
}

#[tokio::test]
async fn login_bad_credentials_and_suspended_share_one_generic_error() {
    // Wrong password → generic.
    {
        let db = Arc::new(Mutex::new(FakeDb::default()));
        seed_active_user(&db, "a@example.com", "correct-horse");
        let auth = make_service(db);
        let err = auth.login("1.1.1.1", "a@example.com", "wrong").await;
        assert_eq!(err.unwrap_err(), AuthError::InvalidCredentials);
    }
    // Suspended, RIGHT password → same generic error (no leak).
    {
        let db = Arc::new(Mutex::new(FakeDb::default()));
        let id = seed_active_user(&db, "a@example.com", "correct-horse");
        db.lock()
            .unwrap()
            .users
            .iter_mut()
            .find(|u| u.id == id)
            .unwrap()
            .account_state = AccountState::Suspended;
        let auth = make_service(db);
        let err = auth
            .login("1.1.1.1", "a@example.com", "correct-horse")
            .await;
        assert_eq!(err.unwrap_err(), AuthError::InvalidCredentials);
    }
    // Unknown email → generic.
    {
        let db = Arc::new(Mutex::new(FakeDb::default()));
        let auth = make_service(db);
        let err = auth.login("1.1.1.1", "nobody@example.com", "x").await;
        assert_eq!(err.unwrap_err(), AuthError::InvalidCredentials);
    }
}

#[tokio::test]
async fn rate_limit_blocks_login_after_window_hits() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "a@example.com", "correct-horse");
    let auth = make_service(db);
    // 5 attempts allowed, the 6th hits the ip+email limit (per-ip limit is 10).
    for _ in 0..5 {
        let r = auth.login("1.1.1.1", "a@example.com", "wrong").await;
        assert_eq!(r.unwrap_err(), AuthError::InvalidCredentials);
    }
    let sixth = auth.login("1.1.1.1", "a@example.com", "wrong").await;
    assert_eq!(sixth.unwrap_err(), AuthError::RateLimited);
}

#[tokio::test]
async fn register_email_taken_is_leak_free() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "taken@example.com", "x");
    let auth = make_service(db.clone());
    let result = auth
        .register(
            "1.1.1.1",
            "taken@example.com",
            None,
            "password123",
            LocaleCode::PtBr,
        )
        .await;
    assert!(result.is_ok(), "taken email returns Ok (no-existence-leak)");
    // No verification email was sent.
    assert!(db.lock().unwrap().emails.is_empty());
    // No new user was created.
    assert_eq!(db.lock().unwrap().users.len(), 1);
}

/// The i18n rule applied to mail: `register` hands the queue a *description*
/// of the message — kind, recipient, link and the locale the signup happened
/// in — and no subject or body. Rendering happens in the layer that owns the
/// catalog, so a pt-BR signup cannot receive an English email.
#[tokio::test]
async fn register_queues_one_message_carrying_the_signup_locale() {
    for locale in [LocaleCode::PtBr, LocaleCode::En] {
        let db = Arc::new(Mutex::new(FakeDb::default()));
        let auth = make_service(db.clone());
        auth.register("1.1.1.1", "a@example.com", None, "password123", locale)
            .await
            .unwrap();

        let queued = db.lock().unwrap().emails.clone();
        assert_eq!(queued.len(), 1, "exactly one message per registration");
        assert_eq!(queued[0].to, "a@example.com");
        assert_eq!(queued[0].locale, locale);
        assert!(
            matches!(queued[0].kind, EmailKind::VerifyEmail { .. }),
            "{:?}",
            queued[0].kind
        );
        assert!(
            queued[0]
                .kind
                .action_link()
                .contains("/verify-email?token=")
        );

        // The locale is also on the account, so the *next* message — a resend
        // or a password reset, both sent with no request in scope — finds it.
        assert_eq!(db.lock().unwrap().users[0].locale, locale);
    }
}

/// The registration succeeds without anything being delivered: the service
/// holds an `EmailQueue`, not a provider, so a slow or broken relay is no
/// longer on the request path at all. (The queue double here records and never
/// sends; `register` still returns `Ok`.)
#[tokio::test]
async fn register_does_not_wait_on_delivery() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let auth = make_service(db.clone());
    assert!(
        auth.register(
            "1.1.1.1",
            "a@example.com",
            None,
            "password123",
            LocaleCode::PtBr
        )
        .await
        .is_ok()
    );
    // Handed off, not sent.
    assert_eq!(db.lock().unwrap().emails.len(), 1);
    assert_eq!(
        db.lock().unwrap().users[0].account_state,
        AccountState::PendingEmailVerification
    );
}

/// Admission failure rolls back the whole registration aggregate.
#[tokio::test]
async fn a_failing_queue_fails_the_registration() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    db.lock().unwrap().queue_broken = true;
    let auth = make_service(db.clone());

    let err = auth
        .register(
            "1.1.1.1",
            "a@example.com",
            None,
            "password123",
            LocaleCode::PtBr,
        )
        .await
        .unwrap_err();

    assert_eq!(err, AuthError::Unavailable);
    assert!(db.lock().unwrap().emails.is_empty());
    assert!(db.lock().unwrap().users.is_empty());
    assert!(db.lock().unwrap().verification.is_empty());
    assert!(db.lock().unwrap().outbox.is_empty());
    assert!(db.lock().unwrap().audits.is_empty());
}

/// A resend, a reset and an email change all happen without a page being
/// rendered, so their locale can only come from the stored one.
#[tokio::test]
async fn later_messages_use_the_stored_account_locale() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let id = seed_active_user(&db, "a@example.com", "correct-horse");
    db.lock()
        .unwrap()
        .users
        .iter_mut()
        .find(|u| u.id == id)
        .unwrap()
        .locale = LocaleCode::En;
    let auth = make_service(db.clone());
    let email = UserEmail::parse("a@example.com").unwrap();

    db.lock().unwrap().users[0].account_state = AccountState::PendingEmailVerification;
    auth.resend_verification("1.1.1.1", &email).await.unwrap();
    db.lock().unwrap().users[0].account_state = AccountState::Active;
    auth.request_password_reset("1.1.1.1", &email)
        .await
        .unwrap();
    auth.change_email(
        "1.1.1.1",
        id,
        "correct-horse",
        &UserEmail::parse("new@example.com").unwrap(),
    )
    .await
    .unwrap();

    let queued = db.lock().unwrap().emails.clone();
    assert_eq!(queued.len(), 3);
    assert!(
        queued.iter().all(|m| m.locale == LocaleCode::En),
        "every message follows the account language: {queued:?}"
    );
    assert!(matches!(queued[0].kind, EmailKind::VerifyEmail { .. }));
    assert!(matches!(queued[1].kind, EmailKind::ResetPassword { .. }));
    // An email change is its own message kind (and goes to the NEW address).
    assert!(matches!(
        queued[2].kind,
        EmailKind::ConfirmEmailChange { .. }
    ));
    assert_eq!(queued[2].to, "new@example.com");
}

/// The language toggle persists for a signed-in user, so mail sent after it
/// switches too.
#[tokio::test]
async fn set_locale_updates_the_account() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let id = seed_active_user(&db, "a@example.com", "correct-horse");
    let auth = make_service(db.clone());
    assert_eq!(db.lock().unwrap().users[0].locale, LocaleCode::PtBr);

    auth.set_locale(id, LocaleCode::En).await.unwrap();
    assert_eq!(db.lock().unwrap().users[0].locale, LocaleCode::En);

    let email = UserEmail::parse("a@example.com").unwrap();
    auth.request_password_reset("1.1.1.1", &email)
        .await
        .unwrap();
    assert_eq!(db.lock().unwrap().emails[0].locale, LocaleCode::En);
}

#[tokio::test]
async fn register_rejects_weak_password() {
    let auth = make_service(Arc::new(Mutex::new(FakeDb::default())));
    let err = auth
        .register("1.1.1.1", "a@example.com", None, "short", LocaleCode::PtBr)
        .await;
    assert_eq!(err.unwrap_err(), AuthError::WeakPassword);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn verify_email_consumes_token_single_use() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let auth = make_service(db.clone());
    auth.register(
        "1.1.1.1",
        "a@example.com",
        None,
        "password123",
        LocaleCode::PtBr,
    )
    .await
    .unwrap();
    let user_id = db.lock().unwrap().users[0].id;
    assert!(db.lock().unwrap().users[0].email_verified_at.is_none());

    let token = find_token(&db, "/verify-email");
    // The link alone does not activate a pending account: the password does.
    assert_eq!(
        auth.verify_email("1.1.1.1", &token, None).await,
        Err(AuthError::InvalidCredentials)
    );
    assert_eq!(
        auth.verify_email("1.1.1.1", &token, Some("wrong-password"))
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert!(db.lock().unwrap().users[0].email_verified_at.is_none());
    assert!(
        auth.verify_email("1.1.1.1", &token, Some("password123"))
            .await
            .is_ok()
    );

    let users = db.lock().unwrap();
    let u = users.users.iter().find(|u| u.id == user_id).unwrap();
    assert!(u.email_verified_at.is_some());
    assert_eq!(u.account_state, AccountState::Active);
    drop(users);

    // Second use of the same token fails (single-use).
    assert!(matches!(
        auth.verify_email("1.1.1.1", &token, Some("password123"))
            .await,
        Err(AuthError::TokenInvalid)
    ));
}

#[tokio::test]
async fn suspended_or_deleted_accounts_cannot_verify_and_resend_is_neutral() {
    for blocked_state in [AccountState::Suspended, AccountState::Deleted] {
        let db = Arc::new(Mutex::new(FakeDb::default()));
        let auth = make_service(db.clone());
        auth.register(
            "1.1.1.1",
            "blocked@example.com",
            None,
            "password123",
            LocaleCode::PtBr,
        )
        .await
        .unwrap();
        let token = find_token(&db, "/verify-email");
        db.lock().unwrap().users[0].account_state = blocked_state;

        assert_eq!(
            auth.verify_email("1.1.1.1", &token, Some("password123"))
                .await,
            Err(AuthError::TokenInvalid)
        );
        let emails_before = db.lock().unwrap().emails.len();
        auth.resend_verification("1.1.1.1", &UserEmail::parse("blocked@example.com").unwrap())
            .await
            .unwrap();
        assert_eq!(db.lock().unwrap().emails.len(), emails_before);
        assert_eq!(db.lock().unwrap().users[0].account_state, blocked_state);
    }
}

#[tokio::test]
async fn suspension_revokes_old_tokens_even_after_restore() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let auth = make_service(db.clone());
    auth.register(
        "1.1.1.1",
        "pending@example.com",
        None,
        "password123",
        LocaleCode::PtBr,
    )
    .await
    .unwrap();
    let target = db.lock().unwrap().users[0].id;
    let verification_token = find_token(&db, "/verify-email");
    auth.request_password_reset("1.1.1.1", &UserEmail::parse("pending@example.com").unwrap())
        .await
        .unwrap();
    let reset_token = find_token(&db, "/password-reset/new");
    let admin_id = seed_user_with_roles(&db, "admin@example.com", vec![Role::Admin]);
    let admin = actor_for(&db, admin_id);

    auth.suspend_user(&admin, target).await.unwrap();
    auth.restore_user(&admin, target).await.unwrap();

    assert_eq!(
        auth.verify_email("1.1.1.1", &verification_token, Some("password123"))
            .await,
        Err(AuthError::TokenInvalid)
    );
    assert_eq!(
        auth.reset_password(&reset_token, "replacement-password")
            .await,
        Err(AuthError::TokenInvalid)
    );
    assert_eq!(
        db.lock().unwrap().users[0].account_state,
        AccountState::PendingEmailVerification
    );
    assert!(db.lock().unwrap().users[0].email_verified_at.is_none());
}

#[tokio::test]
async fn administrator_account_transitions_preserve_deleted_and_verification_states() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let auth = make_service(db.clone());
    let admin_id = seed_user_with_roles(&db, "state-admin@example.com", vec![Role::Admin]);
    let admin = actor_for(&db, admin_id);
    let verified = seed_active_user(&db, "state-verified@example.com", "password");
    let pending = seed_active_user(&db, "state-pending@example.com", "password");
    let deleted = seed_active_user(&db, "state-deleted@example.com", "password");
    {
        let mut locked = db.lock().unwrap();
        locked
            .users
            .iter_mut()
            .find(|user| user.id == verified)
            .unwrap()
            .email_verified_at = Some(Utc::now());
        locked
            .users
            .iter_mut()
            .find(|user| user.id == deleted)
            .unwrap()
            .account_state = AccountState::Deleted;
    }

    auth.suspend_user(&admin, verified).await.unwrap();
    auth.suspend_user(&admin, pending).await.unwrap();
    // A deleted account cannot be suspended or restored, and the refusal is
    // reported rather than passed off as success.
    assert_eq!(
        auth.suspend_user(&admin, deleted).await,
        Err(AuthError::StateUnchanged)
    );
    auth.restore_user(&admin, verified).await.unwrap();
    auth.restore_user(&admin, pending).await.unwrap();
    assert_eq!(
        auth.restore_user(&admin, deleted).await,
        Err(AuthError::StateUnchanged)
    );

    let locked = db.lock().unwrap();
    assert_eq!(
        locked
            .users
            .iter()
            .find(|u| u.id == verified)
            .unwrap()
            .account_state,
        AccountState::Active
    );
    assert_eq!(
        locked
            .users
            .iter()
            .find(|u| u.id == pending)
            .unwrap()
            .account_state,
        AccountState::PendingEmailVerification
    );
    assert_eq!(
        locked
            .users
            .iter()
            .find(|u| u.id == deleted)
            .unwrap()
            .account_state,
        AccountState::Deleted
    );
    assert_eq!(
        locked
            .audits
            .iter()
            .filter(|event| event.action == "user.suspended")
            .count(),
        2
    );
    assert_eq!(
        locked
            .audits
            .iter()
            .filter(|event| event.action == "user.restored")
            .count(),
        2
    );
}

#[tokio::test]
async fn suspend_refuses_self_and_the_last_active_admin() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let auth = make_service(db.clone());
    let admin_id = seed_user_with_roles(&db, "only-admin@example.com", vec![Role::Admin]);
    let other_id = seed_user_with_roles(&db, "other-admin@example.com", vec![Role::Admin]);
    let admin = actor_for(&db, admin_id);

    assert_eq!(
        auth.suspend_user(&admin, admin_id).await,
        Err(AuthError::SelfSuspension),
        "an admin cannot suspend their own account"
    );
    // Suspending the other admin leaves `admin` as the only active one, so a
    // second admin acting on `admin` is refused, and the refusal is distinct.
    auth.suspend_user(&admin, other_id).await.unwrap();
    let suspended_actor = actor_for(&db, other_id);
    assert_eq!(
        auth.suspend_user(&suspended_actor, admin_id).await,
        Err(AuthError::LastActiveAdmin)
    );
    assert_eq!(
        auth.suspend_user(&admin, other_id).await,
        Err(AuthError::StateUnchanged),
        "suspending an already suspended account is not reported as success"
    );
    let locked = db.lock().unwrap();
    let state = |id| {
        locked
            .users
            .iter()
            .find(|u| u.id == id)
            .unwrap()
            .account_state
    };
    assert_eq!(state(admin_id), AccountState::Active);
    assert_eq!(state(other_id), AccountState::Suspended);
}

#[tokio::test]
async fn password_reentry_is_throttled_per_account_across_settings_forms() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "reauth@example.com", "correct-horse");
    let auth = make_service(db.clone());
    let session = auth
        .login("1.1.1.1", "reauth@example.com", "correct-horse")
        .await
        .unwrap()
        .session;
    let new_email = UserEmail::parse("elsewhere@example.com").unwrap();
    // Five wrong guesses split across both forms and two IPs spend the
    // per-account budget...
    for attempt in 0..5 {
        let ip = if attempt % 2 == 0 {
            "1.1.1.1"
        } else {
            "2.2.2.2"
        };
        let result = if attempt < 3 {
            auth.change_password(ip, UserId(1), "guess", "new-password", &session)
                .await
        } else {
            auth.change_email(ip, UserId(1), "guess", &new_email).await
        };
        assert_eq!(result, Err(AuthError::InvalidCurrentPassword));
    }
    // ...so even the right password from a fresh IP is refused for now.
    assert_eq!(
        auth.change_password(
            "3.3.3.3",
            UserId(1),
            "correct-horse",
            "new-password",
            &session
        )
        .await,
        Err(AuthError::RateLimited)
    );
    assert_eq!(
        auth.change_email("3.3.3.3", UserId(1), "correct-horse", &new_email)
            .await,
        Err(AuthError::RateLimited)
    );
}

#[tokio::test]
async fn password_reentry_is_throttled_per_ip_across_accounts() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let auth = make_service(db.clone());
    for n in 0..10 {
        let id = seed_active_user(&db, &format!("ip{n}@example.com"), "secret-pass");
        let session = SessionId::new([n as u8 + 100; 32]);
        assert_eq!(
            auth.change_password("9.9.9.9", id, "guess", "new-password", &session)
                .await,
            Err(AuthError::InvalidCurrentPassword)
        );
    }
    let id = seed_active_user(&db, "ip-last@example.com", "secret-pass");
    let session = SessionId::new([200; 32]);
    assert_eq!(
        auth.change_password("9.9.9.9", id, "guess", "new-password", &session)
            .await,
        Err(AuthError::RateLimited)
    );
}

/// The residual pending-registration hole: an attacker re-registers the
/// owner's still-pending address with their own password. The owner's click
/// on the fresh link must not activate the attacker's credential.
#[tokio::test]
async fn activation_requires_the_password_of_the_current_credential() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let auth = make_service(db.clone());
    auth.register(
        "1.1.1.1",
        "owner@example.com",
        Some("Owner"),
        "owner-password",
        LocaleCode::En,
    )
    .await
    .unwrap();
    let owner_link = find_token(&db, "/verify-email");
    auth.register(
        "6.6.6.6",
        "owner@example.com",
        Some("Mallory"),
        "attacker-password",
        LocaleCode::PtBr,
    )
    .await
    .unwrap();
    {
        let locked = db.lock().unwrap();
        assert_eq!(locked.users.len(), 1, "re-registration reuses the account");
        assert_eq!(locked.users[0].display_name.as_deref(), Some("Mallory"));
        assert_eq!(
            locked.identities[0].credential_hash.as_deref(),
            Some("h:attacker-password"),
            "the latest registration replaces the pending credential"
        );
        let fresh = locked.emails.last().unwrap();
        assert_eq!(fresh.locale, LocaleCode::En, "the stored locale is kept");
        assert_eq!(locked.emails.len(), 2, "a fresh link is queued");
    }
    // The earlier link is retired by the re-registration.
    assert_eq!(
        auth.verify_email("1.1.1.1", &owner_link, Some("owner-password"))
            .await,
        Err(AuthError::TokenInvalid)
    );
    let fresh_link = token_from(db.lock().unwrap().emails[1].kind.action_link());
    // Looking at the link (a GET, or a mail scanner) spends nothing.
    assert_eq!(
        auth.verification_purpose(&fresh_link).await,
        Ok(bikesnest_application::VerificationPurpose::ActivateAccount)
    );
    // The owner clicks the fresh link but does not know the attacker's
    // password, so the account stays pending and the link stays usable.
    assert_eq!(
        auth.verify_email("1.1.1.1", &fresh_link, Some("owner-password"))
            .await,
        Err(AuthError::InvalidCredentials)
    );
    assert_eq!(
        db.lock().unwrap().users[0].account_state,
        AccountState::PendingEmailVerification
    );
    // Re-registering puts the owner's own credential back in charge.
    auth.register(
        "1.1.1.1",
        "owner@example.com",
        Some("Owner"),
        "owner-password",
        LocaleCode::En,
    )
    .await
    .unwrap();
    assert_eq!(
        auth.verify_email("1.1.1.1", &fresh_link, Some("attacker-password"))
            .await,
        Err(AuthError::TokenInvalid),
        "the attacker-era link is retired"
    );
    let owner_link = token_from(db.lock().unwrap().emails[2].kind.action_link());
    auth.verify_email("1.1.1.1", &owner_link, Some("owner-password"))
        .await
        .unwrap();
    assert_eq!(
        db.lock().unwrap().users[0].account_state,
        AccountState::Active
    );
    assert_eq!(
        auth.login("6.6.6.6", "owner@example.com", "attacker-password")
            .await
            .unwrap_err(),
        AuthError::InvalidCredentials
    );
}

#[tokio::test]
async fn activation_password_attempts_are_throttled_and_leave_the_token_unspent() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let auth = make_service(db.clone());
    auth.register(
        "1.1.1.1",
        "throttle@example.com",
        None,
        "password123",
        LocaleCode::En,
    )
    .await
    .unwrap();
    let token = find_token(&db, "/verify-email");
    for _ in 0..5 {
        assert_eq!(
            auth.verify_email("1.1.1.1", &token, Some("guess")).await,
            Err(AuthError::InvalidCredentials)
        );
    }
    assert_eq!(
        auth.verify_email("2.2.2.2", &token, Some("password123"))
            .await,
        Err(AuthError::RateLimited)
    );
    assert!(
        !db.lock().unwrap().verification[0].3,
        "refused attempts never spend the token"
    );
}

#[tokio::test]
async fn reset_password_revokes_all_sessions() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "a@example.com", "correct");
    let auth = make_service(db.clone());
    let outcome = auth
        .login("1.1.1.1", "a@example.com", "correct")
        .await
        .unwrap();
    let session = outcome.session;

    auth.request_password_reset("1.1.1.1", &UserEmail::parse("a@example.com").unwrap())
        .await
        .unwrap();
    let token = find_token(&db, "/password-reset/new");
    auth.reset_password(&token, "newpassword").await.unwrap();

    assert!(
        auth.resolve_session(&session).await.unwrap().is_none(),
        "old session revoked after reset"
    );
}

#[tokio::test]
async fn committed_security_transition_succeeds_when_inline_notice_dispatch_fails() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "notice-failure@example.com", "correct");
    let auth = make_service(db.clone());
    let email = UserEmail::parse("notice-failure@example.com").unwrap();
    auth.request_password_reset("1.1.1.1", &email)
        .await
        .unwrap();
    let token = find_token(&db, "/password-reset/new");
    db.lock().unwrap().dispatch_broken = true;

    auth.reset_password(&token, "replacement-password")
        .await
        .expect("durably committed password change is not reported as failed");
    assert_eq!(
        auth.reset_password(&token, "another-password").await,
        Err(AuthError::TokenInvalid),
        "the spent transition is not replayed when only notice dispatch failed"
    );
    let db = db.lock().unwrap();
    assert_eq!(
        db.identities[0].credential_hash.as_deref(),
        Some("h:replacement-password")
    );
    assert_eq!(
        db.audits
            .iter()
            .filter(|event| event.action == "auth.password_changed")
            .count(),
        1
    );
    assert_eq!(
        db.outbox
            .iter()
            .filter(|mail| mail.message.kind.code() == "password_changed")
            .count(),
        1
    );
}

#[tokio::test]
async fn oauth_only_account_gets_no_password_reset_token_or_email() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let email = UserEmail::parse("oauth-only@example.com").unwrap();
    let mut user = User::new(UserId(1), email.clone(), None);
    user.account_state = AccountState::Active;
    db.lock().unwrap().users.push(user);
    let auth = make_service(db.clone());

    auth.request_password_reset("1.1.1.1", &email)
        .await
        .unwrap();

    let db = db.lock().unwrap();
    assert!(db.reset.is_empty());
    assert!(db.emails.is_empty());
}

#[tokio::test]
async fn weak_password_and_hash_failure_leave_reset_token_usable() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "reset-retry@example.com", "correct");
    let auth = make_service(db.clone());
    auth.request_password_reset(
        "1.1.1.1",
        &UserEmail::parse("reset-retry@example.com").unwrap(),
    )
    .await
    .unwrap();
    let token = find_token(&db, "/password-reset/new");

    assert!(matches!(
        auth.reset_password(&token, "short").await,
        Err(AuthError::WeakPassword)
    ));
    assert!(!db.lock().unwrap().reset[0].3);
    assert!(db.lock().unwrap().audits.is_empty());

    db.lock().unwrap().hash_broken = true;
    assert_eq!(
        auth.reset_password(&token, "replacement-password").await,
        Err(AuthError::Internal)
    );
    assert!(!db.lock().unwrap().reset[0].3);
    assert!(db.lock().unwrap().audits.is_empty());

    db.lock().unwrap().hash_broken = false;
    auth.reset_password(&token, "replacement-password")
        .await
        .unwrap();
    assert!(db.lock().unwrap().reset[0].3);
    let db = db.lock().unwrap();
    let events: Vec<_> = db
        .audits
        .iter()
        .filter(|event| event.action == "auth.password_changed")
        .collect();
    assert_eq!(events.len(), 1);
    let event = events[0];
    assert_eq!(event.actor_user_id, Some(db.users[0].id));
    assert_eq!(event.target_type, "user");
    assert_eq!(event.target_id, db.users[0].id.0.to_string());
    assert_eq!(event.result, "success");
    assert_eq!(event.metadata, serde_json::json!({}));
}

#[tokio::test]
async fn expired_reset_preserves_credential_session_token_and_audit_state() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let user_id = seed_active_user(&db, "expired-reset@example.com", "correct");
    let auth = make_service(db.clone());
    let login = auth
        .login("1.1.1.1", "expired-reset@example.com", "correct")
        .await
        .unwrap();
    auth.request_password_reset(
        "1.1.1.1",
        &UserEmail::parse("expired-reset@example.com").unwrap(),
    )
    .await
    .unwrap();
    db.lock().unwrap().reset[0].2 = Utc::now() - chrono::Duration::seconds(1);
    let audits_before = db.lock().unwrap().audits.len();

    assert_eq!(
        auth.reset_password(
            &find_token(&db, "/password-reset/new"),
            "replacement-password"
        )
        .await,
        Err(AuthError::TokenInvalid)
    );

    let db = db.lock().unwrap();
    assert_eq!(
        db.identities
            .iter()
            .find(|identity| identity.user_id == user_id)
            .unwrap()
            .credential_hash
            .as_deref(),
        Some("h:correct")
    );
    assert!(
        db.sessions
            .iter()
            .find(|(raw, _)| *raw == login.session.to_hex())
            .unwrap()
            .1
            .revoked_at
            .is_none()
    );
    assert!(!db.reset[0].3);
    assert_eq!(db.audits.len(), audits_before);
}

#[tokio::test]
async fn successful_reset_invalidates_competing_tokens_without_changing_account_state() {
    for state in [AccountState::PendingEmailVerification, AccountState::Active] {
        let db = Arc::new(Mutex::new(FakeDb::default()));
        seed_active_user(&db, "competing-reset@example.com", "correct");
        db.lock().unwrap().users[0].account_state = state;
        let auth = make_service(db.clone());
        let email = UserEmail::parse("competing-reset@example.com").unwrap();
        auth.request_password_reset("1.1.1.1", &email)
            .await
            .unwrap();
        let first = find_token(&db, "/password-reset/new");
        auth.request_password_reset("2.2.2.2", &email)
            .await
            .unwrap();
        let second = find_token(&db, "/password-reset/new");

        auth.reset_password(&first, "replacement-password")
            .await
            .unwrap();
        assert_eq!(
            auth.reset_password(&second, "another-password").await,
            Err(AuthError::TokenInvalid)
        );
        assert_eq!(db.lock().unwrap().users[0].account_state, state);
        assert_eq!(
            db.lock()
                .unwrap()
                .audits
                .iter()
                .filter(|event| event.action == "auth.password_changed")
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn grant_role_requires_admin_and_refuses_last_admin_self_revoke() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    {
        let mut g = db.lock().unwrap();
        let mut plain = User::new(UserId(1), UserEmail::parse("u@example.com").unwrap(), None);
        plain.account_state = AccountState::Active;
        let mut admin = User::new(UserId(2), UserEmail::parse("a@example.com").unwrap(), None);
        admin.account_state = AccountState::Active;
        admin.roles = vec![Role::User, Role::Admin];
        g.users.push(plain);
        g.users.push(admin);
    }
    let auth = make_service(db.clone());

    let admin = {
        let g = db.lock().unwrap();
        AuthenticatedUser::from_user(&g.users[1])
    };
    let plain = {
        let g = db.lock().unwrap();
        AuthenticatedUser::from_user(&g.users[0])
    };

    // Non-admin actor denied.
    let err = auth.grant_role(&plain, UserId(2), Role::Moderator).await;
    assert_eq!(err.unwrap_err(), AuthError::Unauthorized);

    // Admin grants Moderator to the plain user.
    auth.grant_role(&admin, UserId(1), Role::Moderator)
        .await
        .unwrap();

    // Admin refuses to revoke own last Admin.
    let err = auth.revoke_role(&admin, UserId(2), Role::Admin).await;
    assert_eq!(err.unwrap_err(), AuthError::RefuseAdminSelfRevoke);
}

#[tokio::test]
async fn admin_may_revoke_another_admin_until_it_would_leave_none() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let a = seed_user_with_roles(&db, "a@example.com", vec![Role::User, Role::Admin]);
    let b = seed_user_with_roles(&db, "b@example.com", vec![Role::User, Role::Admin]);
    let auth = make_service(db.clone());
    let actor = actor_for(&db, a);

    // Two admins: revoking B's ADMIN is fine.
    auth.revoke_role(&actor, b, Role::Admin).await.unwrap();
    assert!(!roles_of(&db, b).contains(&Role::Admin));

    // A is now the only admin: revoking it — from anyone — is refused.
    let err = auth.revoke_role(&actor, a, Role::Admin).await.unwrap_err();
    assert_eq!(err, AuthError::RefuseAdminSelfRevoke);
    assert!(roles_of(&db, a).contains(&Role::Admin));
}

#[tokio::test]
async fn the_repository_owns_the_last_admin_guard_not_the_service() {
    // The refusal is the repository's, taken atomically with the delete. The
    // service used to count admins itself and then call a plain `revoke_role`,
    // which two concurrent revokes could both pass. Calling the repository
    // directly proves the guard has moved and cannot be bypassed by any other
    // caller of the port.
    use bikesnest_application::AccountRepository;

    let db = Arc::new(Mutex::new(FakeDb::default()));
    let a = seed_user_with_roles(&db, "guard-a@example.com", vec![Role::User, Role::Admin]);
    let b = seed_user_with_roles(
        &db,
        "guard-b@example.com",
        vec![Role::User, Role::Moderator],
    );
    let repo = FakeRepo::new(db.clone());

    // Sole admin: refused, and the role is still there.
    let err = repo
        .revoke_role_guarded(a, Role::Admin)
        .await
        .expect_err("the sole admin must not be demotable");
    assert_eq!(err, AuthError::RefuseAdminSelfRevoke);
    assert!(roles_of(&db, a).contains(&Role::Admin));

    // A role the target does not hold removes no admin: a no-op, not a refusal.
    assert!(!repo.revoke_role_guarded(a, Role::Moderator).await.unwrap());
    // And a non-ADMIN revoke on someone else is unaffected by the guard.
    assert!(repo.revoke_role_guarded(b, Role::Moderator).await.unwrap());
    assert!(!roles_of(&db, b).contains(&Role::Moderator));
}

#[tokio::test]
async fn last_admin_cannot_self_demote() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let a = seed_user_with_roles(&db, "only@example.com", vec![Role::User, Role::Admin]);
    let auth = make_service(db.clone());
    let actor = actor_for(&db, a);

    let err = auth.revoke_role(&actor, a, Role::Admin).await.unwrap_err();
    assert_eq!(err, AuthError::RefuseAdminSelfRevoke);
    assert!(roles_of(&db, a).contains(&Role::Admin));
}

#[tokio::test]
async fn admin_may_self_demote_while_another_admin_remains() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let a = seed_user_with_roles(&db, "a2@example.com", vec![Role::User, Role::Admin]);
    let _b = seed_user_with_roles(&db, "b2@example.com", vec![Role::User, Role::Admin]);
    let auth = make_service(db.clone());
    let actor = actor_for(&db, a);

    auth.revoke_role(&actor, a, Role::Admin).await.unwrap();
    assert!(!roles_of(&db, a).contains(&Role::Admin));
}

#[tokio::test]
async fn revoking_a_non_admin_role_is_unaffected_by_the_admin_floor() {
    // One admin in the system, but the revoke removes MODERATOR from someone
    // else — the floor must not block it.
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let a = seed_user_with_roles(&db, "a3@example.com", vec![Role::User, Role::Admin]);
    let b = seed_user_with_roles(&db, "b3@example.com", vec![Role::User, Role::Moderator]);
    let auth = make_service(db.clone());
    let actor = actor_for(&db, a);

    auth.revoke_role(&actor, b, Role::Moderator).await.unwrap();
    assert!(!roles_of(&db, b).contains(&Role::Moderator));
}

#[tokio::test]
async fn change_email_to_a_taken_address_is_refused_before_any_token() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    let user = seed_active_user(&db, "old@example.com", "correct-horse");
    seed_active_user(&db, "taken@example.com", "other-secret");
    let auth = make_service(db.clone());
    // Registration/verification mail from the seeding never happens (the users
    // are inserted directly), so the mailbox starts empty.
    assert!(db.lock().unwrap().emails.is_empty());

    let err = auth
        .change_email(
            "1.1.1.1",
            user,
            "correct-horse",
            &UserEmail::parse("taken@example.com").unwrap(),
        )
        .await
        .unwrap_err();

    assert_eq!(err, AuthError::EmailTaken);
    assert!(
        db.lock().unwrap().verification.is_empty(),
        "no verification token issued for a taken address"
    );
    assert!(
        db.lock().unwrap().emails.is_empty(),
        "no mail sent for a taken address"
    );
    assert_eq!(
        db.lock().unwrap().users[0].email.as_str(),
        "old@example.com",
        "the address is unchanged"
    );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn seed_user_with_roles(db: &Arc<Mutex<FakeDb>>, email: &str, roles: Vec<Role>) -> UserId {
    let mut g = db.lock().unwrap();
    g.next_id += 1;
    let id = UserId(g.next_id);
    let mut user = User::new(id, UserEmail::parse(email).unwrap(), None);
    user.account_state = AccountState::Active;
    user.roles = roles;
    g.users.push(user);
    id
}

fn actor_for(db: &Arc<Mutex<FakeDb>>, id: UserId) -> AuthenticatedUser {
    let g = db.lock().unwrap();
    AuthenticatedUser::from_user(g.users.iter().find(|u| u.id == id).expect("seeded user"))
}

fn roles_of(db: &Arc<Mutex<FakeDb>>, id: UserId) -> Vec<Role> {
    let g = db.lock().unwrap();
    g.users
        .iter()
        .find(|u| u.id == id)
        .map(|u| u.roles.clone())
        .unwrap_or_default()
}

fn seed_active_user(db: &Arc<Mutex<FakeDb>>, email: &str, password: &str) -> UserId {
    let mut g = db.lock().unwrap();
    g.next_id += 1;
    let id = UserId(g.next_id);
    let mut user = User::new(id, UserEmail::parse(email).unwrap(), None);
    user.account_state = AccountState::Active;
    g.users.push(user);
    g.identities.push(IdentityRecord {
        id: id.0,
        user_id: id,
        provider: AuthenticationProvider::Password,
        provider_subject: email.to_string(),
        credential_hash: Some(format!("h:{password}")),
    });
    id
}

/// Find the token param on the first queued message whose link contains `path`.
fn find_token(db: &Arc<Mutex<FakeDb>>, path: &str) -> String {
    db.lock()
        .unwrap()
        .emails
        .iter()
        .find(|e| e.kind.action_link().contains(path))
        .map(|e| token_from(e.kind.action_link()))
        .unwrap_or_default()
}

/// Pull the `token=` value (URL-safe base64) out of a URL in `text`.
fn token_from(text: &str) -> String {
    let Some(at) = text.find("token=") else {
        return String::new();
    };
    let rest = &text[at + "token=".len()..];
    let len = rest
        .bytes()
        .take_while(|b| matches!(b, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'))
        .count();
    rest[..len].to_string()
}

#[tokio::test]
async fn oauth_callback_links_to_existing_verified_email() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    // Seed an account whose email matches the fake OAuth identity, Active + verified.
    {
        let mut g = db.lock().unwrap();
        let mut u = User::new(
            UserId(1),
            UserEmail::parse("oauth.user@example.com").unwrap(),
            None,
        );
        u.account_state = AccountState::Active;
        u.email_verified_at = Some(Utc::now());
        g.users.push(u);
    }
    let auth = make_service(db.clone());
    let outcome = auth.oauth_callback("any-code").await.unwrap();
    assert_eq!(
        outcome.user.id,
        UserId(1),
        "login via the verified-email link path"
    );
    // A Google identity was linked to the existing account.
    let linked = db.lock().unwrap().identities.iter().any(|i| {
        i.user_id == UserId(1)
            && i.provider == AuthenticationProvider::Google
            && i.provider_subject == "sub-1"
    });
    assert!(linked, "google identity linked");
}

#[tokio::test]
async fn oauth_callback_creates_new_account_for_unmatched_email() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    // No existing user — the OAuth identity should create a fresh Active account.
    let auth = make_service(db.clone());
    let outcome = auth.oauth_callback("any-code").await.unwrap();
    assert!(
        outcome
            .user
            .email
            .as_str()
            .starts_with("oauth.user@example.com")
    );
    let db = db.lock().unwrap();
    assert_eq!(db.users.len(), 1);
    assert_eq!(db.users[0].account_state, AccountState::Active); // provider asserts a verified email
}

#[tokio::test]
async fn change_password_requires_current_and_verifies_new() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "a@example.com", "correct-horse");
    let auth = make_service(db.clone());
    let outcome = auth
        .login("1.1.1.1", "a@example.com", "correct-horse")
        .await
        .unwrap();
    let session = outcome.session;

    // Wrong current password is rejected.
    assert_eq!(
        auth.change_password("1.1.1.1", UserId(1), "wrong", "new-password", &session)
            .await
            .unwrap_err(),
        AuthError::InvalidCurrentPassword
    );
    // Correct current password succeeds.
    db.lock().unwrap().dispatch_broken = true;
    auth.change_password(
        "1.1.1.1",
        UserId(1),
        "correct-horse",
        "new-password",
        &session,
    )
    .await
    .unwrap();

    // The stored hash is updated → old password fails, new password logs in.
    assert_eq!(
        auth.login("1.1.1.1", "a@example.com", "correct-horse")
            .await
            .unwrap_err(),
        AuthError::InvalidCredentials
    );
    assert!(
        auth.login("1.1.1.1", "a@example.com", "new-password")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn change_email_switches_address_and_revokes_sessions() {
    let db = Arc::new(Mutex::new(FakeDb::default()));
    seed_active_user(&db, "old@example.com", "correct-horse");
    let auth = make_service(db.clone());
    let outcome = auth
        .login("1.1.1.1", "old@example.com", "correct-horse")
        .await
        .unwrap();
    let session = outcome.session;

    let new_email = UserEmail::parse("new@example.com").unwrap();
    auth.change_email("1.1.1.1", UserId(1), "correct-horse", &new_email)
        .await
        .unwrap();

    // A verification email to the new address was captured; following it switches
    // the canonical email AND revokes the prior session (a security event).
    let token = find_token(&db, "/verify-email");
    assert!(
        !token.is_empty(),
        "verification token sent to the new email"
    );
    db.lock().unwrap().dispatch_broken = true;
    // Confirming a change needs no password: it was re-entered to request it.
    auth.verify_email("1.1.1.1", &token, None).await.unwrap();

    assert_eq!(
        db.lock().unwrap().users[0].email.as_str(),
        "new@example.com"
    );
    assert!(
        auth.resolve_session(&session).await.unwrap().is_none(),
        "old session revoked after email change"
    );
}
