//! Opt-in B18 measurements for the real export repository and image codec.
//!
//! These are measurements, not timing assertions. Run one exact test at a
//! time with `--release --ignored --nocapture --test-threads=1`; export
//! fixtures live inside the ordinary rollback scope and never persist.

use std::time::{Duration, Instant};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
};

use bikesnest_application::{ExportRepository, ImageProcessor, NewExport};
use bikesnest_domain::{PhotoLimits, UserId};
use bikesnest_infrastructure::auth::hash::sha256_hex;
use bikesnest_infrastructure::{LocalImageProcessor, SqlxExportRepository};
use bikesnest_test_support::{TestTx, UserBuilder, run_db_test};
use chrono::Utc;
use image::codecs::jpeg::JpegEncoder;
use image::{DynamicImage, Rgb, RgbImage};

#[derive(Clone, Copy)]
struct ExportProfile {
    name: &'static str,
    seeded_locations: i32,
    reviews: i32,
    review_revisions_each: i32,
    review_body_bytes: usize,
    verifications: i32,
    proposals: i32,
    reports: i32,
    photos: i32,
    sessions: i32,
}

const REPRESENTATIVE_EXPORT: ExportProfile = ExportProfile {
    name: "representative",
    seeded_locations: 100,
    reviews: 50,
    review_revisions_each: 1,
    review_body_bytes: 500,
    verifications: 300,
    proposals: 50,
    reports: 25,
    photos: 10,
    sessions: 5,
};

// There is no finite product/schema upper bound on an account's lifetime
// history. This deliberately heavy envelope represents roughly 100 days of
// maximum photo submissions, 200 hours of maximum proposals, 100 hours of
// maximum review submissions, and 1,000 hours of maximum verifications under
// today's admission windows. It is a re-evaluation point, not a claimed cap.
const HEAVY_EXPORT_ENVELOPE: ExportProfile = ExportProfile {
    name: "heavy_account_envelope",
    seeded_locations: 5_000,
    reviews: 1_000,
    review_revisions_each: 5,
    review_body_bytes: 2_000,
    verifications: 30_000,
    proposals: 1_000,
    reports: 1_000,
    photos: 1_000,
    sessions: 90,
};

#[test]
#[ignore = "B18 opt-in release measurement against explicit TEST_DATABASE_URL"]
fn b18_export_representative_release_measurement() {
    run_db_test(async |tx| measure_export(tx, REPRESENTATIVE_EXPORT).await);
}

#[test]
#[ignore = "B18 opt-in release measurement against explicit TEST_DATABASE_URL"]
fn b18_export_heavy_account_release_measurement() {
    run_db_test(async |tx| measure_export(tx, HEAVY_EXPORT_ENVELOPE).await);
}

#[test]
#[ignore = "B18 phase-isolated release measurement against explicit TEST_DATABASE_URL"]
fn b18_export_download_phase_release_measurement() {
    run_db_test(async |tx| {
        let db = tx.db().await;
        let user = UserBuilder::new()
            .with_email("b18-download-phase@example.com")
            .create(&mut *db.acquire().await.expect("download fixture user"))
            .await
            .expect("download fixture user");
        let token = [0xD1; 32];
        // Build the valid ~20 MiB payload inside PostgreSQL so this test
        // process has not assembled/deserialized it before the measured phase.
        let export_id: i64 = sqlx::query_scalar(
            r#"
            INSERT INTO personal_data_export
                (user_id,state,token_hash,payload,expires_at)
            SELECT $1,'READY',$2,
                   jsonb_build_object(
                     'schema_version',2,
                     'exported_at',clock_timestamp(),
                     'account',jsonb_build_object(
                       'user_id',$1,'email','phase@example.com','display_name',NULL,
                       'public_contribution_name',false,
                       'public_contribution_name_updated_at',NULL,
                       'account_state','ACTIVE','email_verified_at',NULL,
                       'created_at',clock_timestamp(),'roles','[]'::jsonb),
                     'authentication','[]'::jsonb,'sessions','[]'::jsonb,
                     'favorites','[]'::jsonb,
                     'reviews',(SELECT jsonb_agg(jsonb_build_object(
                       'id',g,'location_id',g,'rating',5,'body',repeat('x',2000),
                       'public_author',false,'moderation_state','ACTIVE',
                       'created_at',clock_timestamp(),'updated_at',clock_timestamp(),
                       'revisions','[]'::jsonb)) FROM generate_series(1,9500) g),
                     'verifications','[]'::jsonb,'proposals','[]'::jsonb,
                     'proposal_votes','[]'::jsonb,'reports','[]'::jsonb,
                     'photos','[]'::jsonb,'terms_notice_presentations','[]'::jsonb,
                     'terms_acknowledgements','[]'::jsonb),
                   clock_timestamp()+interval '24 hours'
            RETURNING id
            "#,
        )
        .bind(user.id.0)
        .bind(sha256_hex(&token))
        .fetch_one(&mut *db.acquire().await.expect("download fixture insert"))
        .await
        .expect("download fixture insert");
        let (stored_bytes, json_text_bytes): (i32, i32) = sqlx::query_as(
            "SELECT pg_column_size(payload),octet_length(payload::text) FROM personal_data_export WHERE id=$1",
        )
        .bind(export_id)
        .fetch_one(&mut *db.acquire().await.expect("download fixture size"))
        .await
        .expect("download fixture size");

        let rss = RssSampler::start();
        let started = Instant::now();
        let download = SqlxExportRepository::new(db)
            .consume_download(export_id, &token, Utc::now())
            .await
            .expect("phase-isolated download");
        let consume = started.elapsed();
        let pretty_started = Instant::now();
        let pretty_bytes = serde_json::to_vec_pretty(&download.payload)
            .expect("phase-isolated pretty JSON")
            .len();
        let pretty = pretty_started.elapsed();
        let rss = rss.finish();
        eprintln!(
            "B18_EXPORT_DOWNLOAD_PHASE json_text_bytes={json_text_bytes} stored_jsonb_bytes={stored_bytes} pretty_bytes={pretty_bytes} consume_ms={:.3} pretty_ms={:.3} rss={rss:?} hwm_kib={:?}",
            millis(consume),
            millis(pretty),
            proc_status_kib("VmHWM"),
        );
    });
}

async fn measure_export(tx: &mut TestTx, profile: ExportProfile) {
    let db = tx.db().await;
    let fixture_started = Instant::now();
    let user = UserBuilder::new()
        .with_email(format!(
            "b18-{}@example.com",
            profile.name.replace('_', "-")
        ))
        .create(&mut *db.acquire().await.expect("fixture connection"))
        .await
        .expect("measurement user");
    let uid = user.id.0;

    let location_ids: Vec<i64> = sqlx::query_scalar(
        r#"
        INSERT INTO parking_location
            (name, address, parking_type, cost_kind, location, timezone,
             hours_unknown, moderation_state, version)
        SELECT 'B18 location ' || g, 'B18 address ' || g, 'rack', 'free',
               ST_SetSRID(ST_MakePoint(-46.6 + g * 0.000001, -23.5), 4326)::geography,
               'America/Sao_Paulo', true, 'ACTIVE', 1
        FROM generate_series(1, $1::integer) AS g
        RETURNING id
        "#,
    )
    .bind(profile.seeded_locations)
    .fetch_all(&mut *db.acquire().await.expect("location fixture connection"))
    .await
    .expect("location fixtures");
    assert_eq!(location_ids.len(), profile.seeded_locations as usize);

    sqlx::query(
        r#"
        INSERT INTO authentication_identities
            (user_id, provider, provider_subject, credential_hash)
        VALUES ($1, 'password', $2, 'excluded-from-export')
        "#,
    )
    .bind(uid)
    .bind(format!("b18-{}@example.com", profile.name))
    .execute(&mut *db.acquire().await.expect("identity fixture connection"))
    .await
    .expect("identity fixture");

    sqlx::query(
        r#"
        INSERT INTO sessions (token_hash, user_id, csrf_token, created_at, last_seen_at, expires_at)
        SELECT 'b18-session-' || $2 || '-' || g, $1, 'b18-csrf',
               now() - g * interval '1 minute', now(), now() + interval '1 day'
        FROM generate_series(1, $3::integer) AS g
        "#,
    )
    .bind(uid)
    .bind(profile.name)
    .bind(profile.sessions)
    .execute(&mut *db.acquire().await.expect("session fixture connection"))
    .await
    .expect("session fixtures");

    sqlx::query(
        r#"
        INSERT INTO favorite (user_id, location_id)
        SELECT $1, id FROM unnest($2::bigint[]) AS id
        "#,
    )
    .bind(uid)
    .bind(&location_ids)
    .execute(&mut *db.acquire().await.expect("favorite fixture connection"))
    .await
    .expect("favorite fixtures");

    let review_ids: Vec<i64> = sqlx::query_scalar(
        r#"
        INSERT INTO review (location_id, author_id, rating, body, moderation_state)
        SELECT id, $1, 5, repeat('r', $3), 'ACTIVE'
        FROM unnest($2::bigint[]) WITH ORDINALITY AS location(id, n)
        WHERE n <= $4
        RETURNING id
        "#,
    )
    .bind(uid)
    .bind(&location_ids)
    .bind(profile.review_body_bytes as i32)
    .bind(profile.reviews as i64)
    .fetch_all(&mut *db.acquire().await.expect("review fixture connection"))
    .await
    .expect("review fixtures");
    assert_eq!(review_ids.len(), profile.reviews as usize);

    sqlx::query(
        r#"
        INSERT INTO review_revision (review_id, rating, body, edited_at)
        SELECT id, 4, repeat('e', $2), now() - revision * interval '1 minute'
        FROM unnest($1::bigint[]) AS id
        CROSS JOIN generate_series(1, $3::integer) AS revision
        "#,
    )
    .bind(&review_ids)
    .bind(profile.review_body_bytes as i32)
    .bind(profile.review_revisions_each)
    .execute(&mut *db.acquire().await.expect("revision fixture connection"))
    .await
    .expect("review revision fixtures");

    sqlx::query(
        r#"
        INSERT INTO verification
            (location_id, user_id, kind, result, created_at)
        SELECT $1[1 + ((g - 1) % array_length($1, 1))], $2,
               'existence', 'still_exists', now() - g * interval '1 minute'
        FROM generate_series(1, $3::integer) AS g
        "#,
    )
    .bind(&location_ids)
    .bind(uid)
    .bind(profile.verifications)
    .execute(&mut *db.acquire().await.expect("verification fixture connection"))
    .await
    .expect("verification fixtures");

    let proposal_ids: Vec<i64> = sqlx::query_scalar(
        r#"
        INSERT INTO parking_proposal
            (location_id, proposer_id, base_version, kind, proposed, status)
        SELECT id, $1, 1, 'change_existence',
               jsonb_build_object('existence', 'removed', 'reason', repeat('p', 500)),
               'PENDING'
        FROM unnest($2::bigint[]) WITH ORDINALITY AS location(id, n)
        WHERE n <= $3
        RETURNING id
        "#,
    )
    .bind(uid)
    .bind(&location_ids)
    .bind(profile.proposals as i64)
    .fetch_all(&mut *db.acquire().await.expect("proposal fixture connection"))
    .await
    .expect("proposal fixtures");

    sqlx::query(
        r#"
        INSERT INTO parking_proposal_vote (proposal_id, voter_id, vote)
        SELECT id, $1, 'APPROVE' FROM unnest($2::bigint[]) AS id
        "#,
    )
    .bind(uid)
    .bind(&proposal_ids)
    .execute(&mut *db.acquire().await.expect("vote fixture connection"))
    .await
    .expect("proposal vote fixtures");

    sqlx::query(
        r#"
        INSERT INTO report
            (reporter_id, target_type, target_id, reason, description, state)
        SELECT $1, 'parking', id, 'other', repeat('d', 1000), 'OPEN'
        FROM unnest($2::bigint[]) WITH ORDINALITY AS location(id, n)
        WHERE n <= $3
        "#,
    )
    .bind(uid)
    .bind(&location_ids)
    .bind(profile.reports as i64)
    .execute(&mut *db.acquire().await.expect("report fixture connection"))
    .await
    .expect("report fixtures");

    sqlx::query(
        r#"
        INSERT INTO parking_photo
            (location_id, uploader_id, storage_key, thumbnail_key, content_type,
             width, height, processed_at, moderation_state)
        SELECT id, $1, 'uploads/b18-' || $2 || '-' || n || '/full.jpg',
               'uploads/b18-' || $2 || '-' || n || '/thumb.jpg',
               'image/jpeg', 2000, 1500, now(), 'PENDING_REVIEW'
        FROM unnest($3::bigint[]) WITH ORDINALITY AS location(id, n)
        WHERE n <= $4
        "#,
    )
    .bind(uid)
    .bind(profile.name)
    .bind(&location_ids)
    .bind(profile.photos as i64)
    .execute(&mut *db.acquire().await.expect("photo fixture connection"))
    .await
    .expect("photo fixtures");

    let fixture_ms = fixture_started.elapsed().as_secs_f64() * 1_000.0;
    let repository = SqlxExportRepository::new(db.clone());

    let expected_revision_count = profile.reviews * profile.review_revisions_each;

    // Conservatively approximate synchronous POST memory before any export
    // payload has existed in this process: assemble, measurement-only compact
    // serialization, then production create (which serializes to Value) and
    // READY persistence. The extra compact Vec is not part of production.
    let generation_rss = RssSampler::start();
    let payload = repository
        .assemble_payload(UserId(uid))
        .await
        .expect("payload for persistence measurement");
    assert_eq!(payload.reviews.len(), profile.reviews as usize);
    assert_eq!(
        payload
            .reviews
            .iter()
            .map(|review| review.revisions.len())
            .sum::<usize>(),
        expected_revision_count as usize
    );
    assert_eq!(payload.verifications.len(), profile.verifications as usize);
    let exported_favorites = payload.favorites.len();
    assert_eq!(exported_favorites, profile.seeded_locations as usize);
    let compact_bytes = serde_json::to_vec(&payload)
        .expect("compact export serialization")
        .len();
    let token = [0xB1; 32];
    let create_started = Instant::now();
    // Reproduce the synchronous READY persistence path used by production.
    let export_id = repository
        .create(&NewExport {
            user_id: UserId(uid),
            token,
            payload,
            expires_at: Utc::now() + chrono::Duration::hours(24),
        })
        .await
        .expect("persist measured export");
    let create = create_started.elapsed();
    let generation_rss = generation_rss.finish();
    let stored_bytes: i32 = sqlx::query_scalar(
        "SELECT pg_column_size(payload) FROM personal_data_export WHERE id = $1",
    )
    .bind(export_id)
    .fetch_one(&mut *db.acquire().await.expect("payload size connection"))
    .await
    .expect("stored payload size");

    // Warm caches and code paths before latency-only assembly samples. These
    // occur after the conservative synchronous-POST memory phase above.
    let warm = repository
        .assemble_payload(UserId(uid))
        .await
        .expect("warm export assembly");
    drop(warm);
    let rss_before_kib = proc_status_kib("VmRSS");
    let mut assemble_samples = Vec::with_capacity(7);
    for _ in 0..7 {
        let started = Instant::now();
        let payload = repository
            .assemble_payload(UserId(uid))
            .await
            .expect("measured export assembly");
        assemble_samples.push(started.elapsed());
        std::hint::black_box(&payload);
    }
    let rss_after_assemble_kib = proc_status_kib("VmRSS");

    let download_rss = RssSampler::start();
    let download_started = Instant::now();
    let download = repository
        .consume_download(export_id, &token, Utc::now())
        .await
        .expect("measured export download");
    let consume = download_started.elapsed();
    let pretty_started = Instant::now();
    let pretty_bytes = serde_json::to_vec_pretty(&download.payload)
        .expect("pretty download serialization")
        .len();
    let pretty = pretty_started.elapsed();
    let download_rss = download_rss.finish();

    let (assemble_min, assemble_median, assemble_p95) = summarize(&mut assemble_samples);
    eprintln!(
        "B18_EXPORT profile={} fixture_ms={fixture_ms:.3} seeded_locations={} exported_favorites={} reviews={} review_revisions={} verifications={} proposals={} reports={} photos={} sessions={} compact_bytes={} stored_jsonb_bytes={} download_pretty_bytes={} assemble_min_ms={:.3} assemble_median_ms={:.3} assemble_p95_ms={:.3} create_ms={:.3} consume_ms={:.3} pretty_serialize_ms={:.3} rss_before_kib={:?} rss_after_assemble_kib={:?} conservative_generation_rss={generation_rss:?} download_rss={download_rss:?} peak_rss_kib={:?}",
        profile.name,
        profile.seeded_locations,
        exported_favorites,
        profile.reviews,
        expected_revision_count,
        profile.verifications,
        profile.proposals,
        profile.reports,
        profile.photos,
        profile.sessions,
        compact_bytes,
        stored_bytes,
        pretty_bytes,
        millis(assemble_min),
        millis(assemble_median),
        millis(assemble_p95),
        millis(create),
        millis(consume),
        millis(pretty),
        rss_before_kib,
        rss_after_assemble_kib,
        proc_status_kib("VmHWM"),
    );
}

#[test]
#[ignore = "B18 opt-in release measurement of the real image codec"]
fn b18_media_representative_and_20mp_release_measurement() {
    run_db_test(async |_tx| {
        let limits = PhotoLimits::default();
        let processor = LocalImageProcessor::new(limits);
        let representative = include_bytes!("../../../web/static/img/hero-bike-parking.jpg");
        assert!(representative.len() <= limits.max_bytes);

        // Warm the blocking pool and decoder before taking samples.
        processor
            .process(representative)
            .await
            .expect("warm representative image");
        let (representative_samples, representative_output) =
            measure_image(&processor, representative, 7).await;

        let upper_input = exact_20mp_jpeg();
        assert!(
            upper_input.len() <= limits.max_bytes,
            "20 MP fixture must pass the real byte admission cap: {} bytes",
            upper_input.len()
        );
        let (upper_samples, upper_output) = measure_image(&processor, &upper_input, 5).await;

        let mut representative_samples = representative_samples;
        let mut upper_samples = upper_samples;
        let (rep_min, rep_median, rep_p95) = summarize(&mut representative_samples);
        let (upper_min, upper_median, upper_p95) = summarize(&mut upper_samples);
        let upper_decoded_rgb_bytes = u64::from(upper_output.dimensions.width)
            * u64::from(upper_output.dimensions.height)
            * 3;
        eprintln!(
            "B18_MEDIA representative_input_bytes={} representative_dimensions={}x{} representative_full_bytes={} representative_thumb_bytes={} representative_min_ms={:.3} representative_median_ms={:.3} representative_p95_ms={:.3} upper_input_bytes={} upper_dimensions={}x{} upper_full_bytes={} upper_thumb_bytes={} upper_decoded_rgb_bytes={} upper_min_ms={:.3} upper_median_ms={:.3} upper_p95_ms={:.3} peak_rss_kib={:?}",
            representative.len(),
            representative_output.dimensions.width,
            representative_output.dimensions.height,
            representative_output.full.len(),
            representative_output.thumb.len(),
            millis(rep_min),
            millis(rep_median),
            millis(rep_p95),
            upper_input.len(),
            upper_output.dimensions.width,
            upper_output.dimensions.height,
            upper_output.full.len(),
            upper_output.thumb.len(),
            upper_decoded_rgb_bytes,
            millis(upper_min),
            millis(upper_median),
            millis(upper_p95),
            proc_status_kib("VmHWM"),
        );
    });
}

async fn measure_image(
    processor: &LocalImageProcessor,
    bytes: &[u8],
    samples: usize,
) -> (Vec<Duration>, bikesnest_application::ProcessedImage) {
    let mut timings = Vec::with_capacity(samples);
    let mut last = None;
    for _ in 0..samples {
        let started = Instant::now();
        let processed = processor
            .process(bytes)
            .await
            .expect("measured image process");
        timings.push(started.elapsed());
        last = Some(processed);
    }
    (timings, last.expect("at least one image sample"))
}

fn exact_20mp_jpeg() -> Vec<u8> {
    let image = RgbImage::from_fn(5_000, 4_000, |x, y| {
        // Deterministic, moderately detailed input that does real codec work
        // without exceeding the independent 10 MiB byte ceiling.
        let coarse = ((x / 8) ^ (y / 8)) as u8;
        Rgb([
            coarse.wrapping_add((x % 251) as u8),
            coarse.wrapping_mul(3).wrapping_add((y % 241) as u8),
            ((x + y) % 239) as u8,
        ])
    });
    let mut encoded = Vec::new();
    JpegEncoder::new_with_quality(&mut encoded, 70)
        .encode_image(&DynamicImage::ImageRgb8(image))
        .expect("encode deterministic 20 MP fixture");
    encoded
}

fn summarize(samples: &mut [Duration]) -> (Duration, Duration, Duration) {
    assert!(!samples.is_empty());
    samples.sort_unstable();
    let p95_index = ((samples.len() * 95).div_ceil(100)).saturating_sub(1);
    (samples[0], samples[samples.len() / 2], samples[p95_index])
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

fn proc_status_kib(field: &str) -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status.lines().find_map(|line| {
        let value = line.strip_prefix(field)?.strip_prefix(':')?.trim();
        value.split_whitespace().next()?.parse().ok()
    })
}

struct RssSampler {
    baseline_kib: u64,
    stop: Arc<AtomicBool>,
    handle: JoinHandle<u64>,
}

impl RssSampler {
    fn start() -> Self {
        let baseline_kib = proc_status_kib("VmRSS").unwrap_or(0);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut peak = baseline_kib;
            while !thread_stop.load(Ordering::Relaxed) {
                peak = peak.max(proc_status_kib("VmRSS").unwrap_or(0));
                std::thread::sleep(Duration::from_millis(1));
            }
            peak.max(proc_status_kib("VmRSS").unwrap_or(0))
        });
        Self {
            baseline_kib,
            stop,
            handle,
        }
    }

    fn finish(self) -> (u64, u64, u64) {
        self.stop.store(true, Ordering::Relaxed);
        let peak_kib = self.handle.join().expect("RSS sampler thread");
        (
            self.baseline_kib,
            peak_kib,
            peak_kib.saturating_sub(self.baseline_kib),
        )
    }
}
