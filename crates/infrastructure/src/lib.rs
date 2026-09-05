//! BikesNest infrastructure crate: persistence, config, ports impls.

pub mod auth;
pub mod community;
pub mod config;
pub mod db;
pub mod db_error;
pub mod devdata;
pub mod email;
pub mod fresh_seed;
pub mod geocoding;
pub mod job;
pub mod moderation;
pub mod parking;
pub mod photo;
pub mod privacy;
pub mod probe;
pub mod storage;
pub mod timezone;

pub use auth::{
    AUDIT_METADATA_KEYS, Argon2PasswordHasher, FakeOAuthProvider, InMemoryRateLimiter,
    RealTokenGenerator, SeedOutcome, SharedRateLimiter, SqlxAccountRepository, SqlxAuditLog,
    SqlxSessionStore, SqlxTokenStore, SystemClock, ValKeyRateLimiter, rate_limiter_from_config,
    seed_admin,
};
pub use community::{
    SqlxContributionHistoryReader, SqlxFavoriteRepository, SqlxParkingContributionRepository,
    SqlxReviewRepository, SqlxVerificationRepository,
};
pub use config::{
    AppEnv, Config, ConfigError, DbConfig, EmailConfig, FakeOAuthConfig, GeocodeLimits,
    GeocoderConfig, JobConfig, MapConfig, ModerationConfig, PhotoConfig, PolicySeedConfig,
    RateLimiterBackend, RateLimiterConfig, S3Config, SecurityConfig, TEST_MEDIA_ORIGIN,
};
pub use db::Db;
pub use db_error::{DbFailure, classify, classify_and_log, classify_code};
pub use email::{
    APP_NAME, CapturedEmail, FakeEmailProvider, InlineEmailQueue, JobEmailQueue, RenderedEmail,
    ResendEmailProvider, SmtpEmailProvider, from_config as email_from_config,
    render as render_email,
};
pub use fresh_seed::{FreshSeedResetError, reset_all_data};
pub use geocoding::{
    CachingGeocoder, FEATURED_BBOX_HALF_DEG, FEATURED_ORIGIN, FakeGeocoder, GoogleGeocoder,
    MapboxGeocoder, SharedGeocoder, caching_geocoder_from_config, geocoder_from_config,
};
pub use job::{
    ClaimedJob, JobRegistry, JobServices, SendEmailHandler, SqlxJobRepository, Worker, job_services,
};
pub use moderation::{SqlxAuditLogReader, SqlxModerationRepository, SqlxReportRepository};
pub use parking::{
    SqlxParkingDetailsReader, SqlxParkingPhotoReader, SqlxParkingSearchReader, SqlxSitemapReader,
};
pub use photo::{LocalImageProcessor, SqlxPhotoRepository, SqlxReviewPhotosReader};
pub use privacy::{
    POLICY_LOCALES, POLICY_PLACEHOLDERS, SqlxAnonymizationRepository, SqlxExportRepository,
    SqlxPolicyReader, SqlxPrivacyRequestRepository, SqlxRetentionRepository,
    fill_policy_placeholders, seed_policy,
};
pub use storage::{S3ObjectStorage, SharedObjectStorage};
pub use timezone::OfflineTimezoneResolver;
