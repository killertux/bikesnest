//! Composition root of the web layer.
//!
//! Providers are wired in one place: this module is the only one that names a
//! concrete adapter (`Sqlx…`, the geocoder, the object store, the mail
//! provider). It parses nothing — every knob comes from the already-parsed
//! [`Config`] — builds the services, assembles [`AppState`], mounts the route
//! table from [`crate::routes`] and wraps it in the middleware stack.
//!
//! Handlers therefore never see a repository or a pool: they only see the
//! application ports held in [`AppState`].

use std::sync::Arc;

use axum::{Router, middleware};
use bikesnest_application::{
    AuthService, CheckReadiness, ContributionDeps, ContributionService, EmailProvider, EmailQueue,
    GetParkingDetails, ModerationDeps, ModerationService, ObjectStorage, PasswordHasher, PhotoDeps,
    PhotoService, PrivacyDeps, PrivacyService, RateLimiter, SearchParking,
};
use bikesnest_infrastructure::probe::SqlxDatabaseProbe;
use bikesnest_infrastructure::{
    Argon2PasswordHasher, Config, ConfigError, Db, FakeOAuthProvider, InlineEmailQueue,
    JobEmailQueue, LocalImageProcessor, OfflineTimezoneResolver, RealTokenGenerator,
    S3ObjectStorage, SharedGeocoder, SharedObjectStorage, SharedRateLimiter, SqlxAccountRepository,
    SqlxAnonymizationRepository, SqlxAuditLog, SqlxAuditLogReader, SqlxContributionHistoryReader,
    SqlxExportRepository, SqlxFavoriteRepository, SqlxModerationRepository,
    SqlxParkingContributionRepository, SqlxParkingDetailsReader, SqlxParkingPhotoReader,
    SqlxParkingSearchReader, SqlxPhotoRepository, SqlxPolicyReader, SqlxPrivacyRequestRepository,
    SqlxReportRepository, SqlxReviewPhotosReader, SqlxReviewRepository, SqlxSessionStore,
    SqlxSitemapReader, SqlxTokenStore, SqlxVerificationRepository, SystemClock,
    caching_geocoder_from_config, email_from_config, rate_limiter_from_config,
};
use tower_http::compression::predicate::{DefaultPredicate, NotForContentType, Predicate};

use crate::routes::errors::styled_errors;
use crate::security::SecurityHeaders;
use crate::state::AppState;

/// The doubles a test injects. Production passes [`RouterDeps::from_config`],
/// which builds every one of them from the parsed configuration.
pub struct RouterDeps<H: PasswordHasher + Clone + 'static> {
    /// The mail provider. Shared (not owned): `main` hands the same instance to
    /// `job_services` so the `email.send` handler and the inline queue talk to
    /// one configured relay/ESP.
    pub email: Arc<dyn EmailProvider>,
    /// `None` builds the deterministic stub from `Config::fake_oauth`.
    pub oauth: Option<FakeOAuthProvider>,
    pub hasher: H,
    pub rate_limiter: Box<dyn RateLimiter>,
    pub storage: Arc<dyn ObjectStorage>,
}

impl RouterDeps<Argon2PasswordHasher> {
    /// The real providers the configuration selected. Fails rather than
    /// substituting a fake when a provider the operator asked for cannot be
    /// built.
    pub fn from_config(config: &Config) -> Result<Self, ConfigError> {
        Ok(Self {
            email: Arc::from(email_from_config(&config.email)?),
            oauth: None,
            hasher: Argon2PasswordHasher,
            rate_limiter: rate_limiter_from_config(&config.rate_limiter)?,
            storage: Arc::new(S3ObjectStorage::from_config(&config.storage)),
        })
    }
}

/// Builds the full application router with the real providers the configuration
/// selected.
pub fn app_router(config: Arc<Config>, db: Db) -> Result<Router, ConfigError> {
    let deps = RouterDeps::from_config(&config)?;
    Ok(app_router_with(config, db, deps))
}

/// Builds the router from the parsed configuration plus injectable
/// email/OAuth/password/rate-limiter/storage providers (tests pass fakes — e.g.
/// a fast [`TestPasswordHasher`] and a fresh in-memory limiter — to keep the
/// suite fast and isolated).
///
/// The passed limiter is shared by the auth, contributions, photo and
/// moderation services so they all read one store.
pub fn app_router_with<H: PasswordHasher + Clone + 'static>(
    config: Arc<Config>,
    db: Db,
    deps: RouterDeps<H>,
) -> Router {
    let RouterDeps {
        email,
        oauth,
        hasher,
        rate_limiter,
        storage,
    } = deps;
    let oauth = oauth.unwrap_or_else(|| FakeOAuthProvider::from_config(&config.fake_oauth));
    let google_oauth_enabled = config.google_oauth_enabled;
    let rate_limiter: Arc<dyn RateLimiter> = Arc::from(rate_limiter);
    let probe = SqlxDatabaseProbe::new(db.clone(), config.probe_timeout);
    // One geocoder instance, wrapped in the in-process cache, shared by the
    // use case and the handler: `/search` asks the cache whether a query is
    // already resolved before it spends any of the caller's geocode budget.
    let geocoder = Arc::new(caching_geocoder_from_config(&config.geocoder));
    let search_uc = SearchParking::new(
        Box::new(SharedGeocoder::new(geocoder.clone())),
        Box::new(SqlxParkingSearchReader::new(
            db.clone(),
            config.recommendation,
            config.freshness,
        )),
    );
    let details = GetParkingDetails::new(
        Box::new(SqlxParkingDetailsReader::new(db.clone())),
        config.freshness,
    );
    // Transactional mail leaves the request path when there is a worker to
    // pick it up: `JOBS_ENABLED=true` (the default) queues an `email.send` job,
    // so a slow ESP cannot hold a registration open or fail it after the
    // account row exists. With the worker disabled nothing would ever claim
    // that row, so the same port sends inline instead — queuing it would be
    // indistinguishable from dropping the mail.
    let email_queue: Box<dyn EmailQueue> = if config.jobs.enabled {
        Box::new(JobEmailQueue::new(
            bikesnest_infrastructure::SqlxJobRepository::new(db.clone()),
            config.jobs.max_attempts,
        ))
    } else {
        Box::new(InlineEmailQueue::new(email))
    };
    let auth_service = AuthService::new(
        Box::new(SqlxAccountRepository::new(db.clone())),
        Box::new(SqlxSessionStore::new(db.clone())),
        Box::new(SqlxTokenStore::new(db.clone())),
        Box::new(hasher.clone()), // password hasher (Argon2 in prod, fast fake in tests)
        Box::new(RealTokenGenerator),
        Box::new(SystemClock),
        email_queue,
        Box::new(oauth),
        Box::new(SharedRateLimiter::new(rate_limiter.clone())), // shared ValKey store
        Box::new(SqlxAuditLog::new(db.clone())),
        config.base_url.clone(),
    );
    let contribution_service = ContributionService::new(ContributionDeps {
        tz: Box::new(OfflineTimezoneResolver::new()),
        contributions: Box::new(SqlxParkingContributionRepository::new(db.clone())),
        reviews: Box::new(SqlxReviewRepository::new(db.clone())),
        verifications: Box::new(SqlxVerificationRepository::new(db.clone())),
        favorites: Box::new(SqlxFavoriteRepository::new(db.clone())),
        history: Box::new(SqlxContributionHistoryReader::new(db.clone())),
        review_photos: Box::new(SqlxReviewPhotosReader::new(db.clone())),
        rate_limiter: Box::new(SharedRateLimiter::new(rate_limiter.clone())),
        audit: Box::new(SqlxAuditLog::new(db.clone())),
        clock: Box::new(SystemClock),
        freshness: config.freshness,
    });
    let photo_service = PhotoService::new(PhotoDeps {
        processor: Box::new(LocalImageProcessor::new(config.photo)),
        repository: Box::new(SqlxPhotoRepository::new(db.clone())),
        storage: Box::new(SharedObjectStorage::new(storage.clone())),
        rate_limiter: Box::new(SharedRateLimiter::new(rate_limiter.clone())),
        audit: Box::new(SqlxAuditLog::new(db.clone())),
        clock: Box::new(SystemClock),
        tokens_gen: Box::new(RealTokenGenerator),
        limits: config.photo,
    });
    let moderation_service = ModerationService::new(ModerationDeps {
        reports: Box::new(SqlxReportRepository::new(db.clone())),
        moderation: Box::new(SqlxModerationRepository::new(db.clone())),
        audit: Box::new(SqlxAuditLog::new(db.clone())),
        audit_reader: Box::new(SqlxAuditLogReader::new(db.clone())),
        history: Box::new(SqlxContributionHistoryReader::new(db.clone())),
        rate_limiter: Box::new(SharedRateLimiter::new(rate_limiter.clone())),
        limits: config.moderation,
    });
    let privacy_service = PrivacyService::new(PrivacyDeps {
        exports: Box::new(SqlxExportRepository::new(db.clone())),
        requests: Box::new(SqlxPrivacyRequestRepository::new(db.clone())),
        anonymization: Box::new(SqlxAnonymizationRepository::new(db.clone())),
        accounts: Box::new(SqlxAccountRepository::new(db.clone())),
        sessions: Box::new(SqlxSessionStore::new(db.clone())),
        audit: Box::new(SqlxAuditLog::new(db.clone())),
        hasher: Box::new(hasher),
        tokens_gen: Box::new(RealTokenGenerator),
        clock: Box::new(SystemClock),
    });
    let policy_reader: Arc<dyn bikesnest_application::PolicyReader> =
        Arc::new(SqlxPolicyReader::new(db.clone()));
    let state = AppState {
        readiness: Arc::new(CheckReadiness::new(probe)),
        search: Arc::new(search_uc),
        geocoder: geocoder.clone(),
        rate_limiter: rate_limiter.clone(),
        geocode_limits: config.geocode,
        details: Arc::new(details),
        freshness: config.freshness,
        photos: Arc::new(SqlxParkingPhotoReader::new(db.clone())),
        sitemap: Arc::new(SqlxSitemapReader::new(db.clone())),
        storage: storage.clone(),
        auth: Arc::new(auth_service),
        contributions: Arc::new(contribution_service),
        photo: Arc::new(photo_service),
        moderation: Arc::new(moderation_service),
        privacy: Arc::new(privacy_service),
        policy: policy_reader,
        security: SecurityHeaders::new(&config.security, &config.map, config.tls_on),
        map: config.map.clone(),
        base_url: config.base_url.clone(),
        google_oauth_enabled,
        assets: crate::assets::init(&config.static_root),
        config: config.clone(),
    };
    crate::routes::routes(&state)
        // Order matters: the LAST `.layer()` is the OUTERMOST middleware. We want
        // the security-header middleware to wrap `auth_middleware` so that even
        // when auth short-circuits (e.g. a CSRF-403 without calling the inner
        // handler) the security/CSP headers are still applied. TraceLayer stays
        // outermost so it observes every response.
        //
        // `styled_errors` is innermost: it upgrades the plain-text failures the
        // router and axum's own extractors emit (a 405, a malformed multipart
        // boundary, a body over `DefaultBodyLimit`) into the styled page — or,
        // for a real htmx fragment request, into a swap-safe fragment.
        .layer(middleware::from_fn_with_state(state.clone(), styled_errors))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            crate::auth::auth_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.security.clone(),
            crate::security::security_headers,
        ))
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(crate::observability::RequestSpan)
                .on_response(crate::observability::RequestLog),
        )
        // Outermost: compresses response bodies (br/gzip, negotiated from
        // `Accept-Encoding`) — but never `text/html`. Every HTML response
        // here embeds the per-session CSRF token (`<meta name="csrf">` +
        // hidden form fields) alongside attacker-influenced input (search
        // query, `next`, error messages); compressing that combination is
        // the BREACH oracle — the compressed length leaks the secret byte by
        // byte. Static assets (CSS/JS/JSON) carry no secret, so they still
        // compress normally; `/healthz`/`/readyz` are small JSON/text and
        // pass through the default's size/type exclusions untouched.
        .layer(
            tower_http::compression::CompressionLayer::new()
                .compress_when(DefaultPredicate::new().and(NotForContentType::new("text/html"))),
        )
        .with_state(state)
}
