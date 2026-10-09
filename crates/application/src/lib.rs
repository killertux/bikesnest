//! BikesNest application crate: use cases and ports.
//!
//! Depends only on the domain. Infrastructure implements the ports defined
//! here (dependency points inward).

use async_trait::async_trait;
use bikesnest_domain::DomainError;

pub mod audit;
pub mod auth;
pub mod community;
pub mod destination;
pub mod email;
pub mod job_health;
pub mod jobs;
pub mod moderation;
pub mod photo;
pub mod ports;
pub mod privacy;
pub mod rate_limit;
pub mod search;
pub mod storage;
pub mod timezone;

pub use audit::{
    AuditError, AuditEvent, AuditFilter, AuditLog, AuditLogReader, AuditPage, AuditStoredEvent,
};
pub use auth::{
    AccountRepository, AdmittedAuthMail, AuthError, AuthMailDispatcher, AuthOutbox, AuthService,
    AuthenticatedUser, Clock, EmailConfirmationOutcome, EmailVerificationOutcome, IdentityRecord,
    LoginOutcome, NewAccount, OAuthProvider, PasswordHasher, ResolvedSession, Session,
    SessionStore, TermsAcceptance, TokenGenerator, TokenStore, UserActivity, UserSearch,
    VerificationPurpose,
};
pub use community::{
    AddParkingLocationOutcome, AttributeSummary, CommunityParkingDetails, ContributionDeps,
    ContributionError, ContributionHistoryReader, ContributionItem, ContributionService,
    DuplicateCandidate, FavoriteItem, FavoriteRepository, ListingProposal, NewParkingLocation,
    NewProposal, NewVerification, ParkingContributionRepository, ParkingEdit, PendingFieldCue,
    PendingProposalSummary, ProposalVote, ProposalVoteTotals, Reason, Review, ReviewRepository,
    VerificationRepository, recommendation_reasons,
};
pub use destination::{DestinationError, GeocodeBudget, ResolveDestination};
pub use email::{EmailError, EmailKind, EmailMessage, EmailProvider, EmailQueue};
pub use job_health::{
    JOB_LATE_AFTER_SECS, JobHealth, JobHealthError, JobHealthReader, JobHealthReport,
    JobHealthService, JobQueueSummary, RecurringJobStatus,
};
pub use jobs::{JOB_EMAIL_SEND, JOB_JOBS_GC, JOB_RETENTION, JobError, JobHandler, JobPayload};
pub use moderation::{
    ModerationDeps, ModerationError, ModerationRepository, ModerationService, NewReport, Proposal,
    ProposalApplication, ProposalField, ProposalOverride, QueueCounts, REVIEW_EXCERPT_CHARS,
    Report, ReportRepository, ReportTargetPreview, review_excerpt,
};
pub use photo::{
    ImageProcessor, NewPendingPhoto, PendingPhoto, PhotoDeps, PhotoError, PhotoForModeration,
    PhotoKind, PhotoRepository, PhotoService, PhotoTarget, ProcessedImage, RejectedPhoto,
    UploadedPhoto,
};
pub use ports::{
    AddressSuggestion, BROWSE_LIST_CAP, BROWSE_MARKER_CAP, BoundsPage, BoundsQuery, Cluster,
    CostFilter, Cursor, Filters, FreshnessConfig, GeoHit, GeocodeError, Geocoder,
    ParkingDetailsReader, ParkingPhotoReader, ParkingSearchReader, ParkingSummary, ReaderError,
    ReviewPhotosReader, SearchInput, SearchPage, SearchRequest, SitemapReader, Sort, StoredPhoto,
};
pub use privacy::{
    AnonymizationReport, AnonymizationRepository, Export, ExportAccount, ExportDownload,
    ExportFavorite, ExportPayload, ExportPhoto, ExportProposal, ExportProposalVote, ExportProvider,
    ExportReport, ExportRepository, ExportRequested, ExportReview, ExportReviewRevision,
    ExportSession, ExportTermsAcknowledgement, ExportTermsPresentation, ExportVerification,
    NewExport, NewPrivacyRequest, POLICY_FALLBACK_LOCALE, PendingTermsNotice, PolicyDocument,
    PolicyReader, PrivacyDeps, PrivacyError, PrivacyRequest, PrivacyRequestRepository,
    PrivacyService, RetentionConfig, RetentionJob, RetentionRepository, RetentionStep,
    RetentionSummary, TermsAcknowledgementStore, TermsProof,
};
pub use rate_limit::{RateLimitError, RateLimiter};
pub use search::{
    DEFAULT_RECOMMENDATION_CONFIG, DetailsError, GetParkingDetails, ParkingDetailsView,
    RecommendationConfig, SearchError, SearchParking,
};
pub use storage::{ObjectInfo, ObjectPage, ObjectStorage, PutObject, StorageError};
pub use timezone::{TimezoneError, TimezoneResolver};

/// Port: probe a required dependency (initially, the database).
#[async_trait]
pub trait DatabaseProbe: Send + Sync {
    /// Returns `Ok(())` when the dependency is reachable and responsive.
    async fn ping(&self) -> Result<(), ProbeError>;
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// The dependency is unreachable / timed out (readiness must report
    /// "dependency down", distinct from an application bug).
    #[error("dependency unavailable")]
    Unavailable,
    /// An unexpected error on our side (maps to 5xx app error).
    #[error("probe failed unexpectedly")]
    Unexpected,
}

impl From<DomainError> for ProbeError {
    fn from(_: DomainError) -> Self {
        // Domain errors are not expected from probes; mapped for completeness.
        ProbeError::Unexpected
    }
}

/// Outcome of the readiness use case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Readiness {
    Ready,
    DependencyDown,
    AppError,
}

/// A boxed probe is a probe, so the web layer can hold the readiness use case
/// without naming the concrete adapter behind it.
#[async_trait]
impl<T: DatabaseProbe + ?Sized> DatabaseProbe for Box<T> {
    async fn ping(&self) -> Result<(), ProbeError> {
        (**self).ping().await
    }
}

/// Use case: can the application serve requests and access its dependencies?
pub struct CheckReadiness<P = Box<dyn DatabaseProbe>> {
    probe: P,
}

impl<P: DatabaseProbe> CheckReadiness<P> {
    pub fn new(probe: P) -> Self {
        Self { probe }
    }

    pub async fn execute(&self) -> Readiness {
        match self.probe.ping().await {
            Ok(()) => Readiness::Ready,
            Err(ProbeError::Unavailable) => Readiness::DependencyDown,
            Err(ProbeError::Unexpected) => Readiness::AppError,
        }
    }
}
