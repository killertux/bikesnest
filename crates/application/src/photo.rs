//! Photo use cases.
//!
//! Ports + read models + [`PhotoService`]. Infrastructure implements the ports;
//! the web layer calls the service for every upload/moderation action. The
//! verified-email gate, rate limiting, and the upload validation rules
//! and the moderation lifecycle all live here.
//!
//! The service is generic over [`PhotoTarget`], so a single queue serves both
//! location photos (`PhotoTarget::Parking`) and review photos
//! (`PhotoTarget::Review`).

use crate::audit::{AuditEvent, AuditLog};
use crate::auth::Clock;
use crate::rate_limit::{RateLimitError, RateLimiter};
use crate::storage::{ObjectStorage, PutObject, StorageError};
use async_trait::async_trait;
use bikesnest_domain::{
    PhotoDimensions, PhotoLimits, PhotoModerationState, Role, UserId, bytes_within_limit,
};
use chrono::{DateTime, Utc};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum PhotoError {
    /// The session principal has not verified their email (the  gate).
    #[error("verify your email to add photos")]
    NotVerified,
    #[error("too many photo uploads, try again later")]
    RateLimited,
    #[error("photo exceeds the maximum upload size")]
    TooLarge,
    #[error("unsupported image format")]
    UnsupportedFormat,
    #[error("could not read the image")]
    Undecodable,
    #[error("image exceeds the maximum resolution")]
    TooManyPixels,
    #[error("photo not found")]
    NotFound,
    #[error("photo is not awaiting review")]
    NotPending,
    #[error("you are not permitted to perform this action")]
    Unauthorized,
    #[error("invalid photo input: {0}")]
    InvalidField(String),
    #[error("storage error")]
    Storage(#[source] StorageError),
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

impl From<RateLimitError> for PhotoError {
    fn from(_: RateLimitError) -> Self {
        PhotoError::RateLimited
    }
}

impl From<crate::audit::AuditError> for PhotoError {
    fn from(_: crate::audit::AuditError) -> Self {
        PhotoError::Internal
    }
}

impl From<crate::ports::ReaderError> for PhotoError {
    fn from(_: crate::ports::ReaderError) -> Self {
        PhotoError::Internal
    }
}

// ---------------------------------------------------------------------------
// Read models
// ---------------------------------------------------------------------------

/// A processed image: the full JPEG derivative, the thumbnail, and the
/// dimensions of the full derivative. EXIF is stripped, orientation applied,
/// and re-encoded to JPEG.
#[derive(Debug, Clone)]
pub struct ProcessedImage {
    pub full: Vec<u8>,
    pub thumb: Vec<u8>,
    pub dimensions: PhotoDimensions,
    pub content_type: &'static str,
}

/// The kind of photo: attached to a parking location or to a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhotoKind {
    Parking,
    Review,
}

impl PhotoKind {
    pub fn as_code(self) -> &'static str {
        match self {
            PhotoKind::Parking => "parking",
            PhotoKind::Review => "review",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        match code {
            "parking" => Some(PhotoKind::Parking),
            "review" => Some(PhotoKind::Review),
            _ => None,
        }
    }

    /// The table this kind's rows live in (dispatch helper for repositories).
    pub fn table(self) -> &'static str {
        match self {
            PhotoKind::Parking => "parking_photo",
            PhotoKind::Review => "review_photo",
        }
    }
}

/// Which photo a target is: the parent row id (a parking `location_id` or a
/// `review_id`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PhotoTarget {
    Parking(i64),
    Review(i64),
}

impl PhotoTarget {
    pub fn kind(self) -> PhotoKind {
        match self {
            PhotoTarget::Parking(_) => PhotoKind::Parking,
            PhotoTarget::Review(_) => PhotoKind::Review,
        }
    }

    /// The parent row id (location_id or review_id).
    pub fn parent_id(self) -> i64 {
        match self {
            PhotoTarget::Parking(id) => id,
            PhotoTarget::Review(id) => id,
        }
    }
}

/// Insert a newly-uploaded photo in `PENDING_REVIEW`, already carrying the
/// keys of its stored derivatives.
///
/// The keys are minted from a random id *before* anything is written, so the
/// row is inserted once, complete. It used to be inserted first with an empty
/// `storage_key` (to mint the id the keys were derived from) and patched
/// afterwards: a crash in between left a `PENDING_REVIEW` row pointing at no
/// object, which renders as a broken image in the moderation queue forever.
#[derive(Debug, Clone)]
pub struct NewPendingPhoto {
    pub target: PhotoTarget,
    pub uploader_id: UserId,
    pub content_type: String,
    pub alt: Option<String>,
    pub storage_key: String,
    pub thumbnail_key: String,
    pub dimensions: PhotoDimensions,
    pub processed_at: DateTime<Utc>,
}

/// A photo in the moderator queue, oldest first, across both photo
/// kinds. `uploader_id` is never rendered publicly — the queue only ever shows
/// "Contributor #id".
#[derive(Debug, Clone)]
pub struct PendingPhoto {
    pub id: i64,
    pub kind: PhotoKind,
    /// Location id for a parking photo; the review's location id for a review photo.
    pub parent_id: i64,
    /// The parking location name (both kinds attach to a location).
    pub parent_name: String,
    pub storage_key: String,
    pub thumbnail_key: Option<String>,
    pub alt: Option<String>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub uploader_id: Option<UserId>,
    pub created_at: DateTime<Utc>,
}

/// The current moderation view of a single photo (state + derivative keys).
#[derive(Debug, Clone)]
pub struct PhotoForModeration {
    pub id: i64,
    pub kind: PhotoKind,
    pub parent_id: i64,
    pub state: PhotoModerationState,
    pub storage_key: String,
    pub thumbnail_key: Option<String>,
}

/// The derivative keys of a rejected photo, so [`PhotoService`] can delete the
/// objects (idempotent — a missing object is not an error).
#[derive(Debug, Clone)]
pub struct RejectedPhoto {
    pub storage_key: String,
    pub thumbnail_key: Option<String>,
}

/// Result of a successful upload: the new photo id (the caller can render a
/// "awaiting review" success fragment; the photo is not yet in the gallery).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadedPhoto {
    pub id: i64,
    pub kind: PhotoKind,
}

// ---------------------------------------------------------------------------
// Ports
// ---------------------------------------------------------------------------

/// Internal processing seam (NOT a provider boundary): our own deterministic
/// decode → apply EXIF orientation → re-encode JPEG → thumbnail logic. Trait-ified
/// so application tests use a fast fake.
#[async_trait]
pub trait ImageProcessor: Send + Sync {
    /// Decode, strip metadata and resize. Returns the two byte buffers plus the
    /// full-derivative dimensions. Rejects over-limit formats/sizes before a
    /// full decode where possible.
    async fn process(&self, bytes: &[u8]) -> Result<ProcessedImage, PhotoError>;
}

#[async_trait]
pub trait PhotoRepository: Send + Sync {
    /// Insert the photo as `PENDING_REVIEW`, complete with the derivative keys
    /// and dimensions, and return its id. The objects are already in storage by
    /// the time this is called.
    async fn insert_pending(&self, p: &NewPendingPhoto) -> Result<i64, PhotoError>;
    /// Remove a photo row (compensation for a failed storage write).
    async fn delete(&self, kind: PhotoKind, id: i64) -> Result<(), PhotoError>;
    /// Flip a pending photo to `APPROVED`, place it at the end of its target's
    /// gallery and set the reviewer columns, all in one transaction that holds
    /// a lock on the target, so concurrent approvals for one target never
    /// share a position. Returns the assigned position; `NotPending` if the
    /// photo is no longer pending.
    async fn approve(&self, kind: PhotoKind, id: i64, moderator: UserId)
    -> Result<i32, PhotoError>;
    /// Flip to `REJECTED`, record the reason + reviewer, and return the keys to
    /// delete (one transaction).
    async fn reject(
        &self,
        kind: PhotoKind,
        id: i64,
        moderator: UserId,
        reason: &str,
    ) -> Result<RejectedPhoto, PhotoError>;
    /// The pending queue, oldest first, across both kinds and tables —
    /// keyset-paginated on `(created_at, id)` (the two tables' `id` sequences
    /// are independent, so the cursor needs the timestamp too). `after` is
    /// the last row's `(created_at, id)` from the previous page.
    async fn list_pending(
        &self,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<PendingPhoto>, PhotoError>;
    /// A single photo's moderation view (state + derivative keys).
    async fn get_for_moderation(
        &self,
        kind: PhotoKind,
        id: i64,
    ) -> Result<Option<PhotoForModeration>, PhotoError>;
}

// ---------------------------------------------------------------------------
// Rate-limit defaults. `photo:upload:user:{id}` and `photo:upload:ip:{ip}`.
// Moderator actions are audited, not rate-limited; limits are configurable.
// ---------------------------------------------------------------------------

const PHOTO_UPLOAD_USER_LIMIT: u32 = 10;
const PHOTO_UPLOAD_IP_LIMIT: u32 = 20;

const DAY: Duration = Duration::from_secs(24 * 60 * 60);

/// Max length of the accessible `alt` caption, trimmed in the domain.
const MAX_ALT_LEN: usize = 500;

// ---------------------------------------------------------------------------
// PhotoService
// ---------------------------------------------------------------------------

/// Everything the photo use cases depend on, bundled for construction.
pub struct PhotoDeps {
    pub processor: Box<dyn ImageProcessor>,
    pub repository: Box<dyn PhotoRepository>,
    pub storage: Box<dyn ObjectStorage>,
    pub rate_limiter: Box<dyn RateLimiter>,
    pub audit: Box<dyn AuditLog>,
    pub clock: Box<dyn Clock>,
    /// Random bytes for the upload's storage key, minted before any write.
    pub tokens_gen: Box<dyn crate::auth::TokenGenerator>,
    /// Runtime photo pipeline limits; defaults to the domain constants.
    pub limits: PhotoLimits,
}

pub struct PhotoService {
    deps: PhotoDeps,
}

impl PhotoService {
    pub fn new(deps: PhotoDeps) -> Self {
        Self { deps }
    }

    fn now(&self) -> DateTime<Utc> {
        self.deps.clock.now()
    }

    fn require_verified(&self, user: &crate::auth::AuthenticatedUser) -> Result<(), PhotoError> {
        if user.is_verified {
            Ok(())
        } else {
            Err(PhotoError::NotVerified)
        }
    }

    /// Moderators and admins may moderate.
    fn require_moderator(&self, user: &crate::auth::AuthenticatedUser) -> Result<(), PhotoError> {
        if user.has_role(Role::Moderator) || user.has_role(Role::Admin) {
            Ok(())
        } else {
            Err(PhotoError::Unauthorized)
        }
    }

    async fn allowed(&self, key: &str, limit: u32, window: Duration) -> Result<(), PhotoError> {
        if self.deps.rate_limiter.check(key, limit, window).await? {
            Ok(())
        } else {
            Err(PhotoError::RateLimited)
        }
    }

    /// Normalize + length-limit the `alt` caption. Returns `None` for a
    /// whitespace-only caption.
    fn normalize_alt(alt: Option<&str>) -> Result<Option<String>, PhotoError> {
        match alt {
            None | Some("") => Ok(None),
            Some(raw) => {
                let trimmed = raw.trim();
                if trimmed.is_empty() {
                    return Ok(None);
                }
                if trimmed.chars().count() > MAX_ALT_LEN {
                    return Err(PhotoError::InvalidField("caption too long".to_string()));
                }
                Ok(Some(trimmed.to_string()))
            }
        }
    }

    /// A random, opaque id for this upload's storage keys — 32 hex characters
    /// from the same CSPRNG the session/token generator uses. It is minted
    /// before any write so the keys never depend on a database-generated id.
    fn mint_upload_id(&self) -> String {
        let bytes = self.deps.tokens_gen.generate();
        bytes[..16].iter().map(|b| format!("{b:02x}")).collect()
    }

    fn target_type_code(&self, kind: PhotoKind) -> &'static str {
        match kind {
            PhotoKind::Parking => "parking_photo",
            PhotoKind::Review => "review_photo",
        }
    }

    // -----------------------------------------------------------------------
    // Upload
    // -----------------------------------------------------------------------

    /// Validate → process → mint the storage key → write both derivatives →
    /// insert the row (once, complete) → audit. The original bytes are never
    /// stored: only the stripped, re-encoded derivatives.
    ///
    /// The order matters: the key comes from a random id rather than from the
    /// row's generated id, so no row has to exist before the objects do. A
    /// failure at any point leaves either nothing at all or an unreferenced
    /// object that the retention orphan sweep collects — never a row pointing
    /// at an object that is not there.
    pub async fn upload_photo(
        &self,
        user: &crate::auth::AuthenticatedUser,
        ip: &str,
        target: PhotoTarget,
        bytes: &[u8],
        alt: Option<&str>,
    ) -> Result<UploadedPhoto, PhotoError> {
        self.require_verified(user)?;
        self.allowed(
            &format!("photo:upload:user:{}", user.id.0),
            PHOTO_UPLOAD_USER_LIMIT,
            DAY,
        )
        .await?;
        self.allowed(&format!("photo:upload:ip:{ip}"), PHOTO_UPLOAD_IP_LIMIT, DAY)
            .await?;

        if !bytes_within_limit(bytes.len(), self.deps.limits.max_bytes) {
            return Err(PhotoError::TooLarge);
        }
        let alt = Self::normalize_alt(alt)?;
        let processed = self.deps.processor.process(bytes).await?;

        let kind = target.kind();
        let upload_id = self.mint_upload_id();
        let full_key = format!("uploads/{upload_id}/full.jpg");
        let thumb_key = format!("uploads/{upload_id}/thumb.jpg");

        // Write the full derivative, then the thumbnail. On a failure,
        // compensate by deleting whatever landed; no row exists yet, so there
        // is nothing to roll back in the database.
        if let Err(e) = self
            .deps
            .storage
            .put(PutObject {
                key: full_key.clone(),
                bytes: &processed.full,
                content_type: processed.content_type.to_string(),
            })
            .await
        {
            return Err(PhotoError::Storage(e));
        }
        if let Err(e) = self
            .deps
            .storage
            .put(PutObject {
                key: thumb_key.clone(),
                bytes: &processed.thumb,
                content_type: "image/jpeg".to_string(),
            })
            .await
        {
            let _ = self.deps.storage.delete(&full_key).await;
            return Err(PhotoError::Storage(e));
        }

        // Both objects exist: the row can be written complete, in one insert.
        // If *this* fails the objects are unreferenced and the retention
        // orphan sweep reclaims them, but delete them now rather than waiting.
        let new = NewPendingPhoto {
            target,
            uploader_id: user.id,
            content_type: processed.content_type.to_string(),
            alt,
            storage_key: full_key.clone(),
            thumbnail_key: thumb_key.clone(),
            dimensions: processed.dimensions,
            processed_at: self.now(),
        };
        let id = match self.deps.repository.insert_pending(&new).await {
            Ok(id) => id,
            Err(e) => {
                let _ = self.deps.storage.delete(&thumb_key).await;
                let _ = self.deps.storage.delete(&full_key).await;
                return Err(e);
            }
        };

        self.audit(
            Some(user.id),
            "photo.uploaded",
            self.target_type_code(kind),
            id.to_string(),
            serde_json::json!({ "parent_id": target.parent_id() }),
        )
        .await?;

        Ok(UploadedPhoto { id, kind })
    }

    // -----------------------------------------------------------------------
    // Moderation
    // -----------------------------------------------------------------------

    /// Approve a pending photo: place it at the end of the target's gallery.
    /// Idempotent — approving an already-approved photo is a no-op.
    pub async fn approve_photo(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        kind: PhotoKind,
        id: i64,
    ) -> Result<(), PhotoError> {
        self.require_moderator(moderator)?;
        let photo = self
            .deps
            .repository
            .get_for_moderation(kind, id)
            .await?
            .ok_or(PhotoError::NotFound)?;
        match photo.state {
            PhotoModerationState::PendingReview => {}
            PhotoModerationState::Approved => return Ok(()), // idempotent
            PhotoModerationState::Rejected | PhotoModerationState::Hidden => {
                return Err(PhotoError::NotPending);
            }
        }
        let position = self.deps.repository.approve(kind, id, moderator.id).await?;
        self.audit(
            Some(moderator.id),
            "photo.approved",
            self.target_type_code(kind),
            id.to_string(),
            serde_json::json!({ "position": position }),
        )
        .await?;
        Ok(())
    }

    /// Reject a pending photo: set REJECTED + reason, then delete both stored
    /// derivatives (idempotent object delete). Idempotent for already-rejected.
    pub async fn reject_photo(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        kind: PhotoKind,
        id: i64,
        reason: &str,
    ) -> Result<(), PhotoError> {
        self.require_moderator(moderator)?;
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(PhotoError::InvalidField("reason is required".to_string()));
        }
        let photo = self
            .deps
            .repository
            .get_for_moderation(kind, id)
            .await?
            .ok_or(PhotoError::NotFound)?;
        match photo.state {
            PhotoModerationState::PendingReview => {}
            PhotoModerationState::Rejected => return Ok(()), // idempotent
            PhotoModerationState::Approved | PhotoModerationState::Hidden => {
                return Err(PhotoError::NotPending);
            }
        }
        let rejected = self
            .deps
            .repository
            .reject(kind, id, moderator.id, reason)
            .await?;
        // Best-effort deletes (a missing object is not an error): a rejected
        // photo's bytes go now; leftover in-flight objects are swept later.
        let _ = self.deps.storage.delete(&rejected.storage_key).await;
        if let Some(thumb) = &rejected.thumbnail_key {
            let _ = self.deps.storage.delete(thumb).await;
        }
        self.audit(
            Some(moderator.id),
            "photo.rejected",
            self.target_type_code(kind),
            id.to_string(),
            serde_json::json!({ "reason": reason }),
        )
        .await?;
        Ok(())
    }

    /// The pending queue (both kinds), bounded + keyset-paginated. The web
    /// layer resolves presigned URLs.
    pub async fn list_pending_photos(
        &self,
        moderator: &crate::auth::AuthenticatedUser,
        after: Option<(DateTime<Utc>, i64)>,
        limit: i64,
    ) -> Result<Vec<PendingPhoto>, PhotoError> {
        self.require_moderator(moderator)?;
        self.deps.repository.list_pending(after, limit).await
    }

    async fn audit(
        &self,
        actor: Option<UserId>,
        action: &str,
        target_type: &str,
        target_id: impl Into<String>,
        metadata: serde_json::Value,
    ) -> Result<(), PhotoError> {
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

impl PhotoTarget {
    /// Rebuild a `PhotoTarget` from a kind + parent id (used by approve/reject
    /// to compute a target's gallery position).
    pub fn for_kind(kind: PhotoKind, parent_id: i64) -> PhotoTarget {
        match kind {
            PhotoKind::Parking => PhotoTarget::Parking(parent_id),
            PhotoKind::Review => PhotoTarget::Review(parent_id),
        }
    }
}
