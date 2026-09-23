# B18 implementation handoff

Date: 2026-09-23
Baseline: `3f3056b12149e7ee6a2b1a582142a1699b873756`
Status: bounded corrections frozen for same-reviewer re-review; not committed or deployed

## Decision

Keep personal-data exports and media processing synchronous. The optimized,
phase-isolated measurements did not cross the documented thresholds, so a queue
rewrite would not be evidence-based. The complete numeric threshold set below
was documented and held fixed before the fresh independent reruns; those reruns
also remained below every threshold. There is no durable record proving that
the complete set preceded the first implementation measurements.

| Path | Queue trigger | Measured result |
|---|---|---|
| export | representative p95 >1 s, heavy-envelope p95 >3 s, compact output >25 MiB, or request-phase RSS increase >128 MiB | representative p95 2.661 ms; heavy p95 59.749 ms; 18.94 MB compact; conservative heavy generation + persistence approximation increased RSS 92.3 MiB; separate typed download + pretty increased RSS 89.6 MiB |
| media | representative p95 >1 s, synthetic upper-envelope p95 >5 s, or unsafe memory capacity with a provable durable private quarantine | 74.097 ms representative and 544.292 ms 20 MP p95; no durable raw quarantine exists |

An earlier combined-process export run showed an approximately 141 MiB HWM
increase. That number combined generation, repeated assembly, persistence,
typed download and pretty serialization in one process, so allocator retention
made it invalid attribution for either HTTP phase. The separate-process results
above supersede it; it is not a queue trigger.

The decision does not claim that synchronous work is risk-free. Account history
has no finite product cap and exports have no export-specific admission or
one-active-export guard. Those are explicit monitoring and re-evaluation risks.
B19 must also validate aggregate memory capacity against actual CPU count,
process layout, concurrency and deployment limits.

The first independent review requested bounded evidence-record corrections;
its findings and fresh rerun results remain recorded in
[`B18-review.md`](B18-review.md).

## Request-path inventory and bounds

### Personal-data export

`POST /account/privacy/export` authenticates the account, generates a 32-byte
download credential, then synchronously assembles a versioned payload through
`PrivacyService` and `SqlxExportRepository`. Assembly uses a repeatable-read
snapshot and, for the measured profiles with nonempty reviews, executes 15 SQL
queries (counting account and roles separately). The request inserts a `READY`
JSONB row containing the snapshot and redirects with the raw credential in a
path-scoped, `HttpOnly`, `SameSite=Strict` cookie; only its SHA-256 hash is
stored.

Download requires the authenticated owner and the raw cookie credential. It
loads the JSONB into the typed payload, pretty-serializes it for the response,
and transitions the export to `DOWNLOADED`; it is single-use. PENDING/FAILED
states do not exist in the retained design. Credential/session/token hashes,
CSRF values and audit rows are excluded from the payload.

Exports expire 24 hours after creation. Scheduled
`SqlxRetentionRepository::purge_expired_exports` deletes every expired export
state, including `DOWNLOADED`; the repository-local
`SqlxExportRepository::purge_expired` is READY-only. Thus rows can accumulate
without a count bound inside the TTL, or longer if scheduled retention is
delayed/disabled, but normal retention is not lifetime retention. Account
anonymization deletes the account's export rows.

Underlying contribution endpoints have their own admission controls, but there
is no finite bound on accumulated account history and no export-specific rate
limit, cooldown, or active-export guard. Cancellation drops the remaining
application future; an in-flight PostgreSQL statement can continue briefly
until the driver/server observes cancellation, and there is no durable export
job to resume. Re-evaluate if the envelope or concurrency materially grows.

### Media

The request body is capped at configured bytes plus 64 KiB. Verified accounts
are limited to 10 submissions/day per user and 20/day per IP. Default codec
limits are 10 MiB input and 20 megapixels. JPEG/PNG/WebP content is sniffed;
orientation, metadata-stripping re-encode, full image, and thumbnail processing
run in `spawn_blocking`. The semaphore permit is owned inside that blocking
closure, so HTTP cancellation does not free capacity while CPU/memory work is
still running.

Raw input is never persisted. Only processed derivatives are written, object
writes are compensated on failure, and the database row is inserted only after
both writes. New photos are private `PENDING_REVIEW`; rejection deletes their
objects. Retention includes a 24-hour orphan sweep and one-hour broken-pending
reconciliation. There is no durable, private raw-upload quarantine with proven
expiry/deletion behavior, so introducing a media queue would expand privacy and
cleanup risk without a measured request-latency need.

## Measurement methodology

The ignored opt-in harness is
`crates/infrastructure/tests/audit_b18_measurements.rs`. It calls the real SQLx
export repository and production `LocalImageProcessor`; no mock latency is used.
Run each exact test in a separate optimized process. A 1 ms `/proc/self/status`
sampler records phase baseline and peak RSS. Seven samples are used for export
assembly and representative media; five for 20 MP media. With these small
samples, “p95” is the maximum observed sample, not a production SLO.

The representative export fixture seeds 100 parking locations and favorites
all of them, yielding 100 exported favorites, plus 50 reviews and 50 revisions,
300 verifications, 50 proposals, 25 reports, 10 photos and 5 sessions. The
deliberately heavy, non-cap envelope seeds 5,000 parking locations and yields
5,000 exported favorites, plus 1,000 maximum-body reviews and 5,000
maximum-body revisions, 30,000 verifications, 1,000 proposals/reports/photos,
and 90 sessions. It approximates 100 maximum-photo days, 200 maximum-proposal
hours (about 8.3 days), 100 review hours, and 1,000 verification hours under
current admission windows.

The generation/persistence RSS measurement begins immediately after fixtures,
before any prior export payload allocation. It conservatively approximates the
synchronous POST: the measurement performs assembly, a measurement-only compact
`serde_json::to_vec`, and real production `create`, which separately converts
the payload with `serde_json::to_value` before READY persistence. The extra
compact byte vector is not part of production, so this is not an exact POST
reproduction. The download measurement is a separate process; PostgreSQL
constructs a valid approximately 21 MB snapshot so Rust has not previously
assembled/deserialized it, then the current repository performs typed
consumption and pretty serialization.

Measurements used a newly created disposable database
`bikesnest_test_audit_b18_20260923`, owned by `bikesnest_test`, migrated from an
empty database through committed migrations 1–29. The earlier disposable audit
base had recorded the subsequently reverted draft migration 0030 and was
abandoned unchanged; neither database is production, and neither was deleted.
The final read-only `_sqlx_migrations` check returned `29|29|t` (count, maximum
version, all successful).

## Raw and summarized measurements

All four release tests passed independently. Key results:

| Profile | Input/output | latency | sampled RSS |
|---|---|---|---|
| representative export | 100 seeded locations / 100 exported favorites; 180,705 B compact; 6,913 B JSONB; 214,340 B pretty | assembly 1.677 / 2.235 / 2.661 ms min/median/p95; persist 3.921 ms; consume 2.896 ms; pretty 0.468 ms | conservative generation/persistence approximation +2,120 KiB; download +560 KiB |
| heavy export | 5,000 seeded locations / 5,000 exported favorites; 18,940,014 B compact; 702,190 B JSONB; 21,191,249 B pretty | assembly 53.544 / 55.329 / 59.749 ms; persist 255.031 ms; consume 123.902 ms; pretty 36.717 ms | conservative generation/persistence approximation +94,480 KiB (92.3 MiB) |
| phase-isolated current download | 21,190,707 B PostgreSQL JSON text; 416,398 B JSONB; 21,705,865 B pretty | consume 107.844 ms; pretty 26.438 ms | +91,800 KiB (89.6 MiB) |
| representative JPEG | 635,305 B, 2000×1125 → 494,307 B full + 33,428 B thumb | 69.788 / 72.341 / 74.097 ms | included in standalone media process HWM below |
| synthetic exact 20 MP JPEG | 2,401,929 B, 5000×4000 → 3,196,739 B full + 121,637 B thumb; 60,000,000 B decoded RGB | 526.355 / 529.139 / 544.292 ms | standalone process HWM 151,276 KiB |

Raw output:

```text
B18_EXPORT profile=heavy_account_envelope fixture_ms=666.159 seeded_locations=5000 exported_favorites=5000 reviews=1000 review_revisions=5000 verifications=30000 proposals=1000 reports=1000 photos=1000 sessions=90 compact_bytes=18940014 stored_jsonb_bytes=702190 download_pretty_bytes=21191249 assemble_min_ms=53.544 assemble_median_ms=55.329 assemble_p95_ms=59.749 create_ms=255.031 consume_ms=123.902 pretty_serialize_ms=36.717 rss_before_kib=Some(79652) rss_after_assemble_kib=Some(79652) conservative_generation_rss=(7428, 101908, 94480) download_rss=(79652, 167232, 87580) peak_rss_kib=Some(166644)
B18_EXPORT_DOWNLOAD_PHASE json_text_bytes=21190707 stored_jsonb_bytes=416398 pretty_bytes=21705865 consume_ms=107.844 pretty_ms=26.438 rss=(7612, 99412, 91800) hwm_kib=Some(99856)
B18_EXPORT profile=representative fixture_ms=23.862 seeded_locations=100 exported_favorites=100 reviews=50 review_revisions=50 verifications=300 proposals=50 reports=25 photos=10 sessions=5 compact_bytes=180705 stored_jsonb_bytes=6913 download_pretty_bytes=214340 assemble_min_ms=1.677 assemble_median_ms=2.235 assemble_p95_ms=2.661 create_ms=3.921 consume_ms=2.896 pretty_serialize_ms=0.468 rss_before_kib=Some(9360) rss_after_assemble_kib=Some(9360) conservative_generation_rss=(7344, 9464, 2120) download_rss=(9360, 9920, 560) peak_rss_kib=Some(9920)
B18_MEDIA representative_input_bytes=635305 representative_dimensions=2000x1125 representative_full_bytes=494307 representative_thumb_bytes=33428 representative_min_ms=69.788 representative_median_ms=72.341 representative_p95_ms=74.097 upper_input_bytes=2401929 upper_dimensions=5000x4000 upper_full_bytes=3196739 upper_thumb_bytes=121637 upper_decoded_rgb_bytes=60000000 upper_min_ms=526.355 upper_median_ms=529.139 upper_p95_ms=544.292 peak_rss_kib=Some(151276)
```

The `download_rss` tuple in the heavy test is intentionally not decision
evidence: the same process previously generated the payload and retained
allocator arenas. The separate download process is authoritative.

## Re-evaluation triggers

Reconsider an export queue if any documented threshold is crossed, if query time
approaches the configured statement budget, if load testing shows aggregate
pool/RSS pressure, or if observed account histories exceed this envelope.
Export-specific abuse telemetry should track request count, concurrent exports,
payload size, generation/download latency, failures and retention delay. The
absence of an export-specific admission guard and unbounded lifetime history
are reasons to monitor, not claims that the current path is inherently safe.

Reconsider a media queue only if latency/capacity evidence crosses its threshold
and a durable private raw quarantine has explicit owner authorization,
non-enumerable identifiers, idempotent retries, bounded retention, rejection,
account-deletion and orphan cleanup. B19 must validate CPU-count-based codec
permits against actual deployment memory; an in-process worker would not by
itself reduce total RSS.

## Reproduction and validation

All commands unset `DATABASE_URL`, set
`CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target`, and use:

```text
TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit_b18_20260923
```

Run measurements one test process at a time:

```text
cargo test --release -p bikesnest-infrastructure --test audit_b18_measurements b18_export_heavy_account_release_measurement -- --ignored --exact --nocapture --test-threads=1
cargo test --release -p bikesnest-infrastructure --test audit_b18_measurements b18_export_download_phase_release_measurement -- --ignored --exact --nocapture --test-threads=1
cargo test --release -p bikesnest-infrastructure --test audit_b18_measurements b18_export_representative_release_measurement -- --ignored --exact --nocapture --test-threads=1
cargo test --release -p bikesnest-infrastructure --test audit_b18_measurements b18_media_representative_and_20mp_release_measurement -- --ignored --exact --nocapture --test-threads=1
```

Final focused and strict gates:

```text
cargo test -p bikesnest-application privacy::tests::
  PASS: 7
cargo test -p bikesnest-infrastructure --test privacy_test export_ -- --test-threads=1
  PASS: 4
cargo test -p bikesnest-infrastructure --test photo_test processor_ -- --test-threads=1
  PASS: 5
cargo test -p bikesnest-web --test http_test privacy_public_pages_gating_and_export_flow -- --exact --test-threads=1
  PASS: 1
cargo check --workspace --all-targets --all-features
  PASS
cargo clippy --workspace --all-targets --all-features -- -D warnings
  PASS
cargo fmt --all -- --check
  PASS
git diff --check
  PASS
```

The first exact baseline release attempt against the older disposable audit
database stopped before measurement with SQLx `VersionMissing(30)`. That
database had recorded the reverted draft migration. The fresh database above
was created rather than editing or deleting the contaminated base; all reported
measurements and validation results are from the committed 1–29 baseline.

## Changed paths

- `crates/infrastructure/tests/audit_b18_measurements.rs`
- `docs/plans/2026-09-08-audit-remediation.md`
- `docs/reviews/remediation/B18-handoff.md`

No production/domain/application/web behavior, migration, dependency, template,
i18n, environment configuration or deployment artifact remains changed. B18
did not deploy, migrate production, contact providers, send email, or publish
policy.
