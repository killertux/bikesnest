# B18 independent review

Date: 2026-09-23
Baseline: `3f3056b12149e7ee6a2b1a582142a1699b873756`
Reviewed state: uncommitted B18 diff in `/tmp/bikesnest-audit-remediation`
Final verdict: **PASS** (after bounded correction re-review)

The measurements support retaining the current synchronous export and media
paths. Fresh independent release runs stayed far below every documented queue
trigger, the approximately 141 MiB combined-process HWM is not valid
phase-attributed evidence, focused behavior tests and strict Rust gates pass,
and no production behavior or artifact changed. The first review requested
bounded evidence-record corrections; the correction re-review at the end of
this record confirms that they are resolved.

## First-review findings (resolved below)

### 1. Material: the claimed pre-measurement threshold freeze was not evidenced

`docs/reviews/remediation/B18-handoff.md:9-11` says the thresholds were declared
before the measurements, while `docs/plans/2026-09-08-audit-remediation.md:132-141`
calls them frozen. No durable repository or transcript artifact establishes the
complete numeric set before the first results. The implementer's provenance
check likewise found that the earliest exact surviving wording is after an
initial result; compacted context retrospectively describes a full set but has
no timestamp. File mtimes are not proof of decision ordering.

This does not invalidate the no-rewrite result: the handoff fixed the complete
set before this independent review, and all fresh independent reruns remained
well below it. It does make the historical claim unsupported. Correct the
handoff/plan to say that the thresholds were documented and then held fixed for
the independent reruns, without claiming a provable pre-first-measurement
freeze. Preserve the exact numbers; do not move them after these reruns.

### 2. Minor: methodology and fixture descriptions were not exact

Correct these bounded evidence-record inaccuracies:

- `crates/infrastructure/tests/audit_b18_measurements.rs:52-56` and
  `docs/reviews/remediation/B18-handoff.md:90-96` describe 1,000 proposals as
  roughly 200 days at the maximum admission rate. Production permits five per
  hour (`crates/application/src/community.rs:586` and `:819-823`), so this is
  200 hours (about 8.3 days), not 200 days.
- `docs/reviews/remediation/B18-handoff.md:34-38` says assembly uses 14 query
  shapes. Both measured profiles have reviews and therefore execute 15 SQL
  queries in `SqlxExportRepository::assemble_payload`
  (`crates/infrastructure/src/privacy/export.rs:197-459`), counting account and
  roles separately.
- The profile descriptions/logs call the 100/5,000 seeded parking rows
  `locations`, but locations are not an `ExportPayload` collection. The
  harness turns every one into an exported favorite
  (`crates/infrastructure/tests/audit_b18_measurements.rs:172-228`). Describe
  these as seeded locations plus 100/5,000 exported favorites so the measured
  envelope is not mistaken for full location records.
- The generation RSS scope includes a measurement-only compact serialization
  (`crates/infrastructure/tests/audit_b18_measurements.rs:351-383`). Production
  calls assembly and then `create` (`crates/application/src/privacy.rs:632-653`),
  whose adapter uses `serde_json::to_value`, not `to_vec`
  (`crates/infrastructure/src/privacy/export.rs:484-501`). The extra allocation
  makes the RSS result conservative, but it is not an exact reproduction of the
  production POST. Say so instead of calling it the complete original POST.
  Similarly, download-fixture `compact_bytes` is PostgreSQL `payload::text`
  length (`crates/infrastructure/tests/audit_b18_measurements.rs:129-135`), not
  Rust compact serialization; relabel it as JSON text bytes.

These corrections affect interpretation, not the threshold outcome. They do
not justify production changes or a queue rewrite.

## Independent measurements and threshold comparison

Each command used `--locked`, a separate release test process,
`CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target`, the required
`TEST_DATABASE_URL`, and unset `DATABASE_URL` and `BIKESNEST_BROWSER_MUTANT`.

| Path | Frozen trigger | Fresh result | Assessment |
|---|---:|---:|---|
| representative export assembly | p95/max > 1 s | 2.096 ms | below |
| heavy export assembly | p95/max > 3 s | 57.641 ms | below |
| heavy compact output | > 25 MiB | 18,955,133 B (18.08 MiB) | below |
| heavy generation/persist RSS increase | > 128 MiB | 94,300 KiB (92.09 MiB) | below |
| isolated typed download + pretty RSS increase | > 128 MiB | 72,552 KiB (70.85 MiB) | below |
| representative media | p95/max > 1 s | 71.909 ms | below |
| synthetic exact-20 MP media | p95/max > 5 s | 542.113 ms | below |

The export heavy run also measured persistence at 271.399 ms, consumption at
121.856 ms, and pretty serialization at 38.118 ms. The isolated download
measured consumption at 113.243 ms and pretty serialization at 25.067 ms.

With seven export/representative-media samples and five upper-media samples,
the implemented nearest-rank p95 index is the maximum sample
(`audit_b18_measurements.rs:547-551`). The handoff states this limitation at
`:85-88`; it is an observed maximum, not a production SLO or statistical tail
estimate.

I **reject** the earlier approximately 141 MiB combined-process HWM as a queue
trigger. A process HWM is monotonic, and that process performed fixture setup,
generation, repeated assemblies, persistence, download and pretty
serialization. Allocator arenas retained across phases prevent attribution to
either HTTP phase. The fresh process-isolated generation and download deltas
are the relevant evidence and are both below 128 MiB.

Media memory is more limited evidence: the harness reports an absolute
single-process HWM of 150,816 KiB after constructing the synthetic input and
running representative and repeated 20 MP work. It does not establish a
per-image delta or deployment concurrency capacity. The handoff handles this
honestly at `:148-153`: CPU-count/process/deployment memory validation remains a
B19 gate, and moving work into an in-process worker would not reduce total RSS.

## Measurement validity and production-code cross-check

- The harness contains exactly four `#[test]` functions and exactly four
  `#[ignore]` attributes (`audit_b18_measurements.rs:70-84`, `:454-456`). A
  normal invocation reported `0 passed; 4 ignored`. There are no timing or RSS
  pass/fail assertions; ordinary CI only compiles this target.
- Export tests use the real `SqlxExportRepository`; media uses production
  `LocalImageProcessor` with default 10 MiB/20 MP/quality-85/400 px limits.
- The PostgreSQL-built download fixture successfully deserializes through the
  production typed `consume_download` path before pretty serialization. Its
  schema-version-2 object contains every required top-level/account/review
  field and empty arrays for optional collections.
- Export POST remains synchronous and writes `READY` directly
  (`application/privacy.rs:632-667`; `infrastructure/privacy/export.rs:484-512`).
  Download checks authenticated ownership in the service, then token, state and
  expiry and performs a guarded READY-to-DOWNLOADED transition
  (`application/privacy.rs:678-707`; `infrastructure/privacy/export.rs:557-635`).
  There are no retained PENDING/FAILED states.
- Scheduled retention deletes every expired export state
  (`infrastructure/privacy/retention.rs:148-160`), while the repository-local
  purge is READY-only (`infrastructure/privacy/export.rs:638-652`), and account
  anonymization explicitly deletes exports
  (`infrastructure/privacy/anonymize.rs:204`).
- Media body/rate/codec bounds, `spawn_blocking` execution and closure-owned
  permit are present (`application/photo.rs:286-289`, `:396-418`;
  `infrastructure/photo/processor.rs:33-45`, `:82-170`). Raw input is not
  stored; only stripped derivatives are written, writes are compensated, and
  the row is inserted after both objects (`application/photo.rs:387-475`). New
  rows are private PENDING_REVIEW, rejection deletes derivatives
  (`application/photo.rs:535-581`), and retention has a 24-hour orphan sweep and
  one-hour broken-pending reconciliation
  (`infrastructure/privacy/retention.rs:19-26`, `:162-255`). No durable private
  raw-upload quarantine exists.
- Export fixture writes use the transaction-scoped `Db`; post-run read-only
  checks found zero marked B18 users, locations, sessions, photos and phase
  exports. Media performs no object-store writes. No external provider or
  production system is contacted.

## Commands and results

The first sandboxed heavy-measurement attempt was denied before connecting
(`Operation not permitted`). The identical explicitly authorized loopback run
then passed; this is an environment denial, not a test failure.

- Four exact release measurement commands from the handoff, each with
  `--locked --release --ignored --exact --nocapture --test-threads=1`: **4/4
  passed**, one process at a time.
- `cargo test --locked -p bikesnest-infrastructure --test audit_b18_measurements`:
  **0 passed, 4 ignored**.
- `cargo test --locked -p bikesnest-application privacy::tests::`: **7/7
  passed**.
- `cargo test --locked -p bikesnest-infrastructure --test privacy_test export_
  -- --test-threads=1`: **4/4 passed**.
- `cargo test --locked -p bikesnest-infrastructure --test photo_test processor_
  -- --test-threads=1`: **5/5 passed**.
- `cargo test --locked -p bikesnest-web --test http_test
  privacy_public_pages_gating_and_export_flow -- --exact --test-threads=1`:
  **1/1 passed**.
- `cargo check --locked --workspace --all-targets --all-features`: **passed**.
- `cargo clippy --locked --workspace --all-targets --all-features -- -D
  warnings`: **passed**.
- `cargo fmt --all -- --check`: **passed**.
- `git diff --check`: **passed**.
- Read-only database identity/migration query in the explicitly identified
  `bikesnest-audit-test-20260908` loopback container:
  `bikesnest_test_audit_b18_20260923|bikesnest_test`, `29|29|t`.
- Read-only B18 marker cleanup query after all runs: `0|0|0|0|0`.

## Scope verification

Before this review record, `git status --short` showed only:

- modified `docs/plans/2026-09-08-audit-remediation.md`;
- untracked `crates/infrastructure/tests/audit_b18_measurements.rs`;
- untracked `docs/reviews/remediation/B18-handoff.md`.

Direct guards found no changes to production/domain/application/web source,
Cargo manifests or lockfile, migrations, templates, `.env.example`, Docker or
CI/deployment artifacts, generated/static assets, or unrelated tests. The only
additional path created by this reviewer is this authorized review file. The
branch remains `fix/audit-remediation` at the requested baseline, with no
commit, deployment, database creation/deletion/alteration, provider call,
email, policy action or production write.

## Residual risks after the requested corrections

- Account history has no finite cap and exports have no export-specific
  admission/cooldown/one-active-export guard. The synthetic profiles are
  re-evaluation points, not observed production percentiles or upper bounds.
- Export measurements are local single-request measurements, not concurrent
  load, pool-pressure or statement-budget evidence.
- Media absolute HWM does not prove safe aggregate capacity at production CPU
  count. B19 must validate process layout, concurrency and memory limits.
- Scheduled-retention delay/disablement can retain exports beyond 24 hours;
  backups and operational retention remain external evidence.
- This verdict does not authorize deployment or any B19 external action.

## Correction re-review — PASS

The same independent reviewer inspected the frozen bounded corrections against
both findings above. No finding remains open.

- Threshold chronology is now exact. The handoff says the complete set was
  documented and held fixed before the fresh independent reruns, while
  explicitly admitting that no durable record proves it preceded the first
  implementation measurements (`B18-handoff.md:9-14`). The plan makes the same
  narrower claim (`2026-09-08-audit-remediation.md:132-139`).
- All threshold values are unchanged: export remains 1 s representative, 3 s
  heavy, 25 MiB compact output and 128 MiB request-phase RSS; media remains 1 s
  representative and 5 s synthetic upper-envelope. The conditional media
  capacity/quarantine caveat is also unchanged.
- The proposal horizon is corrected to 200 hours/about 8.3 days in the harness
  and handoff (`audit_b18_measurements.rs:52-56`;
  `B18-handoff.md:99-107`).
- The handoff now says the measured nonempty-review profiles execute 15 SQL
  queries, counting account and roles separately (`B18-handoff.md:41-45`).
- The harness and handoff distinguish 100/5,000 seeded parking rows from the
  100/5,000 favorites actually exported. The harness additionally asserts the
  exported favorite count (`audit_b18_measurements.rs:25-68`, `:172-228`,
  `:369-371`, `:431-456`; `B18-handoff.md:99-107`, `:132-145`).
- The generation RSS scope is accurately described as a conservative
  approximation: it includes measurement-only compact `to_vec` before
  production `create` performs its separate `to_value` conversion. It no
  longer claims an exact production POST reproduction
  (`audit_b18_measurements.rs:351-388`; `B18-handoff.md:109-118`).
- PostgreSQL `payload::text` length is consistently labeled
  `json_text_bytes`/PostgreSQL JSON text rather than Rust compact serialization
  (`audit_b18_measurements.rs:129-151`; `B18-handoff.md:132-146`). The handoff's
  historical numeric output is semantically relabeled; no result was changed.

Proportionate correction gates:

- `cargo test --locked -p bikesnest-infrastructure --test
  audit_b18_measurements --no-run`: **passed**.
- `cargo fmt --all -- --check`: **passed**.
- `git diff --check`: **passed**.
- Threshold/correction label guards: **passed**.
- Scope guards against Cargo manifests/lockfile, environment, Docker/CI,
  migrations, templates/static assets and all production domain/application/
  infrastructure/web source: **passed**.

`git status --short` still contains only the B18 plan, measurement harness,
handoff, and this authorized review file. There is no production behavior,
migration, dependency, lockfile, template, environment, deployment or unrelated
file drift. The earlier four release measurements, focused suites, workspace
check, strict Clippy and cleanup evidence remain valid because the correction
changed only test naming/assertion/commentary and documentation, not the
measured algorithm or production code.

B18 is safe to checkpoint as a measurement-backed decision to retain
synchronous export and media processing. This PASS does not authorize
deployment or any B19 external action; all residual risks listed above remain.
