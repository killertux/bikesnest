# B19 implementation handoff

Date: 2026-09-23
Baseline: accepted B18 checkpoint `11cbf00`
Technical checkpoint: `e0ac245`
Status: local/source gate passed independent review; awaiting release evidence;
not deployed

This is an implementation handoff, not self-approval or release authorization.
It records source and disposable-test evidence only. No production checkout,
database, provider, edge, DNS, policy, mail, job, registry, pull request or
deployment state was changed. No real restore was executed.

## Outcome

B19 removes five vulnerable Rust dependency paths, migrates the direct
MapLibre dependency from 4.7.1 to the official patched 6.11.1 ESM release, and
reduces the B17 exception register from seven entries to the one inactive
SQLx/RSA lockfile artifact that cannot yet be removed safely. The retained RSA
entry remains exact and time-bounded, with updated attempted-remediation and
removal rationale pending independent B19 re-review rather than an unbounded
acceptance.

The prior image processor sized its permit count from this audit runtime's
available parallelism: both `nproc` and `_NPROCESSORS_ONLN` report 8, hence up
to eight simultaneous codec jobs. B18 measured an absolute 20 MP standalone
process high-water mark of 151,276 KiB and 60,000,000 decoded RGB bytes. Those
figures cannot be multiplied into an exact RSS forecast, and no production
memory limit was supplied. B19 therefore replaces CPU-derived admission with
`PHOTO_PROCESSING_CONCURRENCY`, bounded to Tokio's semaphore range and
defaulting to one per process. Cancellation still leaves the owned permit with
started blocking work. A production-like staging capacity run remains an
external release gate even at one permit; raising it requires measured
headroom, not a CPU-count assumption.

All source-controlled and disposable local gates completed successfully except
for a fresh npm advisory query. The approval system rejected both attempts to
contact the live npm advisory service because direct user approval was not
available; no workaround or indirect registry query was attempted. The exact
6.11.1 package, lockfile provenance, official fix release and local dependency
tree are present, and CI remains fail closed, but a fresh `npm audit --json`
result is an explicit release blocker for the independent reviewer/release
owner. It is not represented below as a passing scan.

## Advisory inventory

| B17 finding | Before | B19 disposition | Evidence after |
|---|---|---|---|
| `GHSA-JRC7-96C5-Q579` | direct `maplibre-gl` 4.7.1, critical | remediated in source by official 6.11.1; exception removed | `npm ls maplibre-gl --all` resolves exactly 6.11.1; lock records the 6.11.1 tarball/integrity; official release identifies the attribution allowlist sanitization fix; fresh npm scanner still pending approval |
| `RUSTSEC-2026-0285` | `rustls` 0.23.43 | remediated; exception removed | `rustls` 0.23.45 active; RustSec says patched `>=0.23.45`; fresh cargo-audit no longer reports it |
| `RUSTSEC-2026-0258` | legacy AWS `h2` 0.3.27 | remediated; exception removed | S3 defaults disabled and only the maintained `default-https-client` selected; `h2` 0.3.27 absent, 0.4.19 active; MinIO round trip passed; fresh cargo-audit no longer reports it |
| `RUSTSEC-2026-0104` | legacy AWS `rustls-webpki` 0.101.7 | remediated; exception removed | 0.101.7 absent, 0.103.15 active; RustSec says patched `>=0.103.13`; fresh cargo-audit no longer reports it |
| `RUSTSEC-2026-0098` | legacy AWS `rustls-webpki` 0.101.7 | remediated; exception removed | 0.101.7 absent, 0.103.15 active; RustSec says patched `>=0.103.12`; fresh cargo-audit no longer reports it |
| `RUSTSEC-2026-0099` | legacy AWS `rustls-webpki` 0.101.7 | remediated; exception removed | 0.101.7 absent, 0.103.15 active; RustSec says patched `>=0.103.12`; fresh cargo-audit no longer reports it |
| `RUSTSEC-2023-0071` | `rsa` 0.9.10 through lock-only `sqlx-mysql` | attempted principled pruning; one exception retained through 2026-10-23 | SQLx defaults are off and only Tokio/Rustls, Postgres, migrate, macros, Chrono and JSON remain. `cargo tree --target all -i rsa@0.9.10` and `sqlx-mysql@0.8.6` both print nothing; retained macro package metadata still locks the optional driver. RustSec lists no patched version |

Fresh cargo-audit 0.22.2 updated the official RustSec database and returned
exit 1 with exactly one finding, `RUSTSEC-2023-0071`, and no warnings. The
fail-closed validator accepted that exact current exception and its seven
fixture tests passed. Commands looking up `h2@0.3.27`, `rustls@0.21.12`, and
`rustls-webpki@0.101.7` for all targets fail because those package versions are
not present in the lock resolution.

Removing SQLx `macros` merely to make the lockfile omit RSA would replace the
retained `FromRow` derives and migration integration across the application
with a broad unrelated rewrite. The package has no active current-target or
all-target path. The exception must be removed when SQLx stops locking the
optional driver, RSA publishes a patch, or the macro stack is deliberately
replaced; it must be re-reviewed immediately if MySQL/`any` or any other
feature makes the package reachable.

Primary upstream references used for the decisions:

- [MapLibre GL JS 6.11.1 release](https://github.com/maplibre/maplibre-gl-js/releases/tag/v6.11.1)
  and the official [v5 to v6 migration guide](https://maplibre.org/maplibre-gl-js/docs/guides/v5-to-v6-migration-guide/).
- [aws-sdk-s3 1.144.0 manifest and features](https://docs.rs/crate/aws-sdk-s3/1.144.0/source/Cargo.toml.orig),
  showing that the default set includes both the legacy `rustls` connector and
  `default-https-client`.
- RustSec advisories
  [`RUSTSEC-2026-0285`](https://rustsec.org/advisories/RUSTSEC-2026-0285.html),
  [`RUSTSEC-2026-0258`](https://rustsec.org/advisories/RUSTSEC-2026-0258.html),
  [`RUSTSEC-2026-0104`](https://rustsec.org/advisories/RUSTSEC-2026-0104.html),
  [`RUSTSEC-2026-0098`](https://rustsec.org/advisories/RUSTSEC-2026-0098.html),
  [`RUSTSEC-2026-0099`](https://rustsec.org/advisories/RUSTSEC-2026-0099.html),
  and unpatched [`RUSTSEC-2023-0071`](https://rustsec.org/advisories/RUSTSEC-2023-0071.html).

## Dependency and browser integration details

The AWS S3 crate remains at 1.144.0; this is a feature correction, not a blind
version jump. Its defaults selected the maintained client and a redundant
legacy Hyper 0.14/rustls 0.21 connector. B19 disables defaults and explicitly
retains `sigv4a`, `http-1x`, `default-https-client`, and `rt-tokio`. A real
local MinIO operation put an object, created a SigV4 presigned URL, observed
the object, deleted it, and observed its absence.

MapLibre 6 is ESM-only and no longer ships the old UMD file. The five map
templates now load a small nonce-bearing module. The module imports the main
SDK from the stable same-origin `/static/vendor` path, assigns that namespace to
`window.maplibregl`, and sets an explicit same-origin worker URL. The separate
MapLibre adapter publishes the provider-neutral contract and does not accept or
assign a Mapbox browser token. Legacy `mapbox://` and `api.mapbox.com` styles
instead resolve to the existing Mapbox profile and require its browser token;
other styles remain token-free MapLibre. The main,
shared and worker modules deliberately use one stable directory: placing the
main module behind its individual hashed URL would make its relative shared
module import inherit the wrong per-file hash and return 404. The small loader
itself remains content-hashed. Lazy htmx asset insertion preserves `type=module`.
Mapbox remains on its separate classic bundle and provider adapter.

Actual Chromium coverage proves direct hard load, delayed lazy navigation,
same-page retry, strict enforced CSP, worker request, SDK construction, empty
map load, and failed-tile error surfacing. A source search finds no production
template request for the removed `maplibre-gl.js` UMD asset.

## Integrated validation

Environment identity and isolation:

- audit worktree branch/HEAD: `fix/audit-remediation` at `11cbf00`;
- existing audit PostGIS container: `bikesnest-audit-test-20260908`, loopback
  port 55439, role `bikesnest_test`, identity checked read-only before use;
- explicitly created disposable base database:
  `bikesnest_test_audit_b19_20260923`;
- explicitly named disposable services: `bikesnest-audit-minio-b19` on
  `127.0.0.1:59000` and `bikesnest-audit-valkey-b19` on
  `127.0.0.1:56381`;
- `DATABASE_URL` unset; every database-backed command used the explicit
  `TEST_DATABASE_URL`; external providers remained fake or loopback-local.

Results:

```text
cargo test --workspace --locked
  PASS: 761 passed, 0 failed, 10 ignored
  ignored = four opt-in B18 measurement tests + the six explicit browser tests
  S3 and single-node Valkey integration ran against the named local services

cargo test -p bikesnest-infrastructure --test migration_upgrade_test --locked
  PASS: 1/1; committed migration subset 1–27 upgraded to current 29
  preserved a pre-upgrade policy row and proved new policy tables

read-only _sqlx_migrations query after fresh base migration
  29|29|t  (count|max version|all successful)

six explicit --locked -- --ignored targets, BIKESNEST_BROWSER_MUTANT unset
  email renderer 1/1 (5.11s)
  contribution 1/1 (3.48s)
  CSRF 1/1 (3.15s)
  policy 1/1 (4.11s)
  profile 1/1 (2.23s)
  search 1/1 (4.44s final bounded-correction rerun)

npm run build:assets && npm run build:css
  PASS; pre/post `git diff` snapshots were byte-identical (1,072,191 bytes)
npm run test:browser
  PASS after the final independent-review correction: 28/28 in 21.46s

cargo test -p bikesnest-infrastructure config::tests --locked
  PASS after the final independent-review correction: 36/36

node --check web/static/js/maplibre-loader.mjs
node --check web/static/js/map-provider-maplibre.js
node --check tests/browser/search-app.cjs
node --check tests/browser/navigation.test.cjs
  PASS

docker build -t bikesnest-audit-b19:11cbf00 .
  PASS on pinned rust:1.95; release compile 4m10s
  local manifest-list digest:
  sha256:a389d596c74509d8e8509d8c5fd546df814d0787e7fba91f78bc575d4a07e85c

cargo check --workspace --all-targets --all-features --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
git diff --check
  PASS

cargo test -p bikesnest-infrastructure admission_tests --locked
  PASS: 3/3, including cancellation permit retention, actual running-work peak,
  and direct-constructor zero/over-maximum rejection

cargo test -p bikesnest-infrastructure --test s3_object_storage --locked
  PASS: 1/1 against disposable MinIO; no early-return/skip output

node scripts/verify-advisories.test.cjs
  PASS: 7/7
cargo-audit 0.22.2 fresh RustSec report + validator
  scanner exit 1; exactly RSA finding; validator PASS
npm audit --json
  NOT RUN: live registry access rejected pending direct user approval
```

The forward-upgrade helper uses the same loopback/name validation,
`catch_unwind`, awaited pool close and exact owned child-database cleanup as the
existing isolated runner. It differs only by leaving the new child empty so the
test can run the exact committed 1–27 migration subset before current 28–29.
No migration file, checksum or applied migration row was edited.

The first run of the strengthened search-browser assertion failed before map
loading because `.search-toggle` matched both the map and filter controls.
Changing the test locator to the exact `aria-controls="search-map-panel"`
button resolved that test-harness ambiguity.

Independent review then found a separate implementation defect: the original
loader assigned the non-extensible ESM module namespace directly to
`window.maplibregl`, while the existing strict adapter attempted to write a
non-empty browser token to `maplibregl.accessToken`. That configuration would
throw before map construction. A draft mutable-facade correction removed the
exception but did not make MapLibre apply the token to Mapbox requests, so it
was rejected rather than treated as sufficient.

The final bounded correction keeps the direct ESM namespace, removes the dead
MapLibre token assignment, and makes config resolution route legacy
`mapbox://` and `api.mapbox.com` styles to the separate Mapbox profile. That
profile requires `MAPBOX_MAP_ACCESS_TOKEN` (or the documented legacy fallback);
a missing token is a startup error. Other styles remain token-free MapLibre.
Real-browser coverage constructs the MapLibre map through the actual Axum
router and proves the hashed loader plus stable main/shared/worker requests
with no module request or page errors. Separate navigation coverage loads the
real vendored Mapbox SDK and adapter with a non-empty token, locally intercepts
its `api.mapbox.com` and `events.mapbox.com` requests, and proves each captured
request carries that exact token without contacting a provider. The first
strengthened navigation run exposed a test CSP omission (27/28); after the
fixture allowed the same Mapbox origins as production and retained local
interception, it passed 28/28. This correction is frozen for reviewer re-check;
it does not claim independent acceptance.

## Staged release and rollback plan

No stage below is authorized by this handoff.

### Stage 0 — independent source/release review

1. An independent reviewer verifies this diff and repeats the strict gates.
2. With explicit network approval, the security/release owner runs fresh
   `npm audit --json`, preserves its exit status/report, and runs the fail-closed
   validator. Any current or new advisory returns to implementation; do not
   extend an exception by default.
3. Produce a commit-addressed SBOM/advisory record and rebuild the image from
   that reviewed commit. The local `11cbf00` tag above names the baseline, not
   a final B19 commit.

Decision: stop unless independent review passes, npm's current report is
accounted for, and the rebuilt image matches the reviewed source.

### Stage 1 — owner/counsel/provider/operations evidence

The accountable parties complete the external checklist below. Operations
records the actual database version and whether migrations 26/27 are already
present. If upgrading from before 26/27, stop and drain every old worker and
inline mail-producing web instance before migration; old workers cannot safely
decode the new payloads. Keep `POLICY_ACKNOWLEDGEMENT_ENABLED=false`; this
release does not authorize policy seeding or publication.

In a production-like staging environment, operations uses the real container
memory/CPU limits and replica/process layout to measure baseline RSS,
single-permit 20 MP processing, concurrent export/media load, DB pool pressure,
and OOM/restart behavior. Start with `PHOTO_PROCESSING_CONCURRENCY=1`. Record
export latency/size/concurrency/retention delay because exports remain
synchronous with unbounded lifetime history and no export-specific admission.

Decision: stop unless one codec job and the agreed export envelope have
adequate measured headroom. Raising photo concurrency needs its own evidence.

### Stage 2 — backup, schema and rollback rehearsal

Operations creates/verifies the approved pre-release backup and separately
performs the documented restore exercise on an approved sandbox, including
media and the mandatory retention/erasure reconciliation. Test the exact
rollback binary against schema 29. A pre-B15b acknowledgement-unaware binary
is not acceptable for enabled registration, and the accepted B18 image still
contains the remediated advisories, so it is not an automatic security-safe
rollback target. Prepare a reviewed rollback/forward-fix image that retains
the B19 dependency fixes.

Decision: stop without restore evidence and an explicitly named compatible,
patched rollback target.

### Stage 3 — authorized canary

After a separately approved window, deploy the reviewed digest to one canary
with photo concurrency one. Wait for `/readyz`; verify schema 29, static module
and worker 200/MIME/CSP behavior, S3 put/presign/read/delete behavior on an
approved test object, Valkey rate limits, map provider behavior, mail queue
health, and the two exact recurring-job rows without altering unexpected data.
Observe RSS, OOM, CPU, export latency, image latency, error rate and CSP reports.

Decision: halt and drain the canary on any migration, map, storage, queue,
capacity or advisory discrepancy. Do not reconcile production jobs without
separate approval.

### Stage 4 — controlled fleet rollout

Roll one instance at a time only after canary approval. Gate each instance on
`/readyz`, drain the replaced instance, and confirm fleet-consistent policy
flags and worker versions. Continue capacity/error/CSP monitoring through the
agreed observation window. Policy publication, acknowledgement activation,
provider/DNS/edge changes and outbound notices remain separate changes.

### Rollback and recovery

- Before database migration or user traffic, remove the canary and retain the
  old fleet.
- After migrations 26/27, do not restart an old mail worker. Pause mail and
  roll forward with a patched compatible image.
- After migrations 28/29, use only an acknowledgement-aware rollback binary;
  otherwise pause/drain registration. Never invent acknowledgement records.
- A MapLibre rollback must not restore vulnerable 4.7.1. Use the prepared
  patched rollback image, an already approved alternative provider profile, or
  pause affected map traffic while forwarding a fix.
- Schema migrations are forward-only. Do not edit migration history or perform
  a down migration. A database restore is a separately authorized disaster
  recovery action, not an ordinary application rollback; after restore run
  retention and reconcile later erasures/revocations before serving traffic.

## Release evidence checklist

| Responsible party | Required evidence | Decision/action |
|---|---|---|
| Security/release owner | fresh npm audit v2 report and validator result from the reviewed commit; review of the one RSA exception before 2026-10-23 | block release on a new/unreviewed finding or stale/reachable RSA path |
| Independent reviewer | **Satisfied for the local/source gate:** [B19 review](B19-review.md) covers dependency features, ESM/hash/CSP/worker behavior, the corrected provider-token contract, concurrency bounds, migration helper and complete gates | source PASS recorded after one correction round; this is not release authorization |
| Release owner | final commit SHA, reproducible image digest, SBOM/advisory artifact and named patched rollback image | authorize or reject release candidate |
| Platform/operations | actual production CPU/memory limit, replica/process layout, baseline RSS, single-permit 20 MP and concurrent export/media staging results, DB pool/OOM evidence | keep concurrency 1 or reject/tune with measured headroom |
| Database/operations | actual production migration state, read-only 26–29 preflight, maintenance window, old-worker drain proof, schema-29 canary result | authorize migration sequence or stop |
| Backup/incident owners | current encrypted backup evidence plus approved sandbox restore, matching media restore, retention/erasure reconciliation, RPO/RTO result | authorize rollback/recovery readiness; no real restore from this handoff |
| Job owner/operations | read-only inventory of exact recurring keys and post-canary schedule/ownership/outcome evidence | authorize any reconciliation separately; never delete history as repair |
| Storage/provider owner | selected S3 provider/region/role/DPA, bucket-private policy, CORS/presign behavior and approved test-object evidence | approve storage boundary or stop |
| Map/provider owner | selected production map profile, restricted keys/tokens, tile/style hosts, live MapLibre/WebGL2 support if selected | approve provider and CSP inputs or stop |
| Edge/DNS owner | TLS/HSTS, trusted proxy hops, cache rules, CSP report review, Cloudflare telemetry decision, DNS and rollback TTL evidence | approve edge change separately or stop |
| Email/DNS owner | SMTP/Resend contract/configuration, monitored sender, SPF/DKIM/DMARC and delivery/idempotency evidence | approve mail readiness; no message was sent here |
| Counsel/controller | bilingual text, controller/contact, legal bases, territorial/age scope, retention, provider roles/transfers/DPA and notice/materiality decisions | approve or reject policy publication; source drafts are not approval |
| Policy owner | exact six-document version/effective time, rendered review, fleet-consistent acknowledgement plan and authorized notice channels | only then authorize `seed-policies`/activation; neither occurred here |
| Incident owner | named private roster, escalation channels, protected case system, alert coverage and regulatory decision authority | approve operational readiness |
| Security owner | authenticated staging/penetration scope, privileged-MFA decision and evidence | record residual risk or require follow-up before launch |

Only the independent local/source review row is satisfied. Every remaining row
requires evidence or authority outside the local source gate.

## Changed paths

- Dependency/advisory inputs: `Cargo.toml`, `Cargo.lock`,
  `crates/infrastructure/Cargo.toml`, `package.json`, `package-lock.json`,
  `ci/advisory-exceptions.json`.
- Capacity/configuration: `.env.example`, `ARCHITECTURE.md`,
  `docs/deployment.md`, `crates/infrastructure/src/config.rs`,
  `crates/infrastructure/src/photo/processor.rs`, wiring/main and the affected
  processor/devdata/B18 measurement tests.
- MapLibre ESM integration: five map page templates,
  `web/static/js/maplibre-loader.mjs`, `web/static/js/navigation.js`, generated
  MapLibre CSS/main/shared/worker modules, removal of the old UMD bundle,
  `tests/browser/navigation.test.cjs`, and related web tests/comments.
- Migration evidence: `crates/test-support/src/lib.rs`, `TESTING.md`, and
  `crates/infrastructure/tests/migration_upgrade_test.rs`.
- Review record: this handoff and the B19 status/progress entry in the audit
  remediation plan.

The disposable B19 base database was intentionally left in place through
independent review. The explicitly named MinIO and Valkey containers are test
services only and may be removed after review. The local Docker image was not
pushed. After the reviewer passed the exact technical tree, the remediation
lead checkpointed it as `e0ac245`; this handoff, the independent review and the
plan status are recorded separately.
