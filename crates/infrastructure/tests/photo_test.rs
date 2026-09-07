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
use bikesnest_test_support::{ParkingBuilder, UserBuilder, db_test, pool};
use sqlx::Row;

async fn db() -> Db {
    Db::from_pool(pool().await)
}

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
    let out = LocalImageProcessor::new(PhotoLimits::default())
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
    let out = LocalImageProcessor::new(PhotoLimits::default())
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
        LocalImageProcessor::new(PhotoLimits::default())
            .process(bmp)
            .await,
        Err(PhotoError::UnsupportedFormat)
    ));
}

#[tokio::test]
async fn processor_rejects_non_image_input_as_undecodable() {
    assert!(matches!(
        LocalImageProcessor::new(PhotoLimits::default())
            .process(b"this is definitely not an image")
            .await,
        Err(PhotoError::Undecodable)
    ));
    // Empty input.
    assert!(matches!(
        LocalImageProcessor::new(PhotoLimits::default())
            .process(b"")
            .await,
        Err(PhotoError::Undecodable)
    ));
}

#[tokio::test]
async fn processor_thumbnails_to_max_side() {
    let source = base_jpeg(1200, 2400); // tall
    let out = LocalImageProcessor::new(PhotoLimits::default())
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
// Repository (real Postgres, committed-fixture + cleanup)
// ---------------------------------------------------------------------------

struct Fixture {
    user_id: UserId,
    /// A second real user standing in as the moderator (the `reviewed_by` FK
    /// requires a real user row).
    moderator_id: UserId,
    location_id: i64,
    email: String,
    tag: String,
}

/// Create a committed user + moderator + location and return ids for cleanup.
async fn fresh_fixture(tx: &mut bikesnest_test_support::TestTx, email: &str) -> Fixture {
    let pool = pool().await;
    let tag = format!("photo-fixture-{email}");
    let moderator_email = format!("mod-{email}");
    // Clean leftovers from a prior identical run.
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(&tag)
        .execute(&pool)
        .await
        .unwrap();
    for e in [email, moderator_email.as_str()] {
        sqlx::query("DELETE FROM users WHERE email = $1")
            .bind(e)
            .execute(&pool)
            .await
            .unwrap();
    }

    let user = UserBuilder::new()
        .with_email(email)
        .create(tx.executor())
        .await
        .unwrap();
    let moderator = UserBuilder::new()
        .with_email(&moderator_email)
        .create(tx.executor())
        .await
        .unwrap();
    let location = ParkingBuilder::new()
        .with_fixture_tag(tag.clone())
        .with_name(format!("Photo Test Location {email}"))
        .create(tx.executor())
        .await
        .unwrap();
    tx.commit_fixture().await;
    Fixture {
        user_id: user.id,
        moderator_id: moderator.id,
        location_id: location.id(),
        email: email.to_string(),
        tag,
    }
}

async fn cleanup(fx: &Fixture) {
    let pool = pool().await;
    sqlx::query("DELETE FROM parking_location WHERE seed_key = $1")
        .bind(&fx.tag)
        .execute(&pool)
        .await
        .unwrap();
    for e in [&fx.email, &format!("mod-{}", fx.email)] {
        sqlx::query("DELETE FROM users WHERE email = $1")
            .bind(e)
            .execute(&pool)
            .await
            .unwrap();
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
    let fx = fresh_fixture(tx, "photo-insert@example.com").await;
    let repo = SqlxPhotoRepository::new(db().await);
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
    .fetch_one(&pool().await)
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

    cleanup(&fx).await;
}

#[db_test]
async fn parking_photo_rejects_an_empty_storage_key(tx: &mut bikesnest_test_support::TestTx) {
    // The CHECK from migration 0019: a row that names no object is not
    // representable, so the crash window the old two-step insert had cannot
    // reopen without this test failing.
    let fx = fresh_fixture(tx, "photo-emptykey@example.com").await;
    for table in ["parking_photo", "review_photo"] {
        let sql = if table == "parking_photo" {
            "INSERT INTO parking_photo (location_id, storage_key, content_type, position, \
             moderation_state) VALUES ($1, '', 'image/jpeg', 0, 'PENDING_REVIEW')"
        } else {
            "INSERT INTO review_photo (review_id, storage_key, position, moderation_state) \
             VALUES ($1, '', 0, 'PENDING_REVIEW')"
        };
        let err = sqlx::query(sql)
            .bind(fx.location_id)
            .execute(&pool().await)
            .await
            .expect_err("an empty storage_key must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("storage_key_nonempty") || msg.contains("violates"),
            "{table}: expected the CHECK to fire, got: {msg}"
        );
    }

    cleanup(&fx).await;
}

#[db_test]
async fn repo_approve_sets_position_and_reviewer(tx: &mut bikesnest_test_support::TestTx) {
    let fx = fresh_fixture(tx, "photo-approve@example.com").await;
    let repo = SqlxPhotoRepository::new(db().await);
    let id = repo
        .insert_pending(&new_pending(&fx, "approve"))
        .await
        .unwrap();

    let moderator = fx.moderator_id;
    repo.approve(PhotoKind::Parking, id, moderator, 5)
        .await
        .unwrap();

    let row = sqlx::query(
        "SELECT moderation_state, position, reviewed_by FROM parking_photo WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("moderation_state"), "APPROVED");
    assert_eq!(row.get::<i32, _>("position"), 5);
    assert_eq!(row.get::<Option<i64>, _>("reviewed_by"), Some(moderator.0));

    // Approving a non-pending photo again → NotPending.
    assert!(matches!(
        repo.approve(PhotoKind::Parking, id, moderator, 6).await,
        Err(PhotoError::NotPending)
    ));

    cleanup(&fx).await;
}

#[db_test]
async fn repo_reject_records_reason_and_returns_keys(tx: &mut bikesnest_test_support::TestTx) {
    let fx = fresh_fixture(tx, "photo-reject@example.com").await;
    let repo = SqlxPhotoRepository::new(db().await);
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
    .fetch_one(&pool().await)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("moderation_state"), "REJECTED");
    assert_eq!(
        row.get::<Option<String>, _>("rejection_reason").as_deref(),
        Some("unclear image"),
    );
    assert_eq!(row.get::<Option<i64>, _>("reviewed_by"), Some(moderator.0));

    cleanup(&fx).await;
}

#[db_test]
async fn repo_max_position_and_queue_ordering(tx: &mut bikesnest_test_support::TestTx) {
    let fx = fresh_fixture(tx, "photo-order@example.com").await;
    let repo = SqlxPhotoRepository::new(db().await);

    assert_eq!(
        repo.max_position(PhotoTarget::Parking(fx.location_id))
            .await
            .unwrap(),
        0
    );
    let first = repo
        .insert_pending(&new_pending(&fx, "order-1"))
        .await
        .unwrap();
    let second = repo
        .insert_pending(&new_pending(&fx, "order-2"))
        .await
        .unwrap();
    // max_position counts APPROVED + all photos (position default 0 here).
    assert_eq!(
        repo.max_position(PhotoTarget::Parking(fx.location_id))
            .await
            .unwrap(),
        0
    );

    // Both fixture photos must appear in the queue (it may also hold other
    // tests' pending rows — DB is shared), oldest first relative to each other.
    let list = repo.list_pending(None, 200).await.unwrap();
    let ids: Vec<i64> = list.iter().map(|p| p.id).collect();
    assert!(ids.contains(&first) && ids.contains(&second));
    let i_first = ids.iter().position(|&i| i == first).unwrap();
    let i_second = ids.iter().position(|&i| i == second).unwrap();
    assert!(i_first < i_second, "first upload must come before second");

    cleanup(&fx).await;
}

#[db_test]
async fn reader_returns_thumbnail_key_for_processed_photo(tx: &mut bikesnest_test_support::TestTx) {
    let fx = fresh_fixture(tx, "photo-thumb@example.com").await;
    let repo = SqlxPhotoRepository::new(db().await);
    let id = repo
        .insert_pending(&new_pending(&fx, "thumb"))
        .await
        .unwrap();
    // Approve so the gallery reader returns it.
    repo.approve(PhotoKind::Parking, id, fx.moderator_id, 1)
        .await
        .unwrap();

    let reader = SqlxParkingPhotoReader::new(db().await);
    let photos = reader.photos(fx.location_id).await.unwrap();
    let p = photos
        .iter()
        .find(|p| p.alt.as_deref() == Some("An alt text"))
        .expect("photo");
    assert_eq!(p.key, "uploads/thumb/full.jpg");
    assert_eq!(p.thumbnail_key.as_deref(), Some("uploads/thumb/thumb.jpg"));

    cleanup(&fx).await;
}
