//! Application-layer auth tests with in-memory fakes. These validate the
//! security-critical use-case behaviour without a database (//).

use async_trait::async_trait;
use bikesnest_application::{
    AccountRepository, AuditEvent, AuditLog, AuthError, AuthService, AuthenticatedUser, Clock,
    EmailError, EmailKind, EmailMessage, EmailQueue, IdentityRecord, LoginOutcome, NewAccount,
    OAuthProvider, PasswordHasher, RateLimitError, RateLimiter, Session, SessionStore,
    TokenGenerator, TokenStore, UserActivity, UserSearch,
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
    reset: Vec<(String, UserId, bool)>,
    /// What the use cases handed to the email *queue*. Nothing here was
    /// delivered: `AuthService` can no longer reach a provider at all, which is
    /// the point — a broken ESP cannot fail a registration any more.
    emails: Vec<EmailMessage>,
    /// Set to make `FakeQueue::enqueue` fail, standing in for "the database
    /// that holds the job queue is unreachable".
    queue_broken: bool,
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
    async fn set_password(&self, id: UserId, hash: &str) -> Result<(), AuthError> {
        let mut db = self.db.lock().unwrap();
        for i in db.identities.iter_mut() {
            if i.user_id == id && i.provider == AuthenticationProvider::Password {
                i.credential_hash = Some(hash.to_string());
            }
        }
        Ok(())
    }
    async fn confirm_email(
        &self,
        id: UserId,
        at: DateTime<Utc>,
        email: &UserEmail,
    ) -> Result<(), AuthError> {
        let mut db = self.db.lock().unwrap();
        if let Some(u) = db.users.iter_mut().find(|u| u.id == id) {
            u.email = email.clone();
            u.email_verified_at = Some(at);
            u.account_state = AccountState::Active;
        }
        for i in db.identities.iter_mut() {
            if i.user_id == id && i.provider == AuthenticationProvider::Password {
                i.provider_subject = email.as_str().to_string();
            }
        }
        Ok(())
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
    async fn issue_verification(
        &self,
        user_id: UserId,
        email: &str,
        raw: &VerificationToken,
        _now: DateTime<Utc>,
    ) -> Result<(), AuthError> {
        self.db.lock().unwrap().verification.push((
            raw.to_hex(),
            user_id,
            email.to_string(),
            false,
        ));
        Ok(())
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
    async fn issue_reset(
        &self,
        user_id: UserId,
        raw: &VerificationToken,
        _now: DateTime<Utc>,
    ) -> Result<(), AuthError> {
        self.db
            .lock()
            .unwrap()
            .reset
            .push((raw.to_hex(), user_id, false));
        Ok(())
    }
    async fn consume_reset(
        &self,
        raw: &VerificationToken,
        _now: DateTime<Utc>,
    ) -> Result<Option<UserId>, AuthError> {
        let mut db = self.db.lock().unwrap();
        let key = raw.to_hex();
        if let Some((_, u, used)) = db.reset.iter_mut().find(|(k, _, used)| *k == key && !*used) {
            *used = true;
            return Ok(Some(*u));
        }
        Ok(None)
    }
}

// --- PasswordHasher ---
#[derive(Clone)]
struct FakeHasher;
#[async_trait]
impl PasswordHasher for FakeHasher {
    async fn hash(&self, pw: &Password) -> Result<String, AuthError> {
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

// --- EmailQueue (records the handed-off messages; never delivers) ---
#[derive(Clone)]
struct FakeQueue {
    db: Arc<Mutex<FakeDb>>,
}
#[async_trait]
impl EmailQueue for FakeQueue {
    async fn enqueue(&self, msg: EmailMessage) -> Result<(), EmailError> {
        let mut db = self.db.lock().unwrap();
        if db.queue_broken {
            return Err(EmailError::Unavailable);
        }
        db.emails.push(msg);
        Ok(())
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
struct FakeAudit;
#[async_trait]
impl AuditLog for FakeAudit {
    async fn record(&self, _event: AuditEvent) -> Result<(), bikesnest_application::AuditError> {
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
        Box::new(FakeHasher),
        Box::new(FakeTokens {
            n: Arc::new(Mutex::new(0)),
        }),
        Box::new(FakeClock::new(Utc::now())),
        Box::new(FakeQueue { db: db.clone() }),
        Box::new(FakeOauth {
            email: "oauth.user@example.com".into(),
            subject: "sub-1".into(),
        }),
        Box::new(FakeRate {
            buckets: Arc::new(Mutex::new(HashMap::new())),
        }),
        Box::new(FakeAudit),
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
        assert!(queued[0].kind.link().contains("/verify-email?token="));

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

/// If the hand-off itself fails there is nothing to recover from later, so the
/// registration fails too: telling someone to check their inbox when no message
/// exists (and none ever will) is worse than asking them to try again.
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

    assert_eq!(err, AuthError::Internal);
    assert!(db.lock().unwrap().emails.is_empty());
    // The account and token rows were already written when the enqueue failed
    // (the repository ports expose no shared transaction). That is recoverable
    // and left deliberately visible: the address is unverified, so "resend
    // verification" issues a fresh token and queues a fresh message.
    assert_eq!(db.lock().unwrap().users.len(), 1);
    assert_eq!(
        db.lock().unwrap().users[0].account_state,
        AccountState::PendingEmailVerification
    );
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

    auth.resend_verification("1.1.1.1", &email).await.unwrap();
    auth.request_password_reset("1.1.1.1", &email)
        .await
        .unwrap();
    auth.change_email(
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
    assert!(auth.verify_email(&token).await.is_ok());

    let users = db.lock().unwrap();
    let u = users.users.iter().find(|u| u.id == user_id).unwrap();
    assert!(u.email_verified_at.is_some());
    assert_eq!(u.account_state, AccountState::Active);
    drop(users);

    // Second use of the same token fails (single-use).
    assert!(matches!(
        auth.verify_email(&token).await,
        Err(AuthError::TokenInvalid)
    ));
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
        .find(|e| e.kind.link().contains(path))
        .map(|e| token_from(e.kind.link()))
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
        auth.change_password(UserId(1), "wrong", "new-password", &session)
            .await
            .unwrap_err(),
        AuthError::InvalidCurrentPassword
    );
    // Correct current password succeeds.
    auth.change_password(UserId(1), "correct-horse", "new-password", &session)
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
    auth.change_email(UserId(1), "correct-horse", &new_email)
        .await
        .unwrap();

    // A verification email to the new address was captured; following it switches
    // the canonical email AND revokes the prior session (a security event).
    let token = find_token(&db, "/verify-email");
    assert!(
        !token.is_empty(),
        "verification token sent to the new email"
    );
    auth.verify_email(&token).await.unwrap();

    assert_eq!(
        db.lock().unwrap().users[0].email.as_str(),
        "new@example.com"
    );
    assert!(
        auth.resolve_session(&session).await.unwrap().is_none(),
        "old session revoked after email change"
    );
}
