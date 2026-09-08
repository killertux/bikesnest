//! One typed configuration for the whole process.
//!
//! Everything the app reads from the environment is parsed **once**, at
//! startup, into [`Config`]. Nothing below this module reads `std::env`: each
//! provider takes its typed section, the web layer takes an `Arc<Config>`, and
//! [`Config::validate_for_production`] refuses to start a production deployment
//! that is missing a required setting instead of silently substituting a
//! development default or an in-memory fake.

use std::path::PathBuf;
use std::time::Duration;

use bikesnest_application::{DEFAULT_RECOMMENDATION_CONFIG, FreshnessConfig, RecommendationConfig};
use bikesnest_domain::RetentionPolicy;
use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Environment lookup
// ---------------------------------------------------------------------------

/// Where configuration values come from: a `&str -> Option<String>` lookup.
///
/// Parsing goes through this rather than touching `std::env` directly so the
/// tests can drive [`Config::from_lookup`] with a fixed map — `std::env::set_var`
/// is `unsafe` in edition 2024 and this workspace forbids `unsafe`.
pub struct EnvSource<'a> {
    lookup: &'a dyn Fn(&str) -> Option<String>,
}

impl<'a> EnvSource<'a> {
    pub fn new(lookup: &'a dyn Fn(&str) -> Option<String>) -> Self {
        Self { lookup }
    }

    /// The raw value, exactly as the environment holds it (an explicitly empty
    /// value is `Some("")`, which some knobs treat differently from unset).
    fn raw(&self, key: &str) -> Option<String> {
        (self.lookup)(key)
    }

    /// A trimmed, non-empty value (unset and blank are both `None`).
    fn string(&self, key: &str) -> Option<String> {
        self.raw(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// A comma-separated list, blanks dropped. `None` when the key is unset.
    fn list(&self, key: &str) -> Option<Vec<String>> {
        self.raw(key).map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(str::to_string)
                .collect()
        })
    }

    fn parsed<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.string(key).and_then(|v| v.parse().ok())
    }

    fn u8(&self, key: &str) -> Option<u8> {
        self.parsed(key)
    }
    fn u16(&self, key: &str) -> Option<u16> {
        self.parsed(key)
    }
    fn u32(&self, key: &str) -> Option<u32> {
        self.parsed(key)
    }
    fn u64(&self, key: &str) -> Option<u64> {
        self.parsed(key)
    }
    fn usize(&self, key: &str) -> Option<usize> {
        self.parsed(key)
    }
    fn i64(&self, key: &str) -> Option<i64> {
        self.parsed(key)
    }
    fn f64(&self, key: &str) -> Option<f64> {
        self.parsed(key)
    }

    /// `true`/`1`/`yes`/`on` and their negatives, case-insensitively. Anything
    /// else (including a blank value) is `None` so the caller's default wins.
    fn bool(&self, key: &str) -> Option<bool> {
        self.string(key)
            .and_then(|v| match v.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => Some(true),
                "0" | "false" | "no" | "off" => Some(false),
                _ => None,
            })
    }

    fn require(&self, key: &'static str) -> Result<String, ConfigError> {
        self.string(key).ok_or(ConfigError::MissingEnv(key))
    }
}

// ---------------------------------------------------------------------------
// Typed sections
// ---------------------------------------------------------------------------

/// Which deployment this process believes it is. Drives log format and whether
/// [`Config::validate_for_production`] runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AppEnv {
    #[default]
    Development,
    Production,
}

impl AppEnv {
    pub fn is_production(self) -> bool {
        matches!(self, AppEnv::Production)
    }

    fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim) {
            Some(v) if v.eq_ignore_ascii_case("production") || v.eq_ignore_ascii_case("prod") => {
                AppEnv::Production
            }
            _ => AppEnv::Development,
        }
    }
}

/// Email backend. The variant carries its credentials, so a provider that
/// was asked for but cannot be built is a startup error rather than a silent
/// downgrade to the in-memory fake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailConfig {
    /// In-memory capture; also appends a dev outbox under `outbox_root` when set.
    Fake {
        outbox_root: Option<PathBuf>,
    },
    Smtp {
        host: String,
        port: u16,
        username: String,
        password: String,
        tls: bool,
        from: String,
    },
    Resend {
        api_key: String,
        from: String,
    },
}

/// Server-side address search backend. Real providers cannot exist without
/// their server credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GeocoderConfig {
    Fake,
    Mapbox { token: String },
    Google { api_key: String },
}

/// Rate-limiter store. `InMemory` is per-process and therefore development-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RateLimiterBackend {
    InMemory,
    Valkey { url: String },
    ValkeyCluster { urls: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RateLimiterConfig {
    pub backend: RateLimiterBackend,
    /// When the store is unreachable, allow (true) rather than 429 everything.
    pub fail_open: bool,
}

/// Per-IP budget for *billable* geocoding on `/search`.
///
/// A search that resolves a free-text destination is a paid third-party call,
/// so one visitor reloading a page with a query on it can spend real
/// money. `CachingGeocoder` absorbs the repeats; this bounds what a single
/// network can spend on cache **misses**. Generous by design — it exists to
/// stop abuse and runaway clients, not to ration normal use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeocodeLimits {
    /// Cache-missing geocodes allowed per IP per window.
    pub per_ip: u32,
    pub window: Duration,
}

impl Default for GeocodeLimits {
    fn default() -> Self {
        Self {
            per_ip: 60,
            window: Duration::from_secs(15 * 60),
        }
    }
}

/// S3-compatible object storage. `endpoint` is `None` for the standard AWS
/// endpoint; development defaults to the compose MinIO.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct S3Config {
    /// Endpoint used for server-side S3 operations.
    pub endpoint: Option<String>,
    /// Endpoint embedded in presigned URLs returned to browsers. This may
    /// differ from `endpoint` when the app runs on a private container network.
    pub public_endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// Origins templated into the Content-Security-Policy. HSTS is driven by
/// [`Config::tls_on`], which the security middleware receives alongside this.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SecurityConfig {
    /// Origins MapLibre loads style, tiles, glyphs and sprites from.
    pub tile_hosts: Vec<String>,
    /// Origins the browser may reach for client-side geocoding (empty in dev).
    pub geocode_hosts: Vec<String>,
    /// Object-storage origin(s) parking photos are served from as presigned URLs.
    pub media_hosts: Vec<String>,
}

/// Client-side map configuration for the development MapLibre basemap,
/// Mapbox GL JS, or Google Maps JavaScript API.
pub const DEFAULT_MAP_STYLE_URL: &str = "https://tiles.openfreemap.org/styles/liberty";
pub const DEFAULT_MAPBOX_STYLE_URL: &str = "mapbox://styles/mapbox/streets-v12";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MapConfig {
    MapLibre {
        style_url: String,
        /// Public Mapbox token for the style/tiles; empty for OpenFreeMap.
        access_token: String,
    },
    Mapbox {
        style_url: String,
        /// Browser-visible public token restricted to the site's origins.
        access_token: String,
    },
    Google {
        /// Browser-visible key, restricted to the site's HTTP referrers and
        /// the Maps JavaScript API in Google Cloud.
        browser_api_key: String,
        /// Required by Google's AdvancedMarkerElement.
        map_id: String,
    },
}

impl MapConfig {
    pub fn open_free_map() -> Self {
        Self::MapLibre {
            style_url: DEFAULT_MAP_STYLE_URL.to_string(),
            access_token: String::new(),
        }
    }
}

/// Deterministic dev identity for the Google sign-in stub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FakeOAuthConfig {
    pub email: String,
    pub subject: String,
}

impl Default for FakeOAuthConfig {
    fn default() -> Self {
        Self {
            email: "dev.user@example.com".to_string(),
            subject: "fake-google-sub-000001".to_string(),
        }
    }
}

/// Inputs for the `seed-policies` subcommand: the version being published plus
/// the controller identity substituted into the `{{TOKEN}}`s of `policies/*.md`.
#[derive(Debug, Clone)]
pub struct PolicySeedConfig {
    pub version: String,
    pub effective_at: DateTime<Utc>,
    /// `{{TOKEN}}` → value, for every placeholder whose variable is set.
    pub placeholders: Vec<(&'static str, String)>,
}

impl PolicySeedConfig {
    /// The value for a `{{TOKEN}}`, or `None` when its variable was unset.
    pub fn placeholder(&self, token: &str) -> Option<String> {
        self.placeholders
            .iter()
            .find(|(t, _)| *t == token)
            .map(|(_, v)| v.clone())
    }
}

/// Credentials the `seed-admin` subcommand bootstraps the first administrator
/// from. Optional: only that subcommand needs them, so a serving instance
/// parses them (unset = `None`) without failing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdminSeedConfig {
    /// `ADMIN_EMAIL`.
    pub email: Option<String>,
    /// `ADMIN_PASSWORD`.
    pub password: Option<String>,
}

/// PostgreSQL pool sizing plus the per-connection guard rails every pooled
/// session gets. The two timeouts are set with `SET` on each new connection, so
/// a runaway query or a transaction left open by a crashed client releases the
/// connection instead of pinning it forever.
///
/// Migrations deliberately run with `statement_timeout = 0` (see
/// [`Db::migrate`](crate::Db::migrate)): a cold database can spend minutes
/// building an index, and that is not the case the timeout defends against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbConfig {
    /// Pool ceiling (`DB_MAX_CONNECTIONS`).
    pub max_connections: u32,
    /// How long a caller waits for a free connection before erroring.
    pub acquire_timeout: Duration,
    /// `statement_timeout` per connection (`DB_STATEMENT_TIMEOUT_MS`).
    pub statement_timeout: Duration,
    /// `idle_in_transaction_session_timeout` (`DB_IDLE_IN_TX_TIMEOUT_MS`).
    pub idle_in_tx_timeout: Duration,
    /// Recycle a connection after this long, so a rolling database upgrade or a
    /// moved primary is picked up without a restart.
    pub max_lifetime: Duration,
    /// Close connections idle for longer than this.
    pub idle_timeout: Duration,
}

impl Default for DbConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            acquire_timeout: Duration::from_secs(5),
            statement_timeout: Duration::from_secs(5),
            idle_in_tx_timeout: Duration::from_secs(10),
            max_lifetime: Duration::from_secs(30 * 60),
            idle_timeout: Duration::from_secs(10 * 60),
        }
    }
}

/// Photo upload/derivative limits, env-driven with the domain constants as
/// defaults so operators can tune them without a rebuild.
pub type PhotoConfig = bikesnest_domain::PhotoLimits;

/// Moderation limits, env-driven with the domain constants as defaults.
pub type ModerationConfig = bikesnest_domain::ModerationLimits;

/// Background job queue knobs. Defaults target a single-instance dev worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JobConfig {
    /// Spawn the in-process worker loop at startup (false for web-only instances).
    pub enabled: bool,
    /// How often the worker polls the queue when idle.
    pub poll_interval: Duration,
    /// How many jobs a worker claims per batch.
    pub batch_size: usize,
    /// Lease length; a running job heartbeats to hold it, and a crashed worker's
    /// lease expires so another worker can re-claim.
    pub lease_ttl: Duration,
    /// Retry budget per job (overridable per row at enqueue).
    pub max_attempts: i32,
    /// Exponential backoff base; actual delay = base * 2^(attempt-1) + jitter.
    pub backoff_base_ms: u64,
    /// Terminal rows older than this many days are deleted by `jobs.gc`.
    pub history_retention_days: u32,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            poll_interval: Duration::from_secs(5),
            batch_size: 4,
            lease_ttl: Duration::from_secs(600),
            max_attempts: 5,
            backoff_base_ms: 2000,
            history_retention_days: 7,
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// The whole process configuration, parsed once at startup.
#[derive(Debug, Clone)]
pub struct Config {
    /// Which deployment this is: drives log format and startup validation.
    pub app_env: AppEnv,
    /// PostgreSQL connection string.
    pub database_url: String,
    /// Pool sizing + per-connection statement/idle timeouts.
    pub db: DbConfig,
    /// Socket address to bind, e.g. `0.0.0.0:8080`.
    pub bind_addr: String,
    /// How many reverse proxies sit in front of the app and may be trusted to
    /// have appended an `X-Forwarded-For` entry. `0` (the default) means no
    /// proxy: the TCP peer address is the only client identity, and forwarded
    /// headers are ignored — otherwise any client could pick its own
    /// rate-limit bucket by sending one.
    pub trusted_proxy_hops: u8,
    /// Public origin the app builds absolute links from (verification emails,
    /// canonical URLs, sitemap). No trailing slash is assumed.
    pub base_url: String,
    /// TLS terminates at or in front of this instance → emit HSTS.
    pub tls_on: bool,
    /// Timeout applied to readiness probes.
    pub probe_timeout: Duration,
    /// Directory the `/static` route serves from.
    pub static_root: PathBuf,
    /// Email backend.
    pub email: EmailConfig,
    /// Geocoding backend.
    pub geocoder: GeocoderConfig,
    /// Rate-limiter store.
    pub rate_limiter: RateLimiterConfig,
    /// Per-IP budget for billable geocoding.
    pub geocode: GeocodeLimits,
    /// Object storage.
    pub storage: S3Config,
    /// CSP origins.
    pub security: SecurityConfig,
    /// Client-side map style/token.
    pub map: MapConfig,
    /// Deterministic identity used by the Google sign-in stub.
    pub fake_oauth: FakeOAuthConfig,
    /// Total retention for an exported personal-data payload (hours).
    pub export_ttl_hours: u32,
    /// After this many inactive days an account is anonymized (0 = disabled).
    pub inactive_account_anonymize_after_days: u32,
    /// After this many days an anonymized account shell is purged (0 = disabled).
    pub deleted_account_purge_after_days: u32,
    /// Recommendation weights.
    pub recommendation: RecommendationConfig,
    /// Freshness/confidence thresholds.
    pub freshness: FreshnessConfig,
    /// Privacy retention TTLs.
    pub retention: RetentionPolicy,
    /// Photo pipeline limits.
    pub photo: PhotoConfig,
    /// Moderation limits.
    pub moderation: ModerationConfig,
    /// Background job queue.
    pub jobs: JobConfig,
    /// Inputs for the `seed-policies` subcommand.
    pub policy: PolicySeedConfig,
    /// Inputs for the `seed-admin` subcommand.
    pub admin_seed: AdminSeedConfig,
    /// Google sign-in feature flag. Only the deterministic fake provider exists,
    /// so production refuses to start with this on.
    pub google_oauth_enabled: bool,
}

const DEV_BASE_URL: &str = "http://localhost:8080";
const DEV_S3_ENDPOINT: &str = "http://localhost:9000";
/// The fake origin `Config::for_tests` puts in `security.media_hosts`, and
/// what `bikesnest_test_support::TestObjectStorage::presigned_get` signs its
/// URLs under — a single source of truth so the CSP test's rendered-photo
/// origin and its configured `media_hosts` can never drift apart. Deliberately
/// not `http://localhost:9000` (the real dev MinIO origin): a test that hits
/// this exact string is asserting on the *test double's* URL shape, not on
/// dev/MinIO wiring, and `.invalid` (RFC 2606) can never resolve to a real host.
pub const TEST_MEDIA_ORIGIN: &str = "http://media.test.invalid";
pub const DEFAULT_S3_REGION: &str = "us-east-1";
pub const DEFAULT_S3_BUCKET: &str = "bikesnest";
const DEV_S3_KEY: &str = "minioadmin";
const DEFAULT_EMAIL_FROM: &str = "no-reply@bikesnest.local";
const DEFAULT_POLICY_VERSION: &str = "2026-09-08.1";
/// Compile-time location of the static assets, used only when neither
/// `STATIC_ROOT` nor a `web/static` directory beside the CWD exists (so
/// `cargo run` from anywhere in the repo still serves CSS/JS).
const COMPILED_STATIC_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/static");

impl Config {
    /// Loads configuration from the process environment (typically populated
    /// from `.env` in development).
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|key| std::env::var(key).ok())
    }

    /// Loads configuration from an arbitrary lookup. `from_env` is this with
    /// `std::env::var`; tests pass a fixed map.
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let env = EnvSource::new(lookup);
        let app_env = AppEnv::parse(env.string("APP_ENV").as_deref());
        let dev = !app_env.is_production();

        Ok(Self {
            app_env,
            database_url: env.require("DATABASE_URL")?,
            db: db_config(&env),
            bind_addr: env
                .string("BIND_ADDR")
                .unwrap_or_else(|| "0.0.0.0:8080".to_string()),
            trusted_proxy_hops: env.u8("TRUSTED_PROXY_HOPS").unwrap_or(0),
            base_url: env
                .string("BASE_URL")
                .or_else(|| dev.then(|| DEV_BASE_URL.to_string()))
                .unwrap_or_default(),
            tls_on: env.bool("TLS_ON").unwrap_or(false),
            probe_timeout: Duration::from_millis(env.u64("PROBE_TIMEOUT_MS").unwrap_or(2000)),
            static_root: static_root(&env),
            email: email_config(&env, dev)?,
            geocoder: geocoder_config(&env)?,
            rate_limiter: rate_limiter_config(&env),
            geocode: geocode_limits(&env),
            storage: s3_config(&env, dev),
            security: security_config(&env),
            map: map_config(&env)?,
            fake_oauth: FakeOAuthConfig {
                email: env
                    .string("FAKE_OAUTH_EMAIL")
                    .unwrap_or_else(|| FakeOAuthConfig::default().email),
                subject: env
                    .string("FAKE_OAUTH_SUB")
                    .unwrap_or_else(|| FakeOAuthConfig::default().subject),
            },
            export_ttl_hours: env.u32("EXPORT_TTL_HOURS").unwrap_or(24),
            inactive_account_anonymize_after_days: env
                .u32("INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS")
                .unwrap_or(0),
            deleted_account_purge_after_days: env
                .u32("DELETED_ACCOUNT_PURGE_AFTER_DAYS")
                .unwrap_or(0),
            recommendation: recommendation_config(&env),
            freshness: freshness_config(&env),
            retention: retention_config(&env),
            photo: photo_config(&env),
            moderation: moderation_config(&env),
            jobs: job_config(&env),
            policy: policy_config(&env),
            admin_seed: AdminSeedConfig {
                email: env.string("ADMIN_EMAIL"),
                password: env.string("ADMIN_PASSWORD"),
            },
            google_oauth_enabled: env.bool("GOOGLE_OAUTH_ENABLED").unwrap_or(false),
        })
    }

    /// Everything a production deployment must not be missing, as a list of
    /// human-readable failures (empty = fit to serve). Each rule is checked
    /// independently so one run reports every problem, not just the first.
    ///
    /// A development deployment is never validated — it is expected to run on
    /// fakes.
    pub fn validate_for_production(&self) -> Result<(), Vec<String>> {
        if !self.app_env.is_production() {
            return Ok(());
        }
        let mut errs = Vec::new();

        // Public origin: emailed verification/reset links and canonical URLs
        // are built from it, so a localhost value ships broken emails.
        if self.base_url.is_empty() {
            errs.push("BASE_URL must be set to the public origin".to_string());
        } else if self.base_url.contains("localhost") || self.base_url.contains("127.0.0.1") {
            errs.push(format!(
                "BASE_URL must not point at localhost/127.0.0.1 (got {})",
                self.base_url
            ));
        }

        // Object storage: the MinIO development defaults must not survive.
        match self.storage.endpoint.as_deref() {
            Some(e) if !e.is_empty() => {}
            _ => errs.push("S3_ENDPOINT must be set".to_string()),
        }
        if self.storage.bucket.is_empty() {
            errs.push("S3_BUCKET must be set".to_string());
        }
        if self.storage.access_key_id.is_empty() {
            errs.push("S3_ACCESS_KEY_ID must be set".to_string());
        }
        if self.storage.secret_access_key.is_empty() {
            errs.push("S3_SECRET_ACCESS_KEY must be set".to_string());
        }
        if self.storage.access_key_id == DEV_S3_KEY || self.storage.secret_access_key == DEV_S3_KEY
        {
            errs.push(
                "S3 credentials must not be the MinIO development defaults (minioadmin)"
                    .to_string(),
            );
        }

        // Email: the fake drops every verification message on the floor.
        match &self.email {
            EmailConfig::Fake { .. } => errs.push(
                "EMAIL_PROVIDER must be smtp or resend (the fake provider discards every message)"
                    .to_string(),
            ),
            EmailConfig::Smtp { host, from, .. } => {
                if host.is_empty() {
                    errs.push("SMTP_HOST must be set when EMAIL_PROVIDER=smtp".to_string());
                }
                if from.is_empty() {
                    errs.push("EMAIL_FROM must be set when EMAIL_PROVIDER=smtp".to_string());
                }
            }
            EmailConfig::Resend { api_key, from } => {
                if api_key.is_empty() {
                    errs.push("RESEND_API_KEY must be set when EMAIL_PROVIDER=resend".to_string());
                }
                if from.is_empty() {
                    errs.push(
                        "RESEND_FROM (or EMAIL_FROM) must be set when EMAIL_PROVIDER=resend"
                            .to_string(),
                    );
                }
            }
        }

        // Geocoding: the fake fabricates coordinates for unknown queries.
        match &self.geocoder {
            GeocoderConfig::Mapbox { token } if token.is_empty() => {
                errs.push(
                    "MAPBOX_GEOCODING_ACCESS_TOKEN must be set for Mapbox location search"
                        .to_string(),
                );
            }
            GeocoderConfig::Mapbox { .. } => {}
            GeocoderConfig::Google { api_key } if api_key.is_empty() => {
                errs.push(
                    "GOOGLE_MAPS_SERVER_API_KEY must be set for Google location search"
                        .to_string(),
                );
            }
            GeocoderConfig::Google { .. } => {}
            GeocoderConfig::Fake => errs.push(
                "LOCATION_PROVIDER must be mapbox or google (the fake geocoder fabricates coordinates)"
                    .to_string(),
            ),
        }

        // Rate limiting: an in-memory limiter is per-process, so N replicas
        // multiply every limit by N.
        if matches!(self.rate_limiter.backend, RateLimiterBackend::InMemory) {
            errs.push(
                "VALKEY_URL or VALKEY_CLUSTER_URLS must be set (the in-memory rate limiter is per-process)"
                    .to_string(),
            );
        }

        if !self.tls_on {
            errs.push("TLS_ON must be true (HSTS is not emitted otherwise)".to_string());
        }

        if self.security.media_hosts.is_empty() {
            errs.push(
                "CSP_MEDIA_HOSTS must list the object-storage origin photos are served from"
                    .to_string(),
            );
        }

        if self.google_oauth_enabled {
            errs.push(
                "GOOGLE_OAUTH_ENABLED must be false (only the deterministic fake provider exists)"
                    .to_string(),
            );
        }

        if errs.is_empty() { Ok(()) } else { Err(errs) }
    }

    /// Every fake/in-process backend this configuration selected, as short
    /// labels. Development logs these at startup so it is obvious which parts
    /// of the stack are not real.
    pub fn fakes_in_use(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if matches!(self.email, EmailConfig::Fake { .. }) {
            out.push("email: in-memory fake (nothing is delivered)");
        }
        if matches!(self.geocoder, GeocoderConfig::Fake) {
            out.push("geocoder: deterministic fake (coordinates are made up)");
        }
        if matches!(self.rate_limiter.backend, RateLimiterBackend::InMemory) {
            out.push("rate limiter: in-memory (per-process only)");
        }
        if self.google_oauth_enabled {
            out.push("google sign-in: deterministic fake provider");
        }
        out
    }

    /// A development configuration for tests: fakes everywhere, no network
    /// dependencies beyond the database and the compose MinIO.
    pub fn for_tests(database_url: impl Into<String>) -> Self {
        Self {
            app_env: AppEnv::Development,
            database_url: database_url.into(),
            db: DbConfig::default(),
            bind_addr: "127.0.0.1:0".to_string(),
            trusted_proxy_hops: 0,
            base_url: DEV_BASE_URL.to_string(),
            tls_on: false,
            probe_timeout: Duration::from_secs(2),
            static_root: PathBuf::from(COMPILED_STATIC_ROOT),
            email: EmailConfig::Fake { outbox_root: None },
            geocoder: GeocoderConfig::Fake,
            rate_limiter: RateLimiterConfig {
                backend: RateLimiterBackend::InMemory,
                fail_open: true,
            },
            geocode: GeocodeLimits::default(),
            storage: S3Config {
                endpoint: Some(DEV_S3_ENDPOINT.to_string()),
                public_endpoint: Some(DEV_S3_ENDPOINT.to_string()),
                region: DEFAULT_S3_REGION.to_string(),
                bucket: DEFAULT_S3_BUCKET.to_string(),
                access_key_id: DEV_S3_KEY.to_string(),
                secret_access_key: DEV_S3_KEY.to_string(),
            },
            security: SecurityConfig {
                tile_hosts: vec![DEFAULT_TILE_HOST.to_string()],
                geocode_hosts: Vec::new(),
                media_hosts: vec![TEST_MEDIA_ORIGIN.to_string()],
            },
            map: MapConfig::open_free_map(),
            fake_oauth: FakeOAuthConfig::default(),
            export_ttl_hours: 24,
            inactive_account_anonymize_after_days: 0,
            deleted_account_purge_after_days: 0,
            recommendation: DEFAULT_RECOMMENDATION_CONFIG,
            freshness: FreshnessConfig {
                thresholds: bikesnest_domain::DEFAULT_THRESHOLDS,
            },
            retention: RetentionPolicy::default(),
            photo: PhotoConfig::default(),
            moderation: ModerationConfig::default(),
            jobs: JobConfig {
                enabled: false,
                ..JobConfig::default()
            },
            policy: PolicySeedConfig {
                version: DEFAULT_POLICY_VERSION.to_string(),
                effective_at: Utc::now(),
                placeholders: Vec::new(),
            },
            admin_seed: AdminSeedConfig::default(),
            google_oauth_enabled: false,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing environment variable: {0}")]
    MissingEnv(&'static str),
    #[error("invalid configuration for {key}: {reason}")]
    Invalid { key: &'static str, reason: String },
}

impl ConfigError {
    pub fn invalid(key: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            key,
            reason: reason.into(),
        }
    }
}

// ---------------------------------------------------------------------------
// Section parsers
// ---------------------------------------------------------------------------

/// Default MapLibre tile origin, also the CSP default for `CSP_TILE_HOSTS`.
pub const DEFAULT_TILE_HOST: &str = "https://tiles.openfreemap.org";

/// `STATIC_ROOT`, else `web/static` relative to the working directory, else the
/// path the binary was compiled from (so `cargo run` works from the repo).
fn static_root(env: &EnvSource<'_>) -> PathBuf {
    if let Some(explicit) = env.string("STATIC_ROOT") {
        return PathBuf::from(explicit);
    }
    let cwd_relative = PathBuf::from("web/static");
    if cwd_relative.is_dir() {
        return cwd_relative;
    }
    PathBuf::from(COMPILED_STATIC_ROOT)
}

/// `EMAIL_PROVIDER` (`fake` | `smtp` | `resend`; unset = `fake`). A provider that
/// was explicitly asked for but lacks its credentials is an error — it never
/// degrades to the fake, because a silent fake means every verification email
/// disappears.
fn email_config(env: &EnvSource<'_>, dev: bool) -> Result<EmailConfig, ConfigError> {
    let from = env
        .string("EMAIL_FROM")
        .unwrap_or_else(|| DEFAULT_EMAIL_FROM.to_string());
    match env
        .string("EMAIL_PROVIDER")
        .unwrap_or_else(|| "fake".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "fake" => Ok(EmailConfig::Fake {
            // Only development writes the readable outbox to disk. `MEDIA_ROOT`
            // is now *only* this directory: media itself lives in object
            // storage, and the retention sweep lists the bucket rather than a
            // local tree.
            outbox_root: dev.then(|| {
                env.string("MEDIA_ROOT")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("media"))
            }),
        }),
        "smtp" => Ok(EmailConfig::Smtp {
            host: env.require("SMTP_HOST")?,
            port: env.u16("SMTP_PORT").unwrap_or(1025),
            username: env.string("SMTP_USERNAME").unwrap_or_default(),
            password: env.string("SMTP_PASSWORD").unwrap_or_default(),
            tls: env.bool("SMTP_TLS").unwrap_or(false),
            from,
        }),
        "resend" => Ok(EmailConfig::Resend {
            api_key: env.require("RESEND_API_KEY")?,
            from: env.string("RESEND_FROM").unwrap_or(from),
        }),
        other => Err(ConfigError::invalid(
            "EMAIL_PROVIDER",
            format!("unknown provider {other:?}; expected fake, smtp or resend"),
        )),
    }
}

/// The coherent `LOCATION_PROVIDER` profile wins over the legacy `GEOCODER`
/// selector. Keeping the legacy variable readable avoids breaking existing
/// deployments while new deployments can switch geocoding and maps together.
fn geocoder_config(env: &EnvSource<'_>) -> Result<GeocoderConfig, ConfigError> {
    let from_profile = env.string("LOCATION_PROVIDER");
    let key = if from_profile.is_some() {
        "LOCATION_PROVIDER"
    } else {
        "GEOCODER"
    };
    match from_profile
        .or_else(|| env.string("GEOCODER"))
        .unwrap_or_else(|| "fake".to_string())
        .to_ascii_lowercase()
        .as_str()
    {
        "fake" => Ok(GeocoderConfig::Fake),
        "mapbox" => Ok(GeocoderConfig::Mapbox {
            token: env
                .string("MAPBOX_GEOCODING_ACCESS_TOKEN")
                .or_else(|| env.string("MAPBOX_ACCESS_TOKEN"))
                .ok_or(ConfigError::MissingEnv("MAPBOX_GEOCODING_ACCESS_TOKEN"))?,
        }),
        "google" => Ok(GeocoderConfig::Google {
            api_key: env.require("GOOGLE_MAPS_SERVER_API_KEY")?,
        }),
        other => Err(ConfigError::invalid(
            key,
            format!("unknown provider {other:?}; expected fake, mapbox or google"),
        )),
    }
}

/// `VALKEY_CLUSTER_URLS` wins over `VALKEY_URL`; neither = in-memory.
fn rate_limiter_config(env: &EnvSource<'_>) -> RateLimiterConfig {
    let backend = match env.list("VALKEY_CLUSTER_URLS") {
        Some(urls) if !urls.is_empty() => RateLimiterBackend::ValkeyCluster { urls },
        _ => match env.string("VALKEY_URL") {
            Some(url) => RateLimiterBackend::Valkey { url },
            None => RateLimiterBackend::InMemory,
        },
    };
    RateLimiterConfig {
        backend,
        // Default fail OPEN: a ValKey outage must not 429 every endpoint.
        fail_open: env.bool("RATE_LIMIT_FAIL_OPEN").unwrap_or(true),
    }
}

/// `S3_*`. Development falls back to the compose MinIO so `cargo run` works;
/// production gets no defaults at all, so anything missing surfaces in
/// [`Config::validate_for_production`]. An explicitly empty `S3_ENDPOINT` means
/// "the standard AWS endpoint".
fn s3_config(env: &EnvSource<'_>, dev: bool) -> S3Config {
    let endpoint = match env.raw("S3_ENDPOINT") {
        Some(raw) => {
            let trimmed = raw.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        }
        None => dev.then(|| DEV_S3_ENDPOINT.to_string()),
    };
    let dev_default = |value: &str| {
        if dev {
            value.to_string()
        } else {
            String::new()
        }
    };
    let public_endpoint = match env.raw("S3_PUBLIC_ENDPOINT") {
        Some(raw) => {
            let trimmed = raw.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        }
        None => endpoint.clone(),
    };
    S3Config {
        endpoint,
        public_endpoint,
        region: env
            .string("S3_REGION")
            .unwrap_or_else(|| DEFAULT_S3_REGION.to_string()),
        bucket: env
            .string("S3_BUCKET")
            .unwrap_or_else(|| dev_default(DEFAULT_S3_BUCKET)),
        access_key_id: env
            .string("S3_ACCESS_KEY_ID")
            .unwrap_or_else(|| dev_default(DEV_S3_KEY)),
        secret_access_key: env
            .string("S3_SECRET_ACCESS_KEY")
            .unwrap_or_else(|| dev_default(DEV_S3_KEY)),
    }
}

/// `CSP_*` origin lists.
fn security_config(env: &EnvSource<'_>) -> SecurityConfig {
    SecurityConfig {
        tile_hosts: env
            .list("CSP_TILE_HOSTS")
            .unwrap_or_else(|| vec![DEFAULT_TILE_HOST.to_string()]),
        geocode_hosts: env.list("CSP_GEOCODE_HOSTS").unwrap_or_default(),
        media_hosts: env.list("CSP_MEDIA_HOSTS").unwrap_or_default(),
    }
}

/// Is the style URL a Mapbox style (which needs a client-side access token)?
fn is_mapbox_style(style_url: &str) -> bool {
    style_url.starts_with("mapbox://") || style_url.contains("api.mapbox.com")
}

/// Pure resolver, separated from the lookup so the token rules are unit-testable.
fn resolve_map_config(
    style_url: Option<String>,
    map_token: Option<String>,
    fallback_token: Option<String>,
) -> MapConfig {
    let style_url = style_url.unwrap_or_else(|| DEFAULT_MAP_STYLE_URL.to_string());
    let access_token = if is_mapbox_style(&style_url) {
        map_token.or(fallback_token).unwrap_or_default()
    } else {
        // Non-Mapbox style (e.g. OpenFreeMap) needs no token; keep it off the page.
        String::new()
    };
    MapConfig::MapLibre {
        style_url,
        access_token,
    }
}

/// Map half of the location-provider profile. Without `LOCATION_PROVIDER`,
/// preserve the existing `MAP_STYLE_URL`/MapLibre behavior.
fn map_config(env: &EnvSource<'_>) -> Result<MapConfig, ConfigError> {
    match env
        .string("LOCATION_PROVIDER")
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("google") => Ok(MapConfig::Google {
            browser_api_key: env.require("GOOGLE_MAPS_BROWSER_API_KEY")?,
            map_id: env.require("GOOGLE_MAP_ID")?,
        }),
        Some("mapbox") => Ok(MapConfig::Mapbox {
            style_url: env
                .string("MAPBOX_STYLE_URL")
                .unwrap_or_else(|| DEFAULT_MAPBOX_STYLE_URL.to_string()),
            access_token: env.require("MAPBOX_MAP_ACCESS_TOKEN")?,
        }),
        Some("fake") => Ok(MapConfig::open_free_map()),
        Some(other) => Err(ConfigError::invalid(
            "LOCATION_PROVIDER",
            format!("unknown provider {other:?}; expected fake, mapbox or google"),
        )),
        None => Ok(resolve_map_config(
            env.string("MAP_STYLE_URL"),
            env.string("MAPBOX_MAP_ACCESS_TOKEN"),
            env.string("MAPBOX_ACCESS_TOKEN"),
        )),
    }
}

/// Per-IP geocode budget, from env with the defaults above.
fn geocode_limits(env: &EnvSource<'_>) -> GeocodeLimits {
    let d = GeocodeLimits::default();
    GeocodeLimits {
        per_ip: env.u32("RATE_GEOCODE_PER_IP").unwrap_or(d.per_ip),
        window: env
            .u64("RATE_GEOCODE_WINDOW_SECS")
            .map(Duration::from_secs)
            .unwrap_or(d.window),
    }
}

/// Recommendation weights from environment configuration.
fn recommendation_config(env: &EnvSource<'_>) -> RecommendationConfig {
    let rec = DEFAULT_RECOMMENDATION_CONFIG;
    RecommendationConfig {
        w_distance: env.f64("REC_WEIGHT_DISTANCE").unwrap_or(rec.w_distance),
        w_security: env.f64("REC_WEIGHT_SECURITY").unwrap_or(rec.w_security),
        w_rating: env.f64("REC_WEIGHT_RATING").unwrap_or(rec.w_rating),
        w_freshness: env.f64("REC_WEIGHT_FRESHNESS").unwrap_or(rec.w_freshness),
        w_verification: env
            .f64("REC_WEIGHT_VERIFICATION")
            .unwrap_or(rec.w_verification),
    }
}

/// Freshness thresholds from environment configuration.
fn freshness_config(env: &EnvSource<'_>) -> FreshnessConfig {
    let d = bikesnest_domain::DEFAULT_THRESHOLDS;
    FreshnessConfig {
        thresholds: bikesnest_domain::FreshnessThresholds {
            fresh_days: env.i64("FRESHNESS_FRESH_DAYS").unwrap_or(d.fresh_days),
            recent_days: env.i64("FRESHNESS_RECENT_DAYS").unwrap_or(d.recent_days),
            aging_days: env.i64("FRESHNESS_AGING_DAYS").unwrap_or(d.aging_days),
            stale_days: env.i64("FRESHNESS_STALE_DAYS").unwrap_or(d.stale_days),
        },
    }
}

/// Photo pipeline limits from environment configuration.
fn photo_config(env: &EnvSource<'_>) -> PhotoConfig {
    PhotoConfig {
        max_bytes: env
            .usize("PHOTO_MAX_BYTES")
            .unwrap_or(bikesnest_domain::MAX_PHOTO_BYTES),
        max_megapixels: env
            .u64("PHOTO_MAX_MEGAPIXELS")
            .unwrap_or(bikesnest_domain::MAX_PHOTO_MEGAPIXELS),
        thumb_max_side: env
            .u32("PHOTO_THUMB_MAX_SIDE")
            .unwrap_or(bikesnest_domain::THUMBNAIL_MAX_SIDE),
        derivative_quality: env
            .u8("PHOTO_DERIVATIVE_QUALITY")
            .unwrap_or(bikesnest_domain::DERIVATIVE_QUALITY),
    }
}

/// Moderation limits, from env with the domain defaults.
fn moderation_config(env: &EnvSource<'_>) -> ModerationConfig {
    let d = ModerationConfig::default();
    ModerationConfig {
        report_description_max_len: env
            .usize("MOD_REPORT_DESC_MAX_LEN")
            .unwrap_or(d.report_description_max_len),
        report_create_user_limit: env
            .u32("MOD_REPORT_USER_LIMIT")
            .unwrap_or(d.report_create_user_limit),
        report_create_ip_limit: env
            .u32("MOD_REPORT_IP_LIMIT")
            .unwrap_or(d.report_create_ip_limit),
    }
}

/// Job queue knobs, from env with sane single-instance defaults.
fn job_config(env: &EnvSource<'_>) -> JobConfig {
    let d = JobConfig::default();
    JobConfig {
        enabled: env.bool("JOBS_ENABLED").unwrap_or(d.enabled),
        poll_interval: env
            .u64("JOBS_POLL_INTERVAL_MS")
            .map(Duration::from_millis)
            .unwrap_or(d.poll_interval),
        batch_size: env.usize("JOBS_BATCH_SIZE").unwrap_or(d.batch_size),
        lease_ttl: env
            .u64("JOBS_LEASE_TTL_MS")
            .map(Duration::from_millis)
            .unwrap_or(d.lease_ttl),
        max_attempts: env
            .i64("JOBS_MAX_ATTEMPTS")
            .map(|v| v as i32)
            .unwrap_or(d.max_attempts),
        backoff_base_ms: env.u64("JOBS_BACKOFF_BASE_MS").unwrap_or(d.backoff_base_ms),
        history_retention_days: env
            .u32("JOBS_HISTORY_RETENTION_DAYS")
            .unwrap_or(d.history_retention_days),
    }
}

/// Pool sizing + the per-connection guard rails. Only the three knobs an
/// operator realistically tunes are env-driven; lifetimes are fixed.
fn db_config(env: &EnvSource<'_>) -> DbConfig {
    let d = DbConfig::default();
    DbConfig {
        max_connections: env.u32("DB_MAX_CONNECTIONS").unwrap_or(d.max_connections),
        statement_timeout: env
            .u64("DB_STATEMENT_TIMEOUT_MS")
            .map(Duration::from_millis)
            .unwrap_or(d.statement_timeout),
        idle_in_tx_timeout: env
            .u64("DB_IDLE_IN_TX_TIMEOUT_MS")
            .map(Duration::from_millis)
            .unwrap_or(d.idle_in_tx_timeout),
        ..d
    }
}

/// Retention TTLs. Values are seconds; each defaults to the
/// `RetentionPolicy::default()` value when unset.
fn retention_config(env: &EnvSource<'_>) -> RetentionPolicy {
    let d = RetentionPolicy::default();
    let seconds = |key: &str, fallback: chrono::Duration| {
        chrono::Duration::seconds(env.i64(key).unwrap_or(fallback.num_seconds()))
    };
    RetentionPolicy {
        password_reset_ttl: seconds("RETENTION_PASSWORD_RESET_SECONDS", d.password_reset_ttl),
        email_verification_ttl: seconds(
            "RETENTION_EMAIL_VERIFICATION_SECONDS",
            d.email_verification_ttl,
        ),
        session_idle: seconds("RETENTION_SESSION_IDLE_SECONDS", d.session_idle),
        parked_here_ttl: seconds("RETENTION_PARKED_HERE_SECONDS", d.parked_here_ttl),
        export_ttl: seconds("RETENTION_EXPORT_SECONDS", d.export_ttl),
        upload_orphan_ttl: seconds("RETENTION_UPLOAD_ORPHAN_SECONDS", d.upload_orphan_ttl),
    }
}

/// `seed-policies` inputs: version, effective date, and the controller identity
/// substituted into the `{{TOKEN}}`s of `policies/*.md`.
fn policy_config(env: &EnvSource<'_>) -> PolicySeedConfig {
    PolicySeedConfig {
        version: env
            .string("POLICY_VERSION")
            .unwrap_or_else(|| DEFAULT_POLICY_VERSION.to_string()),
        effective_at: env
            .string("POLICY_EFFECTIVE_AT")
            .and_then(|v| {
                DateTime::parse_from_rfc3339(&v)
                    .ok()
                    .map(|d| d.with_timezone(&Utc))
            })
            .unwrap_or_else(Utc::now),
        placeholders: crate::privacy::POLICY_PLACEHOLDERS
            .iter()
            .filter_map(|(token, var)| env.string(var).map(|value| (*token, value)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A lookup over a fixed map — no process environment involved.
    fn lookup(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + use<> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    fn config(pairs: &[(&str, &str)]) -> Config {
        Config::from_lookup(&lookup(pairs)).expect("config parses")
    }

    const DB: (&str, &str) = ("DATABASE_URL", "postgres://u:p@localhost/db");

    /// `seed-admin` reads its credentials from the configuration, so both
    /// halves must survive parsing — and be `None`, not an empty string, when
    /// the operator has not set them.
    #[test]
    fn admin_seed_credentials_are_parsed_when_present_and_none_when_absent() {
        let c = config(&[
            DB,
            ("ADMIN_EMAIL", "admin@bikesnest.example"),
            ("ADMIN_PASSWORD", "correct horse battery"),
        ]);
        assert_eq!(
            c.admin_seed.email.as_deref(),
            Some("admin@bikesnest.example")
        );
        assert_eq!(
            c.admin_seed.password.as_deref(),
            Some("correct horse battery")
        );

        let bare = config(&[DB]);
        assert_eq!(bare.admin_seed, AdminSeedConfig::default());
        assert!(bare.admin_seed.email.is_none() && bare.admin_seed.password.is_none());
    }

    /// Every knob a production deployment must carry.
    fn production_env() -> Vec<(&'static str, &'static str)> {
        vec![
            DB,
            ("APP_ENV", "production"),
            ("BASE_URL", "https://bikesnest.example"),
            ("TLS_ON", "true"),
            ("S3_ENDPOINT", "https://s3.eu-west-1.amazonaws.com"),
            ("S3_BUCKET", "bikesnest-prod"),
            ("S3_ACCESS_KEY_ID", "AKIAREAL"),
            ("S3_SECRET_ACCESS_KEY", "s3cret"),
            ("EMAIL_PROVIDER", "resend"),
            ("RESEND_API_KEY", "re_live_key"),
            ("EMAIL_FROM", "BikesNest <no-reply@bikesnest.example>"),
            ("GEOCODER", "mapbox"),
            ("MAPBOX_ACCESS_TOKEN", "pk.real"),
            ("VALKEY_URL", "valkey://valkey:6379"),
            ("CSP_MEDIA_HOSTS", "https://cdn.bikesnest.example"),
        ]
    }

    // --- production validation ---------------------------------------------

    #[test]
    fn production_with_nothing_set_reports_every_missing_item() {
        let cfg = config(&[DB, ("APP_ENV", "production")]);
        let errs = cfg
            .validate_for_production()
            .expect_err("an empty production environment cannot be valid");
        let joined = errs.join("\n");
        for expected in [
            "BASE_URL",
            "S3_ENDPOINT",
            "S3_BUCKET",
            "S3_ACCESS_KEY_ID",
            "S3_SECRET_ACCESS_KEY",
            "EMAIL_PROVIDER",
            "LOCATION_PROVIDER",
            "VALKEY_URL",
            "TLS_ON",
            "CSP_MEDIA_HOSTS",
        ] {
            assert!(
                errs.iter().any(|e| e.contains(expected)),
                "missing a failure mentioning {expected}:\n{joined}"
            );
        }
        // Every rule reports independently rather than stopping at the first.
        assert_eq!(errs.len(), 10, "one message per failing rule:\n{joined}");
    }

    #[test]
    fn complete_production_environment_passes() {
        let cfg = config(&production_env());
        assert_eq!(cfg.app_env, AppEnv::Production);
        assert!(
            cfg.validate_for_production().is_ok(),
            "{:?}",
            cfg.validate_for_production()
        );
        assert!(cfg.fakes_in_use().is_empty());
    }

    #[test]
    fn production_rejects_localhost_base_url_and_minio_credentials() {
        let mut env = production_env();
        env.retain(|(k, _)| {
            !matches!(*k, "BASE_URL" | "S3_ACCESS_KEY_ID" | "S3_SECRET_ACCESS_KEY")
        });
        env.extend([
            ("BASE_URL", "http://localhost:8080"),
            ("S3_ACCESS_KEY_ID", "minioadmin"),
            ("S3_SECRET_ACCESS_KEY", "minioadmin"),
        ]);
        let errs = config(&env).validate_for_production().unwrap_err();
        assert!(errs.iter().any(|e| e.contains("localhost")), "{errs:?}");
        assert!(errs.iter().any(|e| e.contains("minioadmin")), "{errs:?}");
    }

    #[test]
    fn production_refuses_google_oauth() {
        let mut env = production_env();
        env.push(("GOOGLE_OAUTH_ENABLED", "true"));
        let errs = config(&env).validate_for_production().unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("GOOGLE_OAUTH_ENABLED"), "{errs:?}");
    }

    #[test]
    fn development_is_never_validated() {
        // The same barren environment that fails production is fine in dev.
        assert!(config(&[DB]).validate_for_production().is_ok());
    }

    // --- development defaults ----------------------------------------------

    #[test]
    fn development_with_no_provider_vars_selects_the_fakes() {
        let cfg = config(&[DB]);
        assert_eq!(cfg.app_env, AppEnv::Development);
        assert!(matches!(cfg.email, EmailConfig::Fake { .. }));
        assert_eq!(cfg.geocoder, GeocoderConfig::Fake);
        assert_eq!(cfg.rate_limiter.backend, RateLimiterBackend::InMemory);
        assert!(cfg.rate_limiter.fail_open);
        assert_eq!(cfg.base_url, DEV_BASE_URL);
        assert_eq!(cfg.storage.endpoint.as_deref(), Some(DEV_S3_ENDPOINT));
        assert_eq!(
            cfg.storage.public_endpoint.as_deref(),
            Some(DEV_S3_ENDPOINT)
        );
        assert_eq!(cfg.storage.access_key_id, DEV_S3_KEY);
        assert!(!cfg.google_oauth_enabled);
        assert_eq!(cfg.fakes_in_use().len(), 3);
    }

    #[test]
    fn public_s3_endpoint_can_differ_from_the_internal_endpoint() {
        let cfg = config(&[
            DB,
            ("S3_ENDPOINT", "http://minio:9000"),
            ("S3_PUBLIC_ENDPOINT", "http://localhost:9000"),
        ]);
        assert_eq!(cfg.storage.endpoint.as_deref(), Some("http://minio:9000"));
        assert_eq!(
            cfg.storage.public_endpoint.as_deref(),
            Some("http://localhost:9000")
        );
    }

    #[test]
    fn database_url_is_the_only_hard_requirement() {
        let err = Config::from_lookup(&lookup(&[])).expect_err("DATABASE_URL is required");
        assert!(matches!(err, ConfigError::MissingEnv("DATABASE_URL")));
    }

    // --- provider selection ------------------------------------------------

    /// A provider that was explicitly asked for but has no credentials is a
    /// startup error in EVERY environment — it never degrades to the fake,
    /// because a silent fake makes every verification email disappear.
    /// (Production additionally rejects the fake in `validate_for_production`.)
    #[test]
    fn resend_without_api_key_is_an_error_even_in_development() {
        let err = Config::from_lookup(&lookup(&[DB, ("EMAIL_PROVIDER", "resend")]))
            .expect_err("resend without a key must not fall back to the fake");
        assert!(matches!(err, ConfigError::MissingEnv("RESEND_API_KEY")));
    }

    #[test]
    fn mapbox_without_token_is_an_error() {
        let err = Config::from_lookup(&lookup(&[DB, ("GEOCODER", "mapbox")]))
            .expect_err("mapbox without a token must not fall back to the fake");
        assert!(matches!(
            err,
            ConfigError::MissingEnv("MAPBOX_GEOCODING_ACCESS_TOKEN")
        ));
    }

    #[test]
    fn google_profile_requires_server_and_browser_credentials() {
        let err = Config::from_lookup(&lookup(&[DB, ("LOCATION_PROVIDER", "google")]))
            .expect_err("Google needs its server key");
        assert!(matches!(
            err,
            ConfigError::MissingEnv("GOOGLE_MAPS_SERVER_API_KEY")
        ));

        let err = Config::from_lookup(&lookup(&[
            DB,
            ("LOCATION_PROVIDER", "google"),
            ("GOOGLE_MAPS_SERVER_API_KEY", "server-key"),
        ]))
        .expect_err("Google needs its browser key");
        assert!(matches!(
            err,
            ConfigError::MissingEnv("GOOGLE_MAPS_BROWSER_API_KEY")
        ));

        let err = Config::from_lookup(&lookup(&[
            DB,
            ("LOCATION_PROVIDER", "google"),
            ("GOOGLE_MAPS_SERVER_API_KEY", "server-key"),
            ("GOOGLE_MAPS_BROWSER_API_KEY", "browser-key"),
        ]))
        .expect_err("Google advanced markers need a map ID");
        assert!(matches!(err, ConfigError::MissingEnv("GOOGLE_MAP_ID")));
    }

    #[test]
    fn google_profile_selects_both_provider_halves() {
        let cfg = config(&[
            DB,
            ("LOCATION_PROVIDER", "google"),
            ("GOOGLE_MAPS_SERVER_API_KEY", "server-key"),
            ("GOOGLE_MAPS_BROWSER_API_KEY", "browser-key"),
            ("GOOGLE_MAP_ID", "map-id"),
        ]);
        assert_eq!(
            cfg.geocoder,
            GeocoderConfig::Google {
                api_key: "server-key".to_string()
            }
        );
        assert_eq!(
            cfg.map,
            MapConfig::Google {
                browser_api_key: "browser-key".to_string(),
                map_id: "map-id".to_string(),
            }
        );
    }

    #[test]
    fn mapbox_profile_selects_geocoder_and_map_style() {
        let cfg = config(&[
            DB,
            ("LOCATION_PROVIDER", "mapbox"),
            ("MAPBOX_GEOCODING_ACCESS_TOKEN", "geo-key"),
            ("MAPBOX_MAP_ACCESS_TOKEN", "map-key"),
            ("MAPBOX_STYLE_URL", "mapbox://styles/example/streets"),
        ]);
        assert!(matches!(
            cfg.geocoder,
            GeocoderConfig::Mapbox { token } if token == "geo-key"
        ));
        assert!(matches!(
            cfg.map,
            MapConfig::Mapbox { style_url, access_token }
                if style_url == "mapbox://styles/example/streets" && access_token == "map-key"
        ));
    }

    #[test]
    fn mapbox_profile_requires_a_separate_browser_token() {
        let err = Config::from_lookup(&lookup(&[
            DB,
            ("LOCATION_PROVIDER", "mapbox"),
            ("MAPBOX_GEOCODING_ACCESS_TOKEN", "geo-key"),
        ]))
        .expect_err("Mapbox GL needs a public browser token");
        assert!(matches!(
            err,
            ConfigError::MissingEnv("MAPBOX_MAP_ACCESS_TOKEN")
        ));
    }

    #[test]
    fn unknown_provider_names_are_rejected() {
        assert!(matches!(
            Config::from_lookup(&lookup(&[DB, ("EMAIL_PROVIDER", "sendgrid")])),
            Err(ConfigError::Invalid {
                key: "EMAIL_PROVIDER",
                ..
            })
        ));
        assert!(matches!(
            Config::from_lookup(&lookup(&[DB, ("GEOCODER", "nominatim")])),
            Err(ConfigError::Invalid {
                key: "GEOCODER",
                ..
            })
        ));
    }

    #[test]
    fn smtp_reads_its_whole_block() {
        let cfg = config(&[
            DB,
            ("EMAIL_PROVIDER", "smtp"),
            ("SMTP_HOST", "mail.example"),
            ("SMTP_PORT", "587"),
            ("SMTP_USERNAME", "u"),
            ("SMTP_PASSWORD", "p"),
            ("SMTP_TLS", "true"),
            ("EMAIL_FROM", "BikesNest <no-reply@example>"),
        ]);
        assert_eq!(
            cfg.email,
            EmailConfig::Smtp {
                host: "mail.example".to_string(),
                port: 587,
                username: "u".to_string(),
                password: "p".to_string(),
                tls: true,
                from: "BikesNest <no-reply@example>".to_string(),
            }
        );
    }

    #[test]
    fn valkey_cluster_wins_over_single_node() {
        let cfg = config(&[
            DB,
            ("VALKEY_URL", "valkey://one:6379"),
            ("VALKEY_CLUSTER_URLS", "valkey://a:6379, valkey://b:6379"),
            ("RATE_LIMIT_FAIL_OPEN", "false"),
        ]);
        assert_eq!(
            cfg.rate_limiter,
            RateLimiterConfig {
                backend: RateLimiterBackend::ValkeyCluster {
                    urls: vec!["valkey://a:6379".to_string(), "valkey://b:6379".to_string()],
                },
                fail_open: false,
            }
        );
    }

    // --- static assets ------------------------------------------------------

    #[test]
    fn static_root_is_relocatable() {
        let cfg = config(&[DB, ("STATIC_ROOT", "/srv/bikesnest/static")]);
        assert_eq!(cfg.static_root, PathBuf::from("/srv/bikesnest/static"));
        // Without the knob it still resolves to a directory that exists, so a
        // `cargo run` from the repo serves CSS/JS.
        assert!(config(&[DB]).static_root.is_dir());
    }

    // --- tuning knobs (defaults must not drift) -----------------------------

    #[test]
    fn recommendation_defaults_match_expected_values() {
        let got = config(&[DB]).recommendation;
        let exp = DEFAULT_RECOMMENDATION_CONFIG;
        assert_eq!(got.w_distance, exp.w_distance);
        assert_eq!(got.w_security, exp.w_security);
        assert_eq!(got.w_rating, exp.w_rating);
        assert_eq!(got.w_freshness, exp.w_freshness);
        assert_eq!(got.w_verification, exp.w_verification);
    }

    #[test]
    fn freshness_defaults_match_expected_values() {
        assert_eq!(
            config(&[DB]).freshness.thresholds,
            bikesnest_domain::DEFAULT_THRESHOLDS
        );
    }

    #[test]
    fn photo_defaults_match_expected_values() {
        let got = config(&[DB]).photo;
        assert_eq!(got.max_bytes, bikesnest_domain::MAX_PHOTO_BYTES);
        assert_eq!(got.max_megapixels, bikesnest_domain::MAX_PHOTO_MEGAPIXELS);
        assert_eq!(got.thumb_max_side, bikesnest_domain::THUMBNAIL_MAX_SIDE);
        assert_eq!(got.derivative_quality, bikesnest_domain::DERIVATIVE_QUALITY);
    }

    #[test]
    fn retention_defaults_match_policy() {
        let got = config(&[DB]).retention;
        let exp = RetentionPolicy::default();
        assert_eq!(got.session_idle, exp.session_idle);
        assert_eq!(got.parked_here_ttl, exp.parked_here_ttl);
        assert_eq!(got.export_ttl, exp.export_ttl);
    }

    #[test]
    fn moderation_defaults() {
        let m = config(&[DB]).moderation;
        assert_eq!(m.report_description_max_len, 1000);
        assert_eq!(m.report_create_user_limit, 10);
        assert_eq!(m.report_create_ip_limit, 20);
    }

    #[test]
    fn db_defaults_and_overrides() {
        assert_eq!(config(&[DB]).db, DbConfig::default());
        let cfg = config(&[
            DB,
            ("DB_MAX_CONNECTIONS", "40"),
            ("DB_STATEMENT_TIMEOUT_MS", "1500"),
            ("DB_IDLE_IN_TX_TIMEOUT_MS", "20000"),
        ]);
        assert_eq!(cfg.db.max_connections, 40);
        assert_eq!(cfg.db.statement_timeout, Duration::from_millis(1500));
        assert_eq!(cfg.db.idle_in_tx_timeout, Duration::from_secs(20));
        // Lifetimes are not env-driven; they stay on the defaults.
        assert_eq!(cfg.db.max_lifetime, DbConfig::default().max_lifetime);
        assert_eq!(cfg.db.idle_timeout, DbConfig::default().idle_timeout);
    }

    #[test]
    fn trusted_proxy_hops_defaults_to_zero_and_parses() {
        assert_eq!(config(&[DB]).trusted_proxy_hops, 0);
        assert_eq!(
            config(&[DB, ("TRUSTED_PROXY_HOPS", "2")]).trusted_proxy_hops,
            2
        );
        // Garbage falls back to the safe default (peer address only) rather
        // than trusting an unknown number of hops.
        assert_eq!(
            config(&[DB, ("TRUSTED_PROXY_HOPS", "lots")]).trusted_proxy_hops,
            0
        );
    }

    #[test]
    fn job_defaults_and_overrides() {
        assert_eq!(config(&[DB]).jobs, JobConfig::default());
        let cfg = config(&[DB, ("JOBS_ENABLED", "false"), ("JOBS_BATCH_SIZE", "16")]);
        assert!(!cfg.jobs.enabled);
        assert_eq!(cfg.jobs.batch_size, 16);
    }

    #[test]
    fn policy_placeholders_come_from_the_environment() {
        let cfg = config(&[
            DB,
            ("POLICY_VERSION", "2026-10-01.1"),
            ("POLICY_OPERATOR_NAME", "BikesNest Ltda."),
        ]);
        assert_eq!(cfg.policy.version, "2026-10-01.1");
        assert_eq!(
            cfg.policy.placeholder("OPERATOR_NAME").as_deref(),
            Some("BikesNest Ltda.")
        );
        assert!(cfg.policy.placeholder("CONTACT_EMAIL").is_none());
    }

    // --- map style ----------------------------------------------------------

    #[test]
    fn map_style_defaults_to_streets_with_matching_csp_origin() {
        let c = resolve_map_config(None, None, None);
        let MapConfig::MapLibre {
            style_url,
            access_token,
        } = c
        else {
            panic!("default map must use MapLibre");
        };
        assert_eq!(style_url, DEFAULT_MAP_STYLE_URL);
        assert_eq!(access_token, "");
        assert!(style_url.starts_with(&format!("{DEFAULT_TILE_HOST}/")));
    }

    #[test]
    fn mapbox_style_pulls_token() {
        let c = resolve_map_config(
            Some("mapbox://styles/u/s".to_string()),
            Some("public-map-tok".to_string()),
            Some("geo-tok".to_string()),
        );
        // The dedicated map token wins; the geocoder token is only a fallback.
        assert!(matches!(
            c,
            MapConfig::MapLibre { access_token, .. } if access_token == "public-map-tok"
        ));

        let fallback = resolve_map_config(
            Some("https://api.mapbox.com/styles/v1/u/s".to_string()),
            None,
            Some("geo-tok".to_string()),
        );
        assert!(matches!(
            fallback,
            MapConfig::MapLibre { access_token, .. } if access_token == "geo-tok"
        ));
    }

    #[test]
    fn non_mapbox_style_never_leaks_token() {
        // A non-Mapbox style (e.g. OpenFreeMap / a self-hosted style) must not
        // embed a Mapbox token even when one is configured for geocoding.
        let c = resolve_map_config(
            Some("https://tiles.example/style.json".to_string()),
            None,
            Some("geo-tok".to_string()),
        );
        assert!(matches!(
            c,
            MapConfig::MapLibre { access_token, .. } if access_token.is_empty()
        ));
        assert!(!is_mapbox_style("https://tiles.example/style.json"));
        assert!(is_mapbox_style("mapbox://styles/u/s"));
        assert!(is_mapbox_style("https://api.mapbox.com/styles/v1/u/s"));
    }

    #[test]
    fn test_config_is_a_development_config() {
        let cfg = Config::for_tests("postgres://localhost/test");
        assert_eq!(cfg.app_env, AppEnv::Development);
        assert!(cfg.validate_for_production().is_ok());
        assert!(matches!(cfg.email, EmailConfig::Fake { outbox_root: None }));
    }
}
