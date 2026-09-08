//! Community contribution use cases.
//!
//! Ports + read models + [`ContributionService`]. Infrastructure implements the
//! ports; the web layer calls the service for every contribution action. The
//! verified-email gate, rate limiting, optimistic concurrency, and the
//! confidence rule all live here.

use crate::audit::{AuditEvent, AuditLog};
use crate::auth::Clock;
use crate::ports::{FreshnessConfig, ReaderError, ReviewPhotosReader, StoredPhoto};
use crate::rate_limit::{RateLimitError, RateLimiter};
use crate::timezone::{TimezoneError, TimezoneResolver};
use async_trait::async_trait;
use bikesnest_domain::{
    AttributeResult, Confidence, Cost, ExistenceResult, ExistenceSignal, GeoPoint, ModerationState,
    OpeningHours, ParkingLocation, ParkingType, ReviewBody, RevisionSummary, SecurityFeature,
    StarRating, UserId,
};
use chrono::{DateTime, Utc};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ContributionError {
    /// The session principal has not verified their email (the  gate).
    #[error("verify your email to contribute")]
    NotVerified,
    #[error("too many attempts, try again later")]
    RateLimited,
    /// Optimistic concurrency: the expected `version` no longer matches.
    #[error("someone else changed this recently; reload and try again")]
    VersionConflict,
    #[error("not found")]
    NotFound,
    /// Moderation took the location down (REMOVED / INVALID / FLAGGED /
    /// PENDING_REVIEW): it accepts no further contributions.
    #[error("this spot is no longer accepting contributions")]
    LocationNotActive,
    #[error("invalid input: {0}")]
    InvalidField(String),
    #[error("you are not permitted to perform this action")]
    Unauthorized,
    #[error("timezone could not be resolved")]
    Timezone,
    /// Storage refused a duplicate, or a concurrent writer won the race
    /// (unique violation, serialization failure, deadlock).
    #[error("that change conflicts with an existing record")]
    Conflict,
    /// Storage is unreachable or overloaded; the same request may work shortly.
    #[error("service temporarily unavailable")]
    Unavailable,
    /// Anything unexpected / storage-side. Never leaks details.
    #[error("internal error")]
    Internal,
}

impl From<RateLimitError> for ContributionError {
    fn from(_: RateLimitError) -> Self {
        ContributionError::RateLimited
    }
}

impl From<crate::audit::AuditError> for ContributionError {
    fn from(_: crate::audit::AuditError) -> Self {
        ContributionError::Internal
    }
}

impl From<bikesnest_domain::DomainError> for ContributionError {
    fn from(e: bikesnest_domain::DomainError) -> Self {
        ContributionError::InvalidField(e.to_string())
    }
}

impl From<TimezoneError> for ContributionError {
    fn from(_: TimezoneError) -> Self {
        ContributionError::Timezone
    }
}

impl From<ReaderError> for ContributionError {
    fn from(_: ReaderError) -> Self {
        ContributionError::Internal
    }
}

// ---------------------------------------------------------------------------
// Read models
// ---------------------------------------------------------------------------

/// A new parking location, awaiting persistence. `timezone` is optional — the
/// service auto-derives it from the pin when absent (override is allowed).
#[derive(Debug, Clone)]
pub struct NewParkingLocation {
    pub name: String,
    pub address: String,
    pub description: Option<String>,
    pub parking_type: ParkingType,
    pub cost: Cost,
    pub point: GeoPoint,
    pub timezone: Option<chrono_tz::Tz>,
    pub hours: OpeningHours,
    pub security: Vec<SecurityFeature>,
}

pub use bikesnest_domain::ParkingEdit;

/// A gated listing change. `proposed` carries the typed edit, move, or existence
/// payload; publication belongs to the approval workflow.
#[derive(Debug, Clone)]
pub struct NewProposal {
    pub location_id: i64,
    pub proposer_id: UserId,
    pub base_version: i64,
    pub kind: bikesnest_domain::ProposalKind,
    pub proposed: serde_json::Value,
}

/// A voter's one mutable position on a pending proposal.  It intentionally
/// carries no identity in public read models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalVote {
    Approve,
    Reject,
}

impl ProposalVote {
    pub fn as_code(self) -> &'static str {
        match self {
            Self::Approve => "APPROVE",
            Self::Reject => "REJECT",
        }
    }
    pub fn from_code(value: &str) -> Option<Self> {
        match value {
            "approve" | "APPROVE" => Some(Self::Approve),
            "reject" | "REJECT" => Some(Self::Reject),
            _ => None,
        }
    }
}

/// Public-safe totals returned after a vote update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProposalVoteTotals {
    pub approvals: i64,
    pub rejections: i64,
    pub published: bool,
}

/// Privacy-safe proposal card data for a listing. It deliberately omits voter
/// ids, email, and any media key.
#[derive(Debug, Clone)]
pub struct ListingProposal {
    pub id: i64,
    pub kind: bikesnest_domain::ProposalKind,
    pub change: bikesnest_domain::ProposedChange,
    pub reason: Option<String>,
    pub status: bikesnest_domain::ProposalStatus,
    pub approvals: i64,
    pub rejections: i64,
    pub created_at: DateTime<Utc>,
    pub base_version: i64,
    pub proposer_id: Option<UserId>,
}

/// An advisory duplicate candidate. Non-blocking; ranked by
/// name-similarity + address overlap.
#[derive(Debug, Clone)]
pub struct DuplicateCandidate {
    pub id: i64,
    pub name: String,
    pub address: String,
    pub distance_m: f64,
    pub similarity: f64,
}

/// A review as read from the store (only `ACTIVE` rows are ever returned).
/// `author` is `None` once the reviewer's account is anonymized.
#[derive(Debug, Clone)]
pub struct Review {
    pub id: i64,
    pub location_id: i64,
    pub author: Option<UserId>,
    /// Already privacy-filtered; never derive a public label from an account ID.
    pub public_author_name: Option<String>,
    pub rating: StarRating,
    pub body: ReviewBody,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-attribute verification tallies ( /  — disputes shown, never averaged).
#[derive(Debug, Clone)]
pub struct AttributeSummary {
    pub code: String,
    pub correct: i64,
    pub incorrect: i64,
}

/// A verification signal to record, typed so the service can validate + choose
/// the rate-limit key and (for `still_exists`) trigger the freshness update.
#[derive(Debug, Clone)]
pub enum NewVerification {
    Existence {
        location_id: i64,
        user_id: UserId,
        result: ExistenceResult,
    },
    Attribute {
        location_id: i64,
        user_id: UserId,
        code: String,
        result: AttributeResult,
    },
    ParkedHere {
        location_id: i64,
        user_id: UserId,
    },
}

impl NewVerification {
    pub fn location_id(&self) -> i64 {
        match self {
            NewVerification::Existence { location_id, .. }
            | NewVerification::Attribute { location_id, .. }
            | NewVerification::ParkedHere { location_id, .. } => *location_id,
        }
    }
    pub fn user_id(&self) -> UserId {
        match self {
            NewVerification::Existence { user_id, .. }
            | NewVerification::Attribute { user_id, .. }
            | NewVerification::ParkedHere { user_id, .. } => *user_id,
        }
    }
    pub fn is_parked_here(&self) -> bool {
        matches!(self, NewVerification::ParkedHere { .. })
    }
    /// Whether this positive existence confirmation should refresh freshness.
    pub fn is_still_exists(&self) -> bool {
        matches!(
            self,
            NewVerification::Existence {
                result: ExistenceResult::StillExists,
                ..
            }
        )
    }
}

/// One row of a user's favorites list — the location id plus the timestamp
/// the keyset cursor paginates on (recency: most recently favorited first).
#[derive(Debug, Clone, Copy)]
pub struct FavoriteItem {
    pub location_id: i64,
    pub created_at: DateTime<Utc>,
}

/// One row of the contribution history contribution-history feed.
#[derive(Debug, Clone)]
pub struct ContributionItem {
    /// "added" | "edited" | "proposed" | "reviewed" | "verified" |
    /// "parked_here" | "favorited" | "photo.pending"
    pub kind: String,
    /// The affected location name (or id when a name is unavailable).
    pub target: String,
    /// A machine code for the current state: "active" | "pending" | "history"…
    pub state: String,
    pub at: DateTime<Utc>,
    /// Opaque keyset-cursor value for this row (paired with `at`). Not a
    /// foreign key into any one table — the feed unions eight heterogeneous
    /// sources, so this is a per-source id encoded to sort consistently.
    pub id: i64,
}

/// The extended detail view (reviews, confidence, verification, favorite,
/// recommendation explanation) produced by [`ContributionService::community_details`].
#[derive(Debug, Clone)]
pub struct CommunityParkingDetails {
    pub location: ParkingLocation,
    pub reviews: Vec<Review>,
    /// Approved review photos, keyed by review id — only APPROVED render.
    pub review_photos: std::collections::HashMap<i64, Vec<StoredPhoto>>,
    pub confidence: Confidence,
    pub disputed: bool,
    pub attribute_summary: Vec<AttributeSummary>,
    pub parked_here_count: i64,
    pub is_favorited: bool,
    pub own_review: Option<Review>,
    pub own_verification: Option<ExistenceSignal>,
    pub reasons: Vec<Reason>,
}

/// One reason in the "recommended because…" block. Only positive
/// factors are ever surfaced; missing data yields no claim.
#[derive(Debug, Clone)]
pub struct Reason {
    /// "distance" | "security" | "rating" | "freshness" | "verification"
    pub factor: &'static str,
    /// i18n key resolved by the web layer.
    pub label_key: &'static str,
    /// Location-specific detail (e.g. "350 m", "4.5", "3 security features").
    pub detail: String,
}

/// Outcome of [`ContributionService::add_parking_location`]: the new id plus
/// the advisory duplicate warnings the handler surfaces.
#[derive(Debug, Clone)]
pub struct AddParkingLocationOutcome {
    pub id: i64,
    pub duplicates: Vec<DuplicateCandidate>,
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ParkingContributionRepository: Send + Sync {
    async fn get_for_edit(&self, id: i64) -> Result<Option<ParkingLocation>, ContributionError>;
    /// Insert the location (+ security + revision v1) in one transaction.
    async fn create(
        &self,
        new: &NewParkingLocation,
        creator: UserId,
        now: DateTime<Utc>,
    ) -> Result<i64, ContributionError>;
    /// Atomic optimistic apply: `UPDATE … WHERE id = $ AND version = $expected`
    /// bumps `version` and writes the revision. 0 rows → `VersionConflict`.
    async fn apply_edit(
        &self,
        id: i64,
        expected_version: i64,
        edit: &ParkingEdit,
        editor: UserId,
        now: DateTime<Utc>,
    ) -> Result<i64, ContributionError>;
    async fn create_proposal(&self, p: &NewProposal) -> Result<i64, ContributionError>;
    /// Upsert one vote and return only aggregate totals. Implementations must
    /// reject self votes and ineligible/deactivated accounts atomically.
    async fn vote_on_proposal(
        &self,
        _proposal_id: i64,
        _voter: UserId,
        _vote: ProposalVote,
    ) -> Result<ProposalVoteTotals, ContributionError> {
        Err(ContributionError::Unauthorized)
    }
    async fn listing_proposals(
        &self,
        _location_id: i64,
        _limit: i64,
    ) -> Result<Vec<ListingProposal>, ContributionError> {
        Ok(Vec::new())
    }
    /// Newest `limit` revisions (`version DESC`) — bounded, not the full
    /// history (a location edited often would otherwise return every version).
    async fn revision_history(
        &self,
        id: i64,
        limit: i64,
    ) -> Result<Vec<RevisionSummary>, ContributionError>;
    async fn duplicate_candidates(
        &self,
        point: GeoPoint,
        name: &str,
    ) -> Result<Vec<DuplicateCandidate>, ContributionError>;
}

#[async_trait]
pub trait ReviewRepository: Send + Sync {
    /// Insert-or-update + append `review_revision` + recompute the location
    /// rating aggregate, all in one transaction. Returns `true` when an
    /// existing review was updated, `false` when one was created.
    async fn upsert_review(
        &self,
        location_id: i64,
        author: UserId,
        rating: StarRating,
        body: &ReviewBody,
    ) -> Result<bool, ContributionError>;
    async fn find_own(
        &self,
        location_id: i64,
        author: UserId,
    ) -> Result<Option<Review>, ContributionError>;
    /// Only `ACTIVE` reviews are public. Keyset-paginated, newest first:
    /// `after_id` is the last review id from the previous page (`None` for the
    /// first page); `limit` is clamped by the implementation.
    async fn list_active(
        &self,
        location_id: i64,
        after_id: Option<i64>,
        limit: i64,
    ) -> Result<Vec<Review>, ContributionError>;
}

#[async_trait]
pub trait VerificationRepository: Send + Sync {
    /// `now` is supplied (from the service's `Clock`) so `expires_at` and the
    /// freshness update use one deterministic timestamp.
    async fn record(
        &self,
        signal: &NewVerification,
        now: DateTime<Utc>,
    ) -> Result<(), ContributionError>;
    /// Latest existence verification per user (deduped), ordered by time.
    async fn latest_existence_per_user(
        &self,
        location_id: i64,
    ) -> Result<Vec<ExistenceSignal>, ContributionError>;
    /// One-query fold of the per-attribute tally and the parked-here count:
    /// both read `verification` for this location, differing only in `kind`,
    /// so a single statement (two `FILTER`-aggregated subqueries joined on
    /// `true`) answers both instead of two round trips.
    async fn attribute_and_parked_summary(
        &self,
        location_id: i64,
    ) -> Result<(Vec<AttributeSummary>, i64), ContributionError>;
    async fn mark_verified_at(
        &self,
        location_id: i64,
        at: DateTime<Utc>,
    ) -> Result<(), ContributionError>;
}

#[async_trait]
pub trait FavoriteRepository: Send + Sync {
    /// Returns `true` when the location is now favorited.
    async fn toggle(&self, user: UserId, location_id: i64) -> Result<bool, ContributionError>;
    async fn is_favorited(&self, user: UserId, location_id: i64)
    -> Result<bool, ContributionError>;
    /// Keyset-paginated newest first (`created_at DESC, location_id DESC` —
    /// most recently favorited first). `after` is the `(created_at,
    /// location_id)` of the last item on the previous page. Returns
    /// `created_at` alongside each id (not just `Vec<i64>`) so the caller can
    /// build the next page's cursor without a second read.
    async fn list(
        &self,
        user: UserId,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<FavoriteItem>, ContributionError>;
}

/// contribution history read-model: aggregate all of a user's contributions into one feed.
#[async_trait]
pub trait ContributionHistoryReader: Send + Sync {
    /// Keyset-paginated, newest first (`at DESC`, ties broken by `id`).
    /// `after` is the `(at, id)` of the last item on the previous page; `id`
    /// is an opaque per-source cursor value, not a foreign key into any one
    /// table (the feed unions eight heterogeneous sources).
    async fn history(
        &self,
        user: UserId,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<ContributionItem>, ContributionError>;
}

// ---------------------------------------------------------------------------
// Recommendation explanation
// ---------------------------------------------------------------------------

/// Build the "recommended because…" reasons for a single summary row. Mirrors
/// the sub-scores of the `Recommended` sort key (computed in SQL by the search
/// reader, weighted by [`crate::RecommendationConfig`]), so the explanation and
/// the numeric sort never disagree. Only **positive** factors are surfaced;
/// missing/neutral data → the factor is omitted (never a fabricated claim).
#[allow(clippy::too_many_arguments)]
pub fn recommendation_reasons(
    item: &crate::ports::ParkingSummary,
    radius_m: u32,
    origin: Option<GeoPoint>,
    now: DateTime<Utc>,
    freshness: &FreshnessConfig,
) -> Vec<Reason> {
    let mut reasons = Vec::new();

    // Distance — only surfaces when the request carries an origin.
    if origin.is_some() {
        let d = (item.distance_m / radius_m as f64).clamp(0.0, 1.0);
        let distance_score = 1.0 - d;
        // "Positive" = closer than the radius boundary would suggest.
        if distance_score >= 0.4 {
            reasons.push(Reason {
                factor: "distance",
                label_key: "reason.distance",
                detail: distance_label(item.distance_m),
            });
        }
    }

    let yes = item.security_yes.len() as f64;
    if yes > 0.0 {
        reasons.push(Reason {
            factor: "security",
            label_key: "reason.security",
            detail: format!("{yes}"),
        });
    }

    if let Some(avg) = item.rating.avg()
        && avg >= 3.5
    {
        reasons.push(Reason {
            factor: "rating",
            label_key: "reason.rating",
            detail: format!("{avg:.1}"),
        });
    }

    let cat = bikesnest_domain::categorize(item.last_verified_at, now, &freshness.thresholds);
    match cat {
        bikesnest_domain::FreshnessCategory::Fresh
        | bikesnest_domain::FreshnessCategory::RecentlyVerified => {
            reasons.push(Reason {
                factor: "freshness",
                label_key: "reason.freshness",
                detail: String::new(),
            });
        }
        _ => {}
    }

    if item.last_verified_at.is_some() {
        reasons.push(Reason {
            factor: "verification",
            label_key: "reason.verification",
            detail: String::new(),
        });
    }

    reasons
}

fn distance_label(m: f64) -> String {
    if m < 1000.0 {
        format!("{m:.0} m")
    } else {
        format!("{:.1} km", m / 1000.0)
    }
}

// ---------------------------------------------------------------------------
// Rate-limit defaults. Keys are `contribution:{kind}:user:{id}` and, for
// parking-create, `:ip:{ip}`. Documented for tuning.
// ---------------------------------------------------------------------------

const PARKING_CREATE_USER_LIMIT: u32 = 5;
const PARKING_CREATE_IP_LIMIT: u32 = 10;
const EDIT_USER_LIMIT: u32 = 15;
const PROPOSAL_USER_LIMIT: u32 = 5;
const REVIEW_USER_LIMIT: u32 = 10;
const VERIFICATION_USER_LIMIT: u32 = 30;
const PARKED_HERE_USER_LIMIT: u32 = 20;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
const HOUR: Duration = Duration::from_secs(60 * 60);

/// The details page shows at most this many reviews inline, newest first
/// (no "load more" control — see [`ContributionService::community_details`]).
const DETAILS_REVIEW_LIMIT: i64 = 50;

// ---------------------------------------------------------------------------
// ContributionService
// ---------------------------------------------------------------------------

/// Everything the contribution use cases depend on, bundled for construction.
pub struct ContributionDeps {
    pub tz: Box<dyn TimezoneResolver>,
    pub contributions: Box<dyn ParkingContributionRepository>,
    pub reviews: Box<dyn ReviewRepository>,
    pub verifications: Box<dyn VerificationRepository>,
    pub favorites: Box<dyn FavoriteRepository>,
    pub history: Box<dyn ContributionHistoryReader>,
    pub review_photos: Box<dyn ReviewPhotosReader>,
    pub rate_limiter: Box<dyn RateLimiter>,
    pub audit: Box<dyn AuditLog>,
    pub clock: Box<dyn Clock>,
    pub freshness: FreshnessConfig,
}

pub struct ContributionService {
    deps: ContributionDeps,
}

impl ContributionService {
    pub fn new(deps: ContributionDeps) -> Self {
        Self { deps }
    }

    fn now(&self) -> DateTime<Utc> {
        self.deps.clock.now()
    }

    fn require_verified(
        &self,
        user: &crate::auth::AuthenticatedUser,
    ) -> Result<(), ContributionError> {
        if user.is_verified {
            Ok(())
        } else {
            Err(ContributionError::NotVerified)
        }
    }

    async fn allowed(
        &self,
        key: &str,
        limit: u32,
        window: Duration,
    ) -> Result<(), ContributionError> {
        if self.deps.rate_limiter.check(key, limit, window).await? {
            Ok(())
        } else {
            Err(ContributionError::RateLimited)
        }
    }

    /// Every contribution write targets a *live* spot. A location moderation
    /// has taken down (REMOVED / INVALID / FLAGGED / PENDING_REVIEW) is already
    /// invisible to the public read path, so it must not keep accepting edits,
    /// proposals, reviews or verifications either — a `still_exists` signal on
    /// a removed spot would otherwise reset its freshness.
    ///
    /// Favorites are deliberately NOT gated: a favorite is a private bookmark,
    /// not a contribution, and refusing to un-favorite a removed spot would
    /// strand rows in the user's own list.
    ///
    /// This is the friendly, early check; the repositories re-check inside their
    /// transactions so a state flip mid-request cannot slip a write through.
    async fn require_active(&self, id: i64) -> Result<ParkingLocation, ContributionError> {
        let location = self
            .deps
            .contributions
            .get_for_edit(id)
            .await?
            .ok_or(ContributionError::NotFound)?;
        if location.moderation_state() != ModerationState::Active {
            return Err(ContributionError::LocationNotActive);
        }
        Ok(location)
    }

    // -----------------------------------------------------------------------
    // Add / edit / propose (–)
    // -----------------------------------------------------------------------

    pub async fn add_parking_location(
        &self,
        user: &crate::auth::AuthenticatedUser,
        ip: &str,
        mut input: NewParkingLocation,
    ) -> Result<AddParkingLocationOutcome, ContributionError> {
        self.require_verified(user)?;
        self.allowed(
            &format!("contribution:parking-create:user:{}", user.id.0),
            PARKING_CREATE_USER_LIMIT,
            DAY,
        )
        .await?;
        self.allowed(
            &format!("contribution:parking-create:ip:{ip}"),
            PARKING_CREATE_IP_LIMIT,
            DAY,
        )
        .await?;

        validate_name_address(&input.name, &input.address)?;
        // Auto-derive the timezone when the form did not provide one.
        if input.timezone.is_none() {
            input.timezone = Some(self.deps.tz.resolve(input.point).await?);
        }

        // The safety net behind the web layer's pre-create interstitial:
        // a similar spot created between that check and this insert is still
        // reported, now as an advisory on a row that exists.
        let duplicates = self.duplicates_for(input.point, &input.name).await?;

        let id = self
            .deps
            .contributions
            .create(&input, user.id, self.now())
            .await?;
        self.audit(
            Some(user.id),
            "parking.created",
            "parking_location",
            id.to_string(),
            serde_json::json!({ "name": input.name }),
        )
        .await?;

        Ok(AddParkingLocationOutcome { id, duplicates })
    }

    /// The duplicate check on its own, so a contributor can be shown the
    /// candidates *before* a spot is created.
    ///
    /// "You added spot 8380 — by the way, it may already be listed" is not a
    /// warning, it is a duplicate: the row is already live and the only way out
    /// is a second, manual edit. Running the same query first turns that into a
    /// decision, and [`Self::add_parking_location`] keeps its own check for the
    /// race between the two requests.
    ///
    /// Only `point` and `name` are compared: the query already matches the
    /// submitted name against each candidate's *name and address*, so passing
    /// the submitted address as well would add no signal.
    pub async fn find_duplicates(
        &self,
        user: &crate::auth::AuthenticatedUser,
        point: GeoPoint,
        name: &str,
    ) -> Result<Vec<DuplicateCandidate>, ContributionError> {
        self.require_verified(user)?;
        self.duplicates_for(point, name).await
    }

    async fn duplicates_for(
        &self,
        point: GeoPoint,
        name: &str,
    ) -> Result<Vec<DuplicateCandidate>, ContributionError> {
        self.deps
            .contributions
            .duplicate_candidates(point, name)
            .await
    }

    /// Submit an edit for approval. Returns the proposal id; never publishes.
    pub async fn apply_parking_edit(
        &self,
        user: &crate::auth::AuthenticatedUser,
        id: i64,
        expected_version: i64,
        edit: &ParkingEdit,
    ) -> Result<i64, ContributionError> {
        self.require_verified(user)?;
        self.allowed(
            &format!("contribution:edit:user:{}", user.id.0),
            EDIT_USER_LIMIT,
            HOUR,
        )
        .await?;
        validate_name_address(&edit.name, &edit.address)?;
        let current = self.require_active(id).await?;
        if current.version() != expected_version {
            return Err(ContributionError::VersionConflict);
        }
        let proposed = edit.to_json();
        if ParkingEdit::from_json(&proposed).is_none() {
            return Err(ContributionError::InvalidField("invalid edit".into()));
        }
        let proposal_id = self
            .deps
            .contributions
            .create_proposal(&NewProposal {
                location_id: id,
                proposer_id: user.id,
                base_version: expected_version,
                kind: bikesnest_domain::ProposalKind::EditDetails,
                proposed,
            })
            .await?;
        self.audit(
            Some(user.id),
            "parking.proposal_created",
            "parking_proposal",
            proposal_id.to_string(),
            serde_json::json!({ "kind": "edit_details" }),
        )
        .await?;
        Ok(proposal_id)
    }

    /// Creates a `PENDING` sensitive-change proposal. No live change.
    pub async fn propose_location_change(
        &self,
        user: &crate::auth::AuthenticatedUser,
        id: i64,
        kind: bikesnest_domain::ProposalKind,
        proposed: serde_json::Value,
    ) -> Result<i64, ContributionError> {
        self.require_verified(user)?;
        self.allowed(
            &format!("contribution:proposal:user:{}", user.id.0),
            PROPOSAL_USER_LIMIT,
            HOUR,
        )
        .await?;

        let current = self.require_active(id).await?;
        let mut payload = bikesnest_domain::ProposalPayload::from_json(kind, &proposed);
        if payload.change == bikesnest_domain::ProposedChange::Unknown {
            return Err(ContributionError::InvalidField("invalid proposal".into()));
        }
        if let bikesnest_domain::ProposedChange::MoveLocation { lat, lon, timezone } =
            &mut payload.change
        {
            *timezone = Some(
                match timezone.as_deref() {
                    Some(tz) => tz
                        .parse::<chrono_tz::Tz>()
                        .map_err(|_| ContributionError::Timezone)?,
                    None => self.deps.tz.resolve(GeoPoint::new(*lat, *lon)?).await?,
                }
                .name()
                .to_string(),
            );
        }
        let proposal = NewProposal {
            location_id: id,
            proposer_id: user.id,
            base_version: current.version(),
            kind,
            proposed: payload.to_json(),
        };
        let proposal_id = self.deps.contributions.create_proposal(&proposal).await?;
        self.audit(
            Some(user.id),
            "parking.proposal_created",
            "parking_proposal",
            proposal_id.to_string(),
            serde_json::json!({ "kind": kind.as_code() }),
        )
        .await?;
        Ok(proposal_id)
    }

    /// Switches (or withdraws by choosing the other position) a participant's
    /// single vote. Eligibility is checked again by the transactional store so
    /// a suspended/deleted account cannot race an old session.
    pub async fn vote_on_proposal(
        &self,
        user: &crate::auth::AuthenticatedUser,
        proposal_id: i64,
        vote: ProposalVote,
    ) -> Result<ProposalVoteTotals, ContributionError> {
        self.require_verified(user)?;
        let totals = self
            .deps
            .contributions
            .vote_on_proposal(proposal_id, user.id, vote)
            .await?;
        if totals.published {
            self.audit(
                Some(user.id),
                "proposal.approved",
                "parking_proposal",
                proposal_id.to_string(),
                serde_json::json!({"decision":"community", "approvals":totals.approvals}),
            )
            .await?;
        }
        self.audit(
            Some(user.id),
            "parking.proposal_voted",
            "parking_proposal",
            proposal_id.to_string(),
            serde_json::json!({ "vote": vote.as_code() }),
        )
        .await?;
        Ok(totals)
    }

    pub async fn listing_proposals(
        &self,
        location_id: i64,
    ) -> Result<Vec<ListingProposal>, ContributionError> {
        self.deps
            .contributions
            .listing_proposals(location_id, 50)
            .await
    }

    pub async fn revision_history(
        &self,
        location_id: i64,
    ) -> Result<Vec<RevisionSummary>, ContributionError> {
        self.deps
            .contributions
            .revision_history(location_id, 50)
            .await
    }

    // -----------------------------------------------------------------------
    // Reviews
    // -----------------------------------------------------------------------

    pub async fn upsert_review(
        &self,
        user: &crate::auth::AuthenticatedUser,
        location_id: i64,
        rating: StarRating,
        body: &ReviewBody,
    ) -> Result<(), ContributionError> {
        self.require_verified(user)?;
        self.allowed(
            &format!("contribution:review:user:{}", user.id.0),
            REVIEW_USER_LIMIT,
            HOUR,
        )
        .await?;

        self.require_active(location_id).await?;

        // The repository reports whether a row already existed (the upsert's
        // `xmax <> 0`), so the audit action never rests on a separate read that
        // a concurrent write could have invalidated.
        let was_update = self
            .deps
            .reviews
            .upsert_review(location_id, user.id, rating, body)
            .await?;
        let action = if was_update {
            "review.edited"
        } else {
            "review.created"
        };
        self.audit(
            Some(user.id),
            action,
            "review",
            location_id.to_string(),
            serde_json::json!({ "rating": rating.value() }),
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Verification
    // -----------------------------------------------------------------------

    pub async fn record_verification(
        &self,
        user: &crate::auth::AuthenticatedUser,
        signal: &NewVerification,
    ) -> Result<(), ContributionError> {
        self.require_verified(user)?;
        let (key, limit) = if signal.is_parked_here() {
            (
                format!("contribution:parked-here:user:{}", user.id.0),
                PARKED_HERE_USER_LIMIT,
            )
        } else {
            (
                format!("contribution:verification:user:{}", user.id.0),
                VERIFICATION_USER_LIMIT,
            )
        };
        self.allowed(&key, limit, HOUR).await?;

        let is_still_exists = signal.is_still_exists();
        let location_id = signal.location_id();
        self.require_active(location_id).await?;
        self.deps.verifications.record(signal, self.now()).await?;
        // A positive existence confirmation is the freshness source.
        if is_still_exists {
            self.deps
                .verifications
                .mark_verified_at(location_id, self.now())
                .await?;
        }
        self.audit(
            Some(user.id),
            "verification.recorded",
            "parking_location",
            location_id.to_string(),
            serde_json::json!({ "kind": signal_kind_code(signal) }),
        )
        .await?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Favorites — authenticated (any logged-in), not necessarily verified.
    // -----------------------------------------------------------------------

    pub async fn toggle_favorite(
        &self,
        user: UserId,
        location_id: i64,
    ) -> Result<bool, ContributionError> {
        self.deps.favorites.toggle(user, location_id).await
    }

    pub async fn list_favorites(
        &self,
        user: UserId,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<FavoriteItem>, ContributionError> {
        self.deps.favorites.list(user, after, limit).await
    }

    pub async fn contribution_history(
        &self,
        user: UserId,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<ContributionItem>, ContributionError> {
        self.deps.history.history(user, after, limit).await
    }

    // -----------------------------------------------------------------------
    // Extended details ( + reviews/confidence/favorite/explanation)
    // -----------------------------------------------------------------------

    /// Builds the community view over an **already-loaded** location: reviews,
    /// confidence (+ disuse disputes), verification panel, favorite state,
    /// own-review/own-verification, and the "recommended because…" reasons
    /// (no origin on the details page, so the distance factor is omitted).
    ///
    /// Takes `location` by value instead of an id so the caller (which already
    /// ran [`crate::search::GetParkingDetails`] to build the base view) loads
    /// the `parking_location` aggregate exactly once per request — this used to
    /// re-fetch it here via `ParkingDetailsReader`, doubling that read.
    pub async fn community_details(
        &self,
        location: ParkingLocation,
        viewer: Option<UserId>,
    ) -> Result<CommunityParkingDetails, ContributionError> {
        let id = location.id();
        let now = self.now();

        // Capped, not paginated: the details page renders the newest reviews
        // inline with no "load more" control (a
        // dedicated paginated reviews view is a separate feature, not a
        // performance fix). `list_active`'s keyset API still supports one.
        let reviews = self
            .deps
            .reviews
            .list_active(id, None, DETAILS_REVIEW_LIMIT)
            .await?;
        let review_ids: Vec<i64> = reviews.iter().map(|r| r.id).collect();
        let review_photos = self.deps.review_photos.for_reviews(&review_ids).await?;
        let signals = self
            .deps
            .verifications
            .latest_existence_per_user(id)
            .await?;
        let confidence =
            bikesnest_domain::confidence(&signals, now, &self.deps.freshness.thresholds);
        let (attribute_summary, parked_here_count) = self
            .deps
            .verifications
            .attribute_and_parked_summary(id)
            .await?;
        let has_attribute_dispute = attribute_summary.iter().any(|a| a.incorrect > 0);
        let has_info_changed = signals
            .iter()
            .any(|s| s.result == ExistenceResult::InfoChanged);
        let disputed = has_attribute_dispute || has_info_changed;

        let own_review = match viewer {
            Some(v) => self.deps.reviews.find_own(id, v).await?,
            None => None,
        };
        let own_verification = viewer.and_then(|v| signals.iter().find(|s| s.user == v).cloned());
        let is_favorited = match viewer {
            Some(v) => self.deps.favorites.is_favorited(v, id).await?,
            None => false,
        };

        // Recommendation explanation from a summary with no origin (distance
        // factor omitted). Page with origin rendering is favorites/search territory.
        let summary = summary_of(&location, &reviews);
        let reasons = recommendation_reasons(
            &summary,
            crate::ports::DEFAULT_RADIUS_M,
            None,
            now,
            &self.deps.freshness,
        );

        Ok(CommunityParkingDetails {
            location,
            reviews,
            review_photos,
            confidence,
            disputed,
            attribute_summary,
            parked_here_count,
            is_favorited,
            own_review,
            own_verification,
            reasons,
        })
    }

    async fn audit(
        &self,
        actor: Option<UserId>,
        action: &str,
        target_type: &str,
        target_id: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Result<(), ContributionError> {
        self.deps
            .audit
            .record(AuditEvent::new(
                actor,
                action,
                target_type,
                target_id,
                "success",
                metadata,
            ))
            .await?;
        Ok(())
    }
}

fn validate_name_address(name: &str, address: &str) -> Result<(), ContributionError> {
    if name.trim().is_empty() {
        return Err(ContributionError::InvalidField(
            "name is required".to_string(),
        ));
    }
    if address.trim().is_empty() {
        return Err(ContributionError::InvalidField(
            "address is required".to_string(),
        ));
    }
    Ok(())
}

fn signal_kind_code(signal: &NewVerification) -> &'static str {
    match signal {
        NewVerification::Existence { .. } => "existence",
        NewVerification::Attribute { .. } => "attribute",
        NewVerification::ParkedHere { .. } => "parked_here",
    }
}

fn summary_of(location: &ParkingLocation, reviews: &[Review]) -> crate::ports::ParkingSummary {
    let rating = bikesnest_domain::Rating::new(
        if reviews.is_empty() {
            None
        } else {
            Some(
                reviews
                    .iter()
                    .map(|r| f64::from(r.rating.value()))
                    .sum::<f64>()
                    / reviews.len() as f64,
            )
        },
        reviews.len() as i64,
    )
    .unwrap_or_else(|_| bikesnest_domain::Rating::new(None, 0).unwrap());
    crate::ports::ParkingSummary {
        id: location.id(),
        name: location.name().to_string(),
        address: location.address().to_string(),
        parking_type: location.parking_type(),
        cost: location.cost().clone(),
        point: *location.point(),
        distance_m: 0.0,
        security_yes: location
            .security()
            .iter()
            .filter(|f| f.state() == bikesnest_domain::SecurityState::Yes)
            .map(|f| f.code().to_string())
            .collect(),
        rating,
        last_verified_at: location.last_verified_at(),
        timezone: location.timezone(),
        is_open_now: false,
        photo_key: None,
        // Not a search result: nothing paginates over this summary.
        sort_key: None,
    }
}
