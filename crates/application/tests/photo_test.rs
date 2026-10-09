//! M4 photo service tests (pure, with fakes): gating, upload validation, the
//! moderation lifecycle, and the compensation/delete behavior.

use async_trait::async_trait;
use bikesnest_application::{
    AuditLog, Clock, ImageProcessor, ObjectStorage, PhotoDeps, PhotoError, PhotoKind,
    PhotoRepository, PhotoService, PhotoTarget, ProcessedImage, PutObject,
};
use bikesnest_domain::{
    AccountState, PhotoDimensions, PhotoLimits, PhotoModerationState, Role, UserEmail, UserId,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---------------------------------------------------------------------------
// Fakes
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum FakeProcError {
    UnsupportedFormat,
    Undecodable,
    TooManyPixels,
}

struct FakeImageProcessor {
    error: Option<FakeProcError>,
}

impl FakeImageProcessor {
    fn ok() -> Self {
        Self { error: None }
    }
    fn fail(kind: FakeProcError) -> Self {
        Self { error: Some(kind) }
    }
}

#[async_trait]
impl ImageProcessor for FakeImageProcessor {
    async fn process(&self, _bytes: &[u8]) -> Result<ProcessedImage, PhotoError> {
        match self.error {
            Some(FakeProcError::UnsupportedFormat) => Err(PhotoError::UnsupportedFormat),
            Some(FakeProcError::Undecodable) => Err(PhotoError::Undecodable),
            Some(FakeProcError::TooManyPixels) => Err(PhotoError::TooManyPixels),
            None => Ok(ProcessedImage {
                full: b"full-bytes".to_vec(),
                thumb: b"thumb-bytes".to_vec(),
                dimensions: PhotoDimensions {
                    width: 100,
                    height: 50,
                },
                content_type: "image/jpeg",
            }),
        }
    }
}

#[derive(Clone, Default)]
struct FakeStorage {
    puts: Arc<Mutex<Vec<String>>>,
    deletes: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ObjectStorage for FakeStorage {
    async fn put(&self, req: PutObject<'_>) -> Result<String, bikesnest_application::StorageError> {
        self.puts.lock().unwrap().push(req.key.clone());
        Ok(req.key)
    }
    async fn presigned_get(
        &self,
        key: &str,
        _ttl: Duration,
    ) -> Result<String, bikesnest_application::StorageError> {
        Ok(format!("/media/{key}"))
    }
    async fn delete(&self, key: &str) -> Result<(), bikesnest_application::StorageError> {
        self.deletes.lock().unwrap().push(key.to_string());
        Ok(())
    }
    async fn exists(&self, key: &str) -> Result<bool, bikesnest_application::StorageError> {
        Ok(self.puts.lock().unwrap().iter().any(|k| k == key))
    }
    async fn list(
        &self,
        prefix: &str,
        _after: Option<&str>,
    ) -> Result<bikesnest_application::ObjectPage, bikesnest_application::StorageError> {
        let objects = self
            .puts
            .lock()
            .unwrap()
            .iter()
            .filter(|k| k.starts_with(prefix))
            .map(|k| bikesnest_application::ObjectInfo {
                key: k.clone(),
                last_modified: chrono::Utc::now(),
            })
            .collect();
        Ok(bikesnest_application::ObjectPage {
            objects,
            next: None,
        })
    }
}

#[derive(Clone)]
struct MemPhoto {
    id: i64,
    kind: PhotoKind,
    parent_id: i64,
    state: PhotoModerationState,
    storage_key: String,
    thumbnail_key: Option<String>,
    alt: Option<String>,
    uploader_id: Option<UserId>,
    position: i32,
}

#[derive(Clone)]
struct FakePhotoRepo {
    next_id: Arc<std::sync::atomic::AtomicI64>,
    photos: Arc<Mutex<HashMap<i64, MemPhoto>>>,
    inserted: Arc<Mutex<Vec<i64>>>,
    deleted: Arc<Mutex<Vec<i64>>>,
}

impl FakePhotoRepo {
    fn new() -> Self {
        Self {
            next_id: Arc::new(std::sync::atomic::AtomicI64::new(100)),
            photos: Arc::new(Mutex::new(HashMap::new())),
            inserted: Arc::new(Mutex::new(Vec::new())),
            deleted: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

#[async_trait]
impl PhotoRepository for FakePhotoRepo {
    async fn insert_pending(
        &self,
        p: &bikesnest_application::NewPendingPhoto,
    ) -> Result<i64, PhotoError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.photos.lock().unwrap().insert(
            id,
            MemPhoto {
                id,
                kind: p.target.kind(),
                parent_id: p.target.parent_id(),
                state: PhotoModerationState::PendingReview,
                storage_key: p.storage_key.clone(),
                thumbnail_key: Some(p.thumbnail_key.clone()),
                alt: p.alt.clone(),
                uploader_id: Some(p.uploader_id),
                position: 0,
            },
        );
        self.inserted.lock().unwrap().push(id);
        Ok(id)
    }
    async fn delete(&self, _kind: PhotoKind, id: i64) -> Result<(), PhotoError> {
        self.photos.lock().unwrap().remove(&id);
        self.deleted.lock().unwrap().push(id);
        Ok(())
    }
    async fn approve(
        &self,
        kind: PhotoKind,
        id: i64,
        _moderator: UserId,
    ) -> Result<i32, PhotoError> {
        let mut photos = self.photos.lock().unwrap();
        let (state, parent_id) = {
            let p = photos.get(&id).ok_or(PhotoError::NotFound)?;
            (p.state, p.parent_id)
        };
        if state != PhotoModerationState::PendingReview {
            return Err(PhotoError::NotPending);
        }
        let position = photos
            .values()
            .filter(|p| p.kind == kind && p.parent_id == parent_id)
            .map(|p| p.position)
            .max()
            .unwrap_or(0)
            + 1;
        let p = photos.get_mut(&id).expect("checked above");
        p.state = PhotoModerationState::Approved;
        p.position = position;
        Ok(position)
    }
    async fn reject(
        &self,
        _kind: PhotoKind,
        id: i64,
        _moderator: UserId,
        _reason: &str,
    ) -> Result<bikesnest_application::RejectedPhoto, PhotoError> {
        let mut photos = self.photos.lock().unwrap();
        let p = photos.get_mut(&id).ok_or(PhotoError::NotFound)?;
        if p.state != PhotoModerationState::PendingReview {
            return Err(PhotoError::NotPending);
        }
        p.state = PhotoModerationState::Rejected;
        Ok(bikesnest_application::RejectedPhoto {
            storage_key: p.storage_key.clone(),
            thumbnail_key: p.thumbnail_key.clone(),
        })
    }
    async fn list_pending(
        &self,
        _after: Option<(chrono::DateTime<chrono::Utc>, i64)>,
        _limit: i64,
    ) -> Result<Vec<bikesnest_application::PendingPhoto>, PhotoError> {
        let photos = self.photos.lock().unwrap();
        Ok(photos
            .values()
            .filter(|p| p.state == PhotoModerationState::PendingReview)
            .map(|p| bikesnest_application::PendingPhoto {
                id: p.id,
                kind: p.kind,
                parent_id: p.parent_id,
                parent_name: format!("Location {}", p.parent_id),
                storage_key: p.storage_key.clone(),
                thumbnail_key: p.thumbnail_key.clone(),
                alt: p.alt.clone(),
                width: None,
                height: None,
                uploader_id: p.uploader_id,
                created_at: chrono::Utc::now(),
            })
            .collect())
    }
    async fn get_for_moderation(
        &self,
        _kind: PhotoKind,
        id: i64,
    ) -> Result<Option<bikesnest_application::PhotoForModeration>, PhotoError> {
        let photos = self.photos.lock().unwrap();
        Ok(photos
            .get(&id)
            .map(|p| bikesnest_application::PhotoForModeration {
                id: p.id,
                kind: p.kind,
                parent_id: p.parent_id,
                state: p.state,
                storage_key: p.storage_key.clone(),
                thumbnail_key: p.thumbnail_key.clone(),
            }))
    }
}

struct FakeClock(chrono::DateTime<chrono::Utc>);
impl Clock for FakeClock {
    fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.0
    }
}

/// Deterministic bytes, so the minted upload key is predictable in assertions.
struct FakeTokens;
impl bikesnest_application::TokenGenerator for FakeTokens {
    fn generate(&self) -> [u8; 32] {
        [0xab; 32]
    }
}

/// The key `FakeTokens` produces: the first 16 bytes as hex.
const FAKE_UPLOAD_ID: &str = "abababababababababababababababab";

#[derive(Default)]
struct FakeRate {
    allow: bool,
}
#[async_trait]
impl bikesnest_application::RateLimiter for FakeRate {
    async fn check(
        &self,
        _key: &str,
        _limit: u32,
        _window: Duration,
    ) -> Result<bool, bikesnest_application::RateLimitError> {
        Ok(self.allow)
    }
}

#[derive(Clone, Default)]
struct FakeAudit(Arc<Mutex<Vec<String>>>);
#[async_trait]
impl AuditLog for FakeAudit {
    async fn record(
        &self,
        event: bikesnest_application::AuditEvent,
    ) -> Result<(), bikesnest_application::AuditError> {
        self.0.lock().unwrap().push(event.action);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn verified_user() -> bikesnest_application::AuthenticatedUser {
    authed_user(1, true, vec![])
}
fn moderator() -> bikesnest_application::AuthenticatedUser {
    authed_user(2, true, vec![Role::Moderator])
}
fn authed_user(
    id: i64,
    verified: bool,
    roles: Vec<Role>,
) -> bikesnest_application::AuthenticatedUser {
    bikesnest_application::AuthenticatedUser {
        id: UserId(id),
        email: UserEmail::parse("user@example.com").unwrap(),
        display_name: None,
        account_state: AccountState::Active,
        is_verified: verified,
        roles,
    }
}

struct Harness {
    service: PhotoService,
    storage: Arc<FakeStorage>,
    repo: Arc<FakePhotoRepo>,
    audit: Arc<FakeAudit>,
}

fn harness(processor: FakeImageProcessor) -> Harness {
    let storage = Arc::new(FakeStorage::default());
    let repo = Arc::new(FakePhotoRepo::new());
    let audit = Arc::new(FakeAudit::default());
    let service = PhotoService::new(PhotoDeps {
        processor: Box::new(processor),
        repository: Box::new(repo.as_ref().clone()),
        storage: Box::new(storage.as_ref().clone()),
        rate_limiter: Box::new(FakeRate { allow: true }),
        audit: Box::new(audit.as_ref().clone()),
        clock: Box::new(FakeClock(chrono::Utc::now())),
        tokens_gen: Box::new(FakeTokens),
        limits: PhotoLimits::default(),
    });
    Harness {
        service,
        storage,
        repo,
        audit,
    }
}

#[tokio::test]
async fn upload_happy_path_inserts_pending_and_writes_both_derivatives() {
    let h = harness(FakeImageProcessor::ok());
    let out = h
        .service
        .upload_photo(
            &verified_user(),
            "1.2.3.4",
            PhotoTarget::Parking(10),
            b"xyz",
            Some("alt"),
        )
        .await
        .unwrap();
    assert!(out.id >= 100);
    // Both derivative objects written.
    assert_eq!(h.storage.puts.lock().unwrap().len(), 2);
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"photo.uploaded".to_string())
    );
    // Inserted as PENDING (gallery reader only returns APPROVED).
    let state = h
        .repo
        .get_for_moderation(PhotoKind::Parking, out.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.state, PhotoModerationState::PendingReview);
}

#[tokio::test]
async fn upload_rejects_unverified_user() {
    let h = harness(FakeImageProcessor::ok());
    let user = authed_user(1, false, vec![]);
    assert!(matches!(
        h.service
            .upload_photo(&user, "1.2.3.4", PhotoTarget::Parking(10), b"xyz", None)
            .await,
        Err(PhotoError::NotVerified)
    ));
    assert_eq!(h.repo.inserted.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn upload_rejects_over_size_before_processing() {
    let h = harness(FakeImageProcessor::ok());
    let too_big = vec![0u8; bikesnest_domain::MAX_PHOTO_BYTES + 1];
    assert!(matches!(
        h.service
            .upload_photo(
                &verified_user(),
                "1.2.3.4",
                PhotoTarget::Parking(10),
                &too_big,
                None
            )
            .await,
        Err(PhotoError::TooLarge)
    ));
    assert_eq!(h.repo.inserted.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn upload_is_rate_limited() {
    let storage = Arc::new(FakeStorage::default());
    let repo = Arc::new(FakePhotoRepo::new());
    let audit = Arc::new(FakeAudit::default());
    let service = PhotoService::new(PhotoDeps {
        processor: Box::new(FakeImageProcessor::ok()),
        repository: Box::new(repo.as_ref().clone()),
        storage: Box::new(storage.as_ref().clone()),
        rate_limiter: Box::new(FakeRate { allow: false }),
        audit: Box::new(audit.as_ref().clone()),
        clock: Box::new(FakeClock(chrono::Utc::now())),
        tokens_gen: Box::new(FakeTokens),
        limits: PhotoLimits::default(),
    });
    assert!(matches!(
        service
            .upload_photo(
                &verified_user(),
                "1.2.3.4",
                PhotoTarget::Parking(10),
                b"x",
                None
            )
            .await,
        Err(PhotoError::RateLimited)
    ));
    assert_eq!(repo.inserted.lock().unwrap().len(), 0);
}

#[tokio::test]
async fn upload_propagates_processor_format_error() {
    let h = harness(FakeImageProcessor::fail(FakeProcError::UnsupportedFormat));
    assert!(matches!(
        h.service
            .upload_photo(
                &verified_user(),
                "1.2.3.4",
                PhotoTarget::Parking(10),
                b"xyz",
                None
            )
            .await,
        Err(PhotoError::UnsupportedFormat)
    ));
}

#[tokio::test]
async fn upload_propagates_undecodable_and_too_many_pixels() {
    let h = harness(FakeImageProcessor::fail(FakeProcError::Undecodable));
    assert!(matches!(
        h.service
            .upload_photo(
                &verified_user(),
                "1.2.3.4",
                PhotoTarget::Parking(10),
                b"x",
                None
            )
            .await,
        Err(PhotoError::Undecodable)
    ));

    let h = harness(FakeImageProcessor::fail(FakeProcError::TooManyPixels));
    assert!(matches!(
        h.service
            .upload_photo(
                &verified_user(),
                "1.2.3.4",
                PhotoTarget::Parking(10),
                b"x",
                None
            )
            .await,
        Err(PhotoError::TooManyPixels)
    ));
}

#[tokio::test]
async fn upload_compensates_the_first_object_when_the_second_put_fails() {
    // Storage fails on the thumbnail put (the second write). Because the row is
    // only inserted once both objects exist, there is no row to compensate —
    // the full derivative that did land must be deleted, nothing must be
    // inserted, and no audit recorded.
    let storage = Arc::new(FakeStorage::default());
    // Override put to fail on the thumb key.
    struct FailThumb(Arc<FakeStorage>);
    #[async_trait]
    impl ObjectStorage for FailThumb {
        async fn put(
            &self,
            req: PutObject<'_>,
        ) -> Result<String, bikesnest_application::StorageError> {
            if req.key.contains("thumb") {
                return Err(bikesnest_application::StorageError::Unavailable);
            }
            self.0.puts.lock().unwrap().push(req.key.clone());
            Ok(req.key)
        }
        async fn presigned_get(
            &self,
            key: &str,
            _ttl: Duration,
        ) -> Result<String, bikesnest_application::StorageError> {
            Ok(format!("/media/{key}"))
        }
        async fn delete(&self, key: &str) -> Result<(), bikesnest_application::StorageError> {
            self.0.deletes.lock().unwrap().push(key.to_string());
            Ok(())
        }
        async fn exists(&self, key: &str) -> Result<bool, bikesnest_application::StorageError> {
            self.0.exists(key).await
        }
        async fn list(
            &self,
            prefix: &str,
            after: Option<&str>,
        ) -> Result<bikesnest_application::ObjectPage, bikesnest_application::StorageError>
        {
            self.0.list(prefix, after).await
        }
    }
    let repo = Arc::new(FakePhotoRepo::new());
    let audit = Arc::new(FakeAudit::default());
    let service = PhotoService::new(PhotoDeps {
        processor: Box::new(FakeImageProcessor::ok()),
        repository: Box::new(repo.as_ref().clone()),
        storage: Box::new(FailThumb(storage.clone())),
        rate_limiter: Box::new(FakeRate { allow: true }),
        audit: Box::new(audit.as_ref().clone()),
        clock: Box::new(FakeClock(chrono::Utc::now())),
        tokens_gen: Box::new(FakeTokens),
        limits: PhotoLimits::default(),
    });
    assert!(matches!(
        service
            .upload_photo(
                &verified_user(),
                "1.2.3.4",
                PhotoTarget::Parking(10),
                b"xyz",
                None
            )
            .await,
        Err(PhotoError::Storage(_))
    ));
    assert!(
        repo.inserted.lock().unwrap().is_empty(),
        "no row may be inserted before both objects exist"
    );
    assert_eq!(
        *storage.deletes.lock().unwrap(),
        vec![format!("uploads/{FAKE_UPLOAD_ID}/full.jpg")],
        "the object that did land must be compensated"
    );
    assert!(
        !audit
            .0
            .lock()
            .unwrap()
            .contains(&"photo.uploaded".to_string())
    );
}

#[tokio::test]
async fn upload_writes_both_objects_before_it_inserts_the_row() {
    let h = harness(FakeImageProcessor::ok());
    let out = h
        .service
        .upload_photo(
            &verified_user(),
            "1.2.3.4",
            PhotoTarget::Parking(10),
            b"xyz",
            None,
        )
        .await
        .unwrap();

    // The key comes from a random id, not from the row id — which is what lets
    // the objects be written first.
    let full = format!("uploads/{FAKE_UPLOAD_ID}/full.jpg");
    let thumb = format!("uploads/{FAKE_UPLOAD_ID}/thumb.jpg");
    assert_eq!(
        *h.storage.puts.lock().unwrap(),
        vec![full.clone(), thumb.clone()]
    );
    assert!(h.storage.exists(&full).await.unwrap());
    assert!(h.storage.exists(&thumb).await.unwrap());

    // And the row it inserted names those objects — never an empty key.
    let row = h
        .repo
        .get_for_moderation(PhotoKind::Parking, out.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.storage_key, full);
    assert_eq!(row.thumbnail_key.as_deref(), Some(thumb.as_str()));
}

#[tokio::test]
async fn upload_compensates_both_objects_when_the_insert_fails() {
    /// A repository whose insert always fails (a constraint violation, a lost
    /// connection): the two objects already written must not be left behind.
    #[derive(Clone)]
    struct FailInsert(Arc<FakePhotoRepo>);
    #[async_trait]
    impl PhotoRepository for FailInsert {
        async fn insert_pending(
            &self,
            _p: &bikesnest_application::NewPendingPhoto,
        ) -> Result<i64, PhotoError> {
            Err(PhotoError::Conflict)
        }
        async fn delete(&self, kind: PhotoKind, id: i64) -> Result<(), PhotoError> {
            self.0.delete(kind, id).await
        }
        async fn approve(
            &self,
            kind: PhotoKind,
            id: i64,
            moderator: UserId,
        ) -> Result<i32, PhotoError> {
            self.0.approve(kind, id, moderator).await
        }
        async fn reject(
            &self,
            kind: PhotoKind,
            id: i64,
            moderator: UserId,
            reason: &str,
        ) -> Result<bikesnest_application::RejectedPhoto, PhotoError> {
            self.0.reject(kind, id, moderator, reason).await
        }
        async fn list_pending(
            &self,
            after: Option<(chrono::DateTime<chrono::Utc>, i64)>,
            limit: i64,
        ) -> Result<Vec<bikesnest_application::PendingPhoto>, PhotoError> {
            self.0.list_pending(after, limit).await
        }
        async fn get_for_moderation(
            &self,
            kind: PhotoKind,
            id: i64,
        ) -> Result<Option<bikesnest_application::PhotoForModeration>, PhotoError> {
            self.0.get_for_moderation(kind, id).await
        }
    }

    let storage = Arc::new(FakeStorage::default());
    let audit = Arc::new(FakeAudit::default());
    let service = PhotoService::new(PhotoDeps {
        processor: Box::new(FakeImageProcessor::ok()),
        repository: Box::new(FailInsert(Arc::new(FakePhotoRepo::new()))),
        storage: Box::new(storage.as_ref().clone()),
        rate_limiter: Box::new(FakeRate { allow: true }),
        audit: Box::new(audit.as_ref().clone()),
        clock: Box::new(FakeClock(chrono::Utc::now())),
        tokens_gen: Box::new(FakeTokens),
        limits: PhotoLimits::default(),
    });
    assert!(matches!(
        service
            .upload_photo(
                &verified_user(),
                "1.2.3.4",
                PhotoTarget::Parking(10),
                b"xyz",
                None
            )
            .await,
        Err(PhotoError::Conflict)
    ));
    let deletes = storage.deletes.lock().unwrap().clone();
    assert_eq!(
        deletes.len(),
        2,
        "both objects must be reclaimed, got {deletes:?}"
    );
    assert!(deletes.iter().any(|k| k.ends_with("/full.jpg")));
    assert!(deletes.iter().any(|k| k.ends_with("/thumb.jpg")));
}

#[tokio::test]
async fn approve_requires_moderator_role() {
    let h = harness(FakeImageProcessor::ok());
    assert!(matches!(
        h.service
            .approve_photo(&verified_user(), PhotoKind::Parking, 99)
            .await,
        Err(PhotoError::Unauthorized)
    ));
}

#[tokio::test]
async fn reject_requires_moderator_role() {
    let h = harness(FakeImageProcessor::ok());
    assert!(matches!(
        h.service
            .reject_photo(&verified_user(), PhotoKind::Parking, 99, "reason")
            .await,
        Err(PhotoError::Unauthorized)
    ));
}

#[tokio::test]
async fn approve_flow_audits_and_repositions() {
    let h = harness(FakeImageProcessor::ok());
    let out = h
        .service
        .upload_photo(
            &verified_user(),
            "1.2.3.4",
            PhotoTarget::Parking(10),
            b"xyz",
            None,
        )
        .await
        .unwrap();
    h.service
        .approve_photo(&moderator(), PhotoKind::Parking, out.id)
        .await
        .unwrap();
    let state = h
        .repo
        .get_for_moderation(PhotoKind::Parking, out.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(state.state, PhotoModerationState::Approved);
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"photo.approved".to_string())
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn reject_flow_deletes_both_derivatives_and_audits() {
    let h = harness(FakeImageProcessor::ok());
    let out = h
        .service
        .upload_photo(
            &verified_user(),
            "1.2.3.4",
            PhotoTarget::Parking(10),
            b"xyz",
            None,
        )
        .await
        .unwrap();
    h.service
        .reject_photo(&moderator(), PhotoKind::Parking, out.id, "unclear")
        .await
        .unwrap();

    let deletes = h.storage.deletes.lock().unwrap();
    assert_eq!(deletes.len(), 2, "both derivatives deleted");
    assert!(deletes.iter().any(|k| k.contains("full.jpg")));
    assert!(deletes.iter().any(|k| k.contains("thumb.jpg")));
    drop(deletes);
    assert!(
        h.audit
            .0
            .lock()
            .unwrap()
            .contains(&"photo.rejected".to_string())
    );
    // Re-reject is idempotent.
    assert!(matches!(
        h.service
            .reject_photo(&moderator(), PhotoKind::Parking, out.id, "again")
            .await,
        Ok(())
    ));
}

#[tokio::test]
async fn approve_of_rejected_photo_is_not_pending() {
    let h = harness(FakeImageProcessor::ok());
    let out = h
        .service
        .upload_photo(
            &verified_user(),
            "1.2.3.4",
            PhotoTarget::Parking(10),
            b"xyz",
            None,
        )
        .await
        .unwrap();
    h.service
        .reject_photo(&moderator(), PhotoKind::Parking, out.id, "unclear")
        .await
        .unwrap();
    assert!(matches!(
        h.service
            .approve_photo(&moderator(), PhotoKind::Parking, out.id)
            .await,
        Err(PhotoError::NotPending)
    ));
}
