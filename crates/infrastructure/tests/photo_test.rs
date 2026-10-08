//! Photo infrastructure tests: the image processor (EXIF strip/orientation,
//! format gate, thumbnailing) and the SQL photo repository.

use bikesnest_application::{
    ImageProcessor, NewPendingPhoto, ParkingPhotoReader, PhotoError, PhotoKind, PhotoRepository,
    PhotoTarget,
};
use bikesnest_domain::{PhotoDimensions, PhotoLimits, UserId};
use bikesnest_infrastructure::{
    Db, LocalImageProcessor, SqlxParkingPhotoReader, SqlxPhotoRepository,
};
use bikesnest_test_support::{ParkingBuilder, UserBuilder, db_test, run_isolated_database_test};
use sqlx::Row;

// ---------------------------------------------------------------------------
// Processor (no DB)
// ---------------------------------------------------------------------------

/// A small, decodable solid-color JPEG.
fn base_jpeg(w: u32, h: u32) -> Vec<u8> {
    let img = image::RgbImage::from_pixel(w, h, image::Rgb([12, 34, 56]));
    let mut bytes = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut bytes)
        .encode_image(&img)
        .unwrap();
    bytes
}

/// Minimal little-endian TIFF with a single orientation IFD entry (tag 0x0112).
fn tiff_with_orientation(orientation: u8) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"II"); // little-endian byte order
    t.extend_from_slice(&0x2A_u16.to_le_bytes()); // TIFF magic 42
    t.extend_from_slice(&8_u32.to_le_bytes()); // IFD0 offset (right after header)
    t.extend_from_slice(&1_u16.to_le_bytes()); // one IFD entry
    t.extend_from_slice(&0x0112_u16.to_le_bytes()); // tag: Orientation
    t.extend_from_slice(&3_u16.to_le_bytes()); // type: SHORT
    t.extend_from_slice(&1_u32.to_le_bytes()); // count: 1
    t.extend_from_slice(&(u32::from(orientation)).to_le_bytes()); // value
    t.extend_from_slice(&0_u32.to_le_bytes()); // no next IFD
    t
}

/// Inject an EXIF APP1 segment (orientation) right after the JPEG SOI.
fn jpeg_with_exif_orientation(base: &[u8], orientation: u8) -> Vec<u8> {
    assert!(
        base.len() >= 4 && base[0] == 0xFF && base[1] == 0xD8,
        "SOI expected"
    );
    let tiff = tiff_with_orientation(orientation);
    let payload_len = 6 + tiff.len(); // "Exif\0\0" + TIFF
    let seg_len = 2 + payload_len; // includes the 2 length bytes
    let mut seg = Vec::new();
    seg.push(0xFF);
    seg.push(0xE1); // APP1
    seg.extend_from_slice(&(seg_len as u16).to_be_bytes());
    seg.extend_from_slice(b"Exif\x00\x00");
    seg.extend_from_slice(&tiff);

    let mut out = vec![0xFF, 0xD8];
    out.extend_from_slice(&seg);
    out.extend_from_slice(&base[2..]);
    out
}

/// Scan JPEG markers and report whether any APP1 (EXIF) segment is present.
fn has_exif(jpeg: &[u8]) -> bool {
    if jpeg.len() < 4 || jpeg[0] != 0xFF || jpeg[1] != 0xD8 {
        return false;
    }
    let mut i = 2;
    while i + 1 < jpeg.len() {
        if jpeg[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = jpeg[i + 1];
        // Standalone markers carry no length.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2;
            continue;
        }
        if marker == 0xDA {
            break; // SOS → entropy-coded data
        }
        if i + 3 >= jpeg.len() {
            break;
        }
        let len = (u16::from(jpeg[i + 2]) << 8) | u16::from(jpeg[i + 3]);
        if marker == 0xE1 {
            return true; // APP1 = EXIF
        }
        i += 2 + len as usize;
    }
    false
}

#[tokio::test]
async fn processor_round_trips_jpeg_to_derivatives_without_exif() {
    let source = base_jpeg(800, 600);
    let out = LocalImageProcessor::new(PhotoLimits::default(), 1)
        .process(&source)
        .await
        .unwrap();
    assert_eq!(out.content_type, "image/jpeg");
    assert_eq!(
        out.dimensions,
        PhotoDimensions {
            width: 800,
            height: 600
        }
    );
    // Both derivatives are non-empty JPEGs.
    assert!(out.full.len() > 1000);
    assert!(out.thumb.len() > 100);
    // No EXIF/APP1 markers survive the re-encode.
    assert!(!has_exif(&out.full), "full derivative must not carry EXIF");
    assert!(!has_exif(&out.thumb), "thumbnail must not carry EXIF");
}

#[tokio::test]
async fn processor_applies_exif_orientation_then_strips_exif() {
    // Orientation 6 = Rotate90 → a 400x300 source yields a 300x400 derivative.
    let source = base_jpeg(400, 300);
    let oriented = jpeg_with_exif_orientation(&source, 6);
    let out = LocalImageProcessor::new(PhotoLimits::default(), 1)
        .process(&oriented)
        .await
        .unwrap();
    assert_eq!(
        out.dimensions,
        PhotoDimensions {
            width: 300,
            height: 400
        }
    );
    assert!(
        !has_exif(&out.full),
        "EXIF must be stripped after applying orientation"
    );
    assert!(!has_exif(&out.thumb));
}

#[tokio::test]
async fn processor_rejects_non_allowlisted_format() {
    // BMP magic "BM" is sniffed as BMP → not in the allowlist → UnsupportedFormat.
    let bmp = b"BM\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
    assert!(matches!(
        LocalImageProcessor::new(PhotoLimits::default(), 1)
            .process(bmp)
            .await,
        Err(PhotoError::UnsupportedFormat)
    ));
}

#[tokio::test]
async fn processor_rejects_non_image_input_as_undecodable() {
    assert!(matches!(
        LocalImageProcessor::new(PhotoLimits::default(), 1)
            .process(b"this is definitely not an image")
            .await,
        Err(PhotoError::Undecodable)
    ));
    // Empty input.
    assert!(matches!(
        LocalImageProcessor::new(PhotoLimits::default(), 1)
            .process(b"")
            .await,
        Err(PhotoError::Undecodable)
    ));
}

#[tokio::test]
async fn processor_thumbnails_to_max_side() {
    let source = base_jpeg(1200, 2400); // tall
    let out = LocalImageProcessor::new(PhotoLimits::default(), 1)
        .process(&source)
        .await
        .unwrap();
    // Longest side must be ≤ THUMBNAIL_MAX_SIDE (400); aspect preserved.
    let (w, h) = decode_jpeg_dims(&out.thumb);
    assert!(w <= 400 && h <= 400, "thumb {w}x{h} exceeds 400 max side");
    assert_eq!(w as f64 / h as f64, 0.5, "aspect ratio must be preserved");
}

/// Decode a JPEG's dimensions (via the image crate) for the thumbnail assertion.
fn decode_jpeg_dims(bytes: &[u8]) -> (u32, u32) {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg).unwrap();
    (img.width(), img.height())
}

// ---------------------------------------------------------------------------
// Repository (real Postgres, shared rollback scope for fixtures and adapters)
// ---------------------------------------------------------------------------

struct Fixture {
    user_id: UserId,
    /// A second real user standing in as the moderator (the `reviewed_by` FK
    /// requires a real user row).
    moderator_id: UserId,
    location_id: i64,
}

/// Create users and location inside the same scope the repositories receive.
async fn fresh_fixture(db: &Db, email: &str) -> Fixture {
    let mut conn = db.acquire().await.unwrap();
    let moderator_email = format!("mod-{email}");
    let user = UserBuilder::new()
        .with_email(email)
        .create(&mut *conn)
        .await
        .unwrap();
    let moderator = UserBuilder::new()
        .with_email(&moderator_email)
        .create(&mut *conn)
        .await
        .unwrap();
    let location = ParkingBuilder::new()
        .with_name(format!("Photo Test Location {email}"))
        .create(&mut conn)
        .await
        .unwrap();
    Fixture {
        user_id: user.id,
        moderator_id: moderator.id,
        location_id: location.id(),
    }
}

/// A pending photo whose derivative objects are already written under `slug`
/// — the shape [`PhotoService`] hands the repository now that keys are minted
/// before any write.
fn new_pending(fx: &Fixture, slug: &str) -> NewPendingPhoto {
    NewPendingPhoto {
        target: PhotoTarget::Parking(fx.location_id),
        uploader_id: fx.user_id,
        content_type: "image/jpeg".to_string(),
        alt: Some("An alt text".to_string()),
        storage_key: format!("uploads/{slug}/full.jpg"),
        thumbnail_key: format!("uploads/{slug}/thumb.jpg"),
        dimensions: PhotoDimensions {
            width: 800,
            height: 600,
        },
        processed_at: chrono::Utc::now(),
    }
}

#[db_test]
async fn repo_insert_pending_creates_pending_row(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let fx = fresh_fixture(&db, "photo-insert@example.com").await;
    let repo = SqlxPhotoRepository::new(db.clone());
    let id = repo
        .insert_pending(&new_pending(&fx, "insert"))
        .await
        .unwrap();

    // The row is PENDING_REVIEW *and* complete: one insert, both derivative
    // keys, the dimensions and `processed_at`. There is no window in which
    // `storage_key` is empty (migration 0019 CHECKs that it never is).
    let row = sqlx::query(
        "SELECT moderation_state, storage_key, thumbnail_key, width, height, processed_at, \
         uploader_id FROM parking_photo WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("moderation_state"), "PENDING_REVIEW");
    assert_eq!(
        row.get::<String, _>("storage_key"),
        "uploads/insert/full.jpg"
    );
    assert_eq!(
        row.get::<Option<String>, _>("thumbnail_key").as_deref(),
        Some("uploads/insert/thumb.jpg"),
    );
    assert_eq!(row.get::<Option<i32>, _>("width"), Some(800));
    assert_eq!(row.get::<Option<i32>, _>("height"), Some(600));
    assert!(
        row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("processed_at")
            .is_some()
    );
    assert_eq!(row.get::<Option<i64>, _>("uploader_id"), Some(fx.user_id.0));
}

#[db_test]
async fn parking_photo_rejects_an_empty_storage_key(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    // The CHECK from migration 0019: a row that names no object is not
    // representable, so the crash window the old two-step insert had cannot
    // reopen without this test failing.
    let fx = fresh_fixture(&db, "photo-emptykey@example.com").await;
    let mut conn = db.acquire().await.unwrap();
    let review_id: i64 = sqlx::query_scalar(
        "INSERT INTO review(location_id,author_id,rating,body) VALUES($1,$2,4,'Photo fixture') RETURNING id",
    )
    .bind(fx.location_id)
    .bind(fx.user_id.0)
    .fetch_one(&mut *conn)
    .await
    .unwrap();
    for (table, parent_id) in [
        ("parking_photo", fx.location_id),
        ("review_photo", review_id),
    ] {
        // Each expected CHECK error aborts only its own savepoint. Real parent
        // rows ensure a foreign-key failure cannot masquerade as this CHECK.
        let mut savepoint = conn.begin().await.unwrap();
        let sql = if table == "parking_photo" {
            "INSERT INTO parking_photo (location_id, storage_key, content_type, position, \
             moderation_state) VALUES ($1, '', 'image/jpeg', 0, 'PENDING_REVIEW')"
        } else {
            "INSERT INTO review_photo (review_id, storage_key, position, moderation_state) \
             VALUES ($1, '', 0, 'PENDING_REVIEW')"
        };
        let err = sqlx::query(sql)
            .bind(parent_id)
            .execute(&mut *savepoint)
            .await
            .expect_err("an empty storage_key must be rejected");
        let error = err.as_database_error().expect("database constraint error");
        assert_eq!(error.code().as_deref(), Some("23514"));
        assert_eq!(
            error.constraint(),
            Some(format!("{table}_storage_key_nonempty").as_str())
        );
        savepoint.rollback().await.unwrap();
    }
}

#[db_test]
async fn repo_approve_sets_position_and_reviewer(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let fx = fresh_fixture(&db, "photo-approve@example.com").await;
    let repo = SqlxPhotoRepository::new(db.clone());
    let id = repo
        .insert_pending(&new_pending(&fx, "approve"))
        .await
        .unwrap();

    let earlier = repo
        .insert_pending(&new_pending(&fx, "approve-earlier"))
        .await
        .unwrap();
    let moderator = fx.moderator_id;
    assert_eq!(
        repo.approve(PhotoKind::Parking, earlier, moderator)
            .await
            .unwrap(),
        1,
        "the first approved photo opens the gallery"
    );
    assert_eq!(
        repo.approve(PhotoKind::Parking, id, moderator)
            .await
            .unwrap(),
        2,
        "the next approval goes to the end of the gallery"
    );

    let row = sqlx::query(
        "SELECT moderation_state, position, reviewed_by FROM parking_photo WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("moderation_state"), "APPROVED");
    assert_eq!(row.get::<i32, _>("position"), 2);
    assert_eq!(row.get::<Option<i64>, _>("reviewed_by"), Some(moderator.0));

    // Approving a non-pending photo again → NotPending.
    assert!(matches!(
        repo.approve(PhotoKind::Parking, id, moderator).await,
        Err(PhotoError::NotPending)
    ));
}

#[db_test]
async fn repo_reject_records_reason_and_returns_keys(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let fx = fresh_fixture(&db, "photo-reject@example.com").await;
    let repo = SqlxPhotoRepository::new(db.clone());
    let id = repo
        .insert_pending(&new_pending(&fx, "reject"))
        .await
        .unwrap();

    let moderator = fx.moderator_id;
    let rejected = repo
        .reject(PhotoKind::Parking, id, moderator, "unclear image")
        .await
        .unwrap();
    assert_eq!(rejected.storage_key, "uploads/reject/full.jpg");
    assert_eq!(
        rejected.thumbnail_key.as_deref(),
        Some("uploads/reject/thumb.jpg")
    );

    let row = sqlx::query(
        "SELECT moderation_state, rejection_reason, reviewed_by FROM parking_photo WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *db.acquire().await.unwrap())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("moderation_state"), "REJECTED");
    assert_eq!(
        row.get::<Option<String>, _>("rejection_reason").as_deref(),
        Some("unclear image"),
    );
    assert_eq!(row.get::<Option<i64>, _>("reviewed_by"), Some(moderator.0));
}

#[db_test]
async fn repo_queue_ordering(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let fx = fresh_fixture(&db, "photo-order@example.com").await;
    let repo = SqlxPhotoRepository::new(db.clone());

    let first = repo
        .insert_pending(&new_pending(&fx, "order-1"))
        .await
        .unwrap();
    let second = repo
        .insert_pending(&new_pending(&fx, "order-2"))
        .await
        .unwrap();

    // Both scoped fixture photos appear oldest first relative to each other.
    // Unrelated committed baseline rows may exist; concurrent scoped fixtures
    // are invisible to this transaction.
    let list = repo.list_pending(None, 200).await.unwrap();
    let ids: Vec<i64> = list.iter().map(|p| p.id).collect();
    assert!(ids.contains(&first) && ids.contains(&second));
    let i_first = ids.iter().position(|&i| i == first).unwrap();
    let i_second = ids.iter().position(|&i| i == second).unwrap();
    assert!(i_first < i_second, "first upload must come before second");
}

#[db_test]
async fn reader_returns_thumbnail_key_for_processed_photo(tx: &mut bikesnest_test_support::TestTx) {
    let db = tx.db().await;
    let fx = fresh_fixture(&db, "photo-thumb@example.com").await;
    let repo = SqlxPhotoRepository::new(db.clone());
    let id = repo
        .insert_pending(&new_pending(&fx, "thumb"))
        .await
        .unwrap();
    // Approve so the gallery reader returns it.
    repo.approve(PhotoKind::Parking, id, fx.moderator_id)
        .await
        .unwrap();

    let reader = SqlxParkingPhotoReader::new(db.clone());
    let photos = reader.photos(fx.location_id).await.unwrap();
    let p = photos
        .iter()
        .find(|p| p.alt.as_deref() == Some("An alt text"))
        .expect("photo");
    assert_eq!(p.key, "uploads/thumb/full.jpg");
    assert_eq!(p.thumbnail_key.as_deref(), Some("uploads/thumb/thumb.jpg"));
}

/// Two moderators approving two photos of one location at the same moment
/// must not both read the same "last position": the position is assigned
/// inside the approve transaction while the location row is locked.
#[test]
fn concurrent_approvals_for_one_location_get_distinct_positions() {
    run_isolated_database_test(|pool: sqlx::PgPool| async move {
        let db = Db::from_pool(pool.clone());
        let fx = fresh_fixture(&db, "photo-race@example.com").await;
        let repo = SqlxPhotoRepository::new(db.clone());
        let first = repo
            .insert_pending(&new_pending(&fx, "race-1"))
            .await
            .unwrap();
        let second = repo
            .insert_pending(&new_pending(&fx, "race-2"))
            .await
            .unwrap();

        // Hold the location so both approvals start and queue on it.
        let mut blocker = pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM parking_location WHERE id = $1 FOR NO KEY UPDATE")
            .bind(fx.location_id)
            .execute(&mut *blocker)
            .await
            .unwrap();
        let (a, b) = (
            SqlxPhotoRepository::new(db.clone()),
            SqlxPhotoRepository::new(db.clone()),
        );
        let moderator = fx.moderator_id;
        let mut approvals = Box::pin(async move {
            tokio::join!(
                a.approve(PhotoKind::Parking, first, moderator),
                b.approve(PhotoKind::Parking, second, moderator),
            )
        });
        let waited = tokio::select! {
            _ = wait_for_lockers(&pool, 2) => true,
            _ = &mut approvals => false,
        };
        blocker.rollback().await.unwrap();
        assert!(waited, "both approvals must wait on the location lock");
        let (a, b) = tokio::time::timeout(std::time::Duration::from_secs(10), approvals)
            .await
            .expect("both approvals finish after the lock is released");
        let mut positions = vec![a.unwrap(), b.unwrap()];
        positions.sort_unstable();
        assert_eq!(positions, vec![1, 2]);

        let stored: Vec<i32> = sqlx::query_scalar(
            "SELECT position FROM parking_photo WHERE location_id = $1 ORDER BY position",
        )
        .bind(fx.location_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(stored, vec![1, 2]);
    });
}

async fn wait_for_lockers(pool: &sqlx::PgPool, expected: i64) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(pool)
            .await
            .unwrap();
            if waiting >= expected {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("all competing operations must reach real database locks");
}
