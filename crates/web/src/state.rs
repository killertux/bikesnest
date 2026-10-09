//! The shared state every handler reads from.
//!
//! One value, cloned per request: services, read-side ports and the
//! configuration-derived settings (map, security policy, base URL, asset
//! manifest). It is built once in [`crate::wiring`] — the only module that
//! knows which concrete provider backs each port.

use std::sync::Arc;

use async_trait::async_trait;
use bikesnest_application::{
    AuthService, CheckReadiness, CommunityParkingDetails, ContributionError, ContributionService,
    FreshnessConfig, GetParkingDetails, JobHealthService, ListingProposal, ModerationService,
    ObjectStorage, ParkingPhotoReader, PendingProposalSummary, PhotoService, PrivacyService,
    RateLimiter, ReaderError, ResolveDestination, SearchParking, SitemapReader, StoredPhoto,
};
use bikesnest_domain::{ParkingLocation, RevisionSummary, UserId};
use bikesnest_infrastructure::{Config, MapConfig};

use crate::security::SecurityHeaders;

#[async_trait]
pub trait DetailReads: Send + Sync {
    async fn photos_page(
        &self,
        id: i64,
        limit: i64,
    ) -> Result<(Vec<StoredPhoto>, i64), ReaderError>;
    async fn pending_photos(&self, id: i64) -> Result<i64, ReaderError>;
    async fn community(
        &self,
        location: ParkingLocation,
        viewer: Option<UserId>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<CommunityParkingDetails, ContributionError>;
    async fn summary(&self, id: i64) -> Result<PendingProposalSummary, ContributionError>;
    async fn proposals(
        &self,
        id: i64,
        after: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<ListingProposal>, i64, bool), ContributionError>;
    async fn history(
        &self,
        id: i64,
        after: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<RevisionSummary>, i64, bool), ContributionError>;
}

pub struct AppDetailReads {
    pub photos: Arc<dyn ParkingPhotoReader>,
    pub contributions: Arc<ContributionService>,
}

#[async_trait]
impl DetailReads for AppDetailReads {
    async fn photos_page(
        &self,
        id: i64,
        limit: i64,
    ) -> Result<(Vec<StoredPhoto>, i64), ReaderError> {
        self.photos.photos_page(id, limit).await
    }
    async fn pending_photos(&self, id: i64) -> Result<i64, ReaderError> {
        self.photos.pending_count(id).await
    }
    async fn community(
        &self,
        location: ParkingLocation,
        viewer: Option<UserId>,
        after: Option<i64>,
        limit: i64,
    ) -> Result<CommunityParkingDetails, ContributionError> {
        self.contributions
            .community_details_page(location, viewer, after, limit)
            .await
    }
    async fn summary(&self, id: i64) -> Result<PendingProposalSummary, ContributionError> {
        self.contributions.pending_proposal_summary(id).await
    }
    async fn proposals(
        &self,
        id: i64,
        after: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<ListingProposal>, i64, bool), ContributionError> {
        self.contributions
            .listing_proposals_page(id, after, limit)
            .await
    }
    async fn history(
        &self,
        id: i64,
        after: Option<i64>,
        limit: i64,
    ) -> Result<(Vec<RevisionSummary>, i64, bool), ContributionError> {
        self.contributions
            .revision_history_page(id, after, limit)
            .await
    }
}

/// Shared application state wired at startup. Everything configuration-derived
/// is resolved once here — no handler reads the process environment.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub readiness: Arc<CheckReadiness>,
    pub search: Arc<SearchParking>,
    /// Destination resolution under the per-IP geocode budget. It reads the
    /// same geocoder (and cache) the search use case calls, so `/search` can
    /// tell whether a destination is already resolved before charging for it.
    pub destinations: Arc<ResolveDestination>,
    /// Shared limiter store (the CSP report endpoint's per-IP cap).
    pub rate_limiter: Arc<dyn RateLimiter>,
    pub details: Arc<GetParkingDetails>,
    pub detail_reads: Arc<dyn DetailReads>,
    /* Configured freshness thresholds, used for display categorization so the
    cards honour the same tunable value as the search/detail services. */
    pub freshness: FreshnessConfig,
    pub photos: Arc<dyn ParkingPhotoReader>,
    /// The ids `/sitemap.xml` lists. A read-side port of its own, so the one
    /// page that needs "every public location" does not reach for the pool.
    pub sitemap: Arc<dyn SitemapReader>,
    pub storage: Arc<dyn ObjectStorage>,
    pub auth: Arc<AuthService>,
    pub contributions: Arc<ContributionService>,
    pub photo: Arc<PhotoService>,
    pub moderation: Arc<ModerationService>,
    pub privacy: Arc<PrivacyService>,
    /// Admin background-job health (recurring jobs, queue pressure).
    pub jobs: Arc<JobHealthService>,
    pub policy: Arc<dyn bikesnest_application::PolicyReader>,
    pub terms: Arc<dyn bikesnest_application::TermsAcknowledgementStore>,
    /// Security/CSP header policy, built once from the configured origins.
    pub security: SecurityHeaders,
    /// Client-side map style/token rendered into every page layout.
    pub map: MapConfig,
    /// Public origin absolute links are built from (canonical URLs, sitemap).
    pub base_url: String,
    /// Google sign-in feature flag (disabled until a real OAuth provider exists).
    pub google_oauth_enabled: bool,
    /// Content-hash manifest for `/static/...`: logical path → hash,
    /// computed once at startup by `crate::assets::init`. Backs the
    /// `/static/h/{hash}/{*path}` handler; `PageLayout::asset()` resolves
    /// URLs from the same manifest.
    pub assets: crate::assets::AssetManifest,
}
