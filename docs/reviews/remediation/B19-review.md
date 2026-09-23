# B19 independent review

Date: 2026-09-23
Reviewer: independent GPT Sol review
Baseline: accepted B18 checkpoint `11cbf00`
Reviewed state: branch `fix/audit-remediation`, baseline HEAD
`11cbf0012c7f6ee4eef37b78b14fb7a40e6d12e1`, including every tracked and
untracked B19 path listed by `git status --short`
Final disposition after correction re-review: **PASS for the local/source B19
gate only**, with the fresh npm scan and every external release gate still
blocking release; no release or production action is authorized

The initial review history is retained below. That round found a reproducible
source regression before the complete integrated rerun matrix and requested a
bounded correction. The later sections record the correction and fresh
independent reruns; implementer evidence is not relabelled as reviewer
evidence.

## Initial review finding (resolved by the correction)

### High — token-bearing MapLibre configurations threw before map construction

`web/static/js/maplibre-loader.mjs:4-7` assigns the MapLibre 6 ESM namespace
object directly to `window.maplibregl`. The existing strict-mode adapter then
writes `window.maplibregl.accessToken` whenever the rendered configuration has
a token (`web/static/js/map-provider-maplibre.js:5-15`). ESM namespace objects
are non-extensible, and the vendored MapLibre 6.11.1 namespace has no
`accessToken` export. The write therefore throws before `new Map(...)`.

This is an exercised configuration, not dead code: `resolve_map_config`
deliberately retains a token for Mapbox-style URLs in the MapLibre profile, and
its tests assert both the dedicated and geocoder-fallback token cases
(`crates/infrastructure/src/config.rs:1024-1044,1937-1958`). The new real-router
browser path uses an empty token, while the lazy fixture replaces the loader
with a mutable `{}` stub (`tests/browser/search-app.cjs:133-147` and
`tests/browser/navigation.test.cjs:111-119`), so neither detects the failure.
The actual-SDK construction test imports MapLibre directly and does not execute
the production loader plus adapter token branch
(`tests/browser/navigation.test.cjs:500-515`).

An independent loopback Chromium reproduction loaded the committed
`maplibre-loader.mjs`, then the committed adapter, with
`data-map-access-token="token"` and a local empty style. It returned:

```text
{"namespaceExtensible":false,"mapCreate":"TypeError:Cannot assign to property 'accessToken' of [object Module]","hasAccessToken":false}
```

Required bounded correction: make the MapLibre 6 loader/adapter and the
configuration contract agree without mutating an ESM namespace. Preserve the
token-bearing configuration correctly, or explicitly reject/migrate that
configuration to the Mapbox profile; do not merely copy the namespace to make
the assignment stop throwing unless the token is also demonstrably applied to
the requests that need it. Add actual-browser coverage using the real vendored
loader and production adapter with a non-empty configured token. Retain the
existing hard-load, lazy-navigation, retry, worker, MIME/hash and enforced-CSP
coverage.

## RSA exception decision

The sole remaining exception, `RUSTSEC-2023-0071`, is acceptable **only as the
current time-bounded inactive lockfile artifact**, not as acceptance of a
reachable vulnerable implementation.

- `cargo tree --locked -i rsa@0.9.10`, `--target all`, and
  `--all-features --target all` each printed `warning: nothing to print`.
- The same three current/all-target/all-feature inverse-tree checks for
  `sqlx-mysql@0.8.6` also printed nothing.
- The active feature tree reaches `sqlx-macros-core` through the deliberately
  retained SQLx derive/migration macro stack, while lock inspection shows that
  package metadata declares optional `sqlx-mysql`; the lock therefore contains
  `sqlx-mysql -> rsa` without an active build path.
- Disabling SQLx defaults removed the active `any`/legacy driver selection.
  Removing the remaining macro stack would be a broad unrelated rewrite of
  `FromRow` and embedded migration uses, not a proportionate B19 workaround.
- The register fields are truthful and specific: owner `BikesNest security
  maintainers`, reviewer `Audit remediation lead`, review date 2026-09-23,
  expiry 2026-10-23, exact advisory ID, no patched-release claim, explicit
  feature-reachability trigger, and concrete removal criteria.

The exception must fail closed at expiry, be removed if SQLx/RSA supplies a
safe resolution, and be re-reviewed immediately if MySQL, `any`, or another
feature makes either package reachable. This decision does not waive the
source finding above or any release gate.

## Commands and results independently run in this round

All commands ran in `/tmp/bikesnest-audit-remediation`. Rust commands used
`CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target`; database variables
were unset for database-free commands.

```text
git branch --show-current
git rev-parse HEAD
git status --short
git diff --stat 11cbf00
git diff --name-status 11cbf00
git ls-files --others --exclude-standard
  PASS: correct worktree/branch and exact B19 tracked/untracked inventory

cargo tree --locked -i rsa@0.9.10
cargo tree --locked --target all -i rsa@0.9.10
cargo tree --locked --all-features --target all -i rsa@0.9.10
cargo tree --locked -i sqlx-mysql@0.8.6
cargo tree --locked --target all -i sqlx-mysql@0.8.6
cargo tree --locked --all-features --target all -i sqlx-mysql@0.8.6
  PASS: all six checks reported no active inverse path

cargo tree --locked -e features -i sqlx-macros-core@0.8.6
  PASS: active macro/derive/migrate/Postgres feature path inspected

npm ls maplibre-gl --all --offline
  PASS: exactly maplibre-gl@6.11.1

temporary loopback Chromium reproduction using the committed loader, adapter,
vendored main/shared modules and a local empty style
  FAIL as expected: non-empty token throws TypeError before map construction

cargo fmt --all -- --check
git diff --check
  PASS

cargo test -p bikesnest-infrastructure \
  photo_processing_concurrency_is_bounded_and_configurable --locked
  PASS: 1 passed

cargo test -p bikesnest-infrastructure admission_tests --locked
  PASS: 3 passed; constructor bounds, cancellation permit retention and actual
  blocking-work peak all passed

node scripts/verify-advisories.test.cjs
  PASS: 7 passed
```

The image-processing code independently reviewed in this round has the right
ownership shape: configuration defaults to one and rejects zero or values over
`Semaphore::MAX_PERMITS`; the owned permit is moved into the blocking closure,
so cancellation of the awaiting HTTP future cannot release capacity while the
codec work continues. Production memory/capacity remains a staging evidence
gate rather than a conclusion from these unit tests.

The new unmigrated-database helper also reuses the existing validated
loopback/name ownership, unique child name, closure `catch_unwind`, bounded
pool close, forced exact-child drop, and panic resumption path. Its 27-to-29
test constructs the committed migration subset from the current embedded set
and checks preservation plus the new proof tables. Runtime migration reruns are
deferred to the corrected-diff review round; this paragraph is source review,
not a fresh passing migration claim.

## Evidence reviewed but not independently rerun yet

The handoff reports 761 workspace tests with 10 expected ignores, all six
ignored renderer/browser targets explicitly passing, fresh 1-to-29 and real
27-to-29 migration evidence, MinIO/Valkey checks, deterministic frontend
rebuild, browser 28/28, a pinned-Rust-1.95 Docker build, all-target/all-feature
check, strict Clippy, fresh cargo-audit with exactly the RSA finding, formatting
and diff checks. The reported environment and counts are internally coherent,
and the handoff distinguishes local evidence from external evidence. These
remain implementer/lead evidence until the corrected source receives the
proportionate independent reruns required by the B19 gate.

## Unsatisfied release gates

Fresh npm advisory evidence is explicitly missing. Per the review instruction,
I did **not** run `npm audit` or contact the npm registry. The local 6.11.1
lock/tree and removal of the old direct exception are useful source evidence,
but they are not a current advisory scan. A security/release owner with explicit
network approval must preserve the fresh npm v2 report and exit status and run
the fail-closed validator from the reviewed commit. Any current or unreviewed
finding blocks release.

All external gates in the handoff remain unavailable: owner/counsel approval;
provider entities, regions, roles, DPAs/transfers and live key restrictions;
edge/TLS/proxy/cache/CSP-report and DNS evidence; SPF/DKIM/DMARC and delivery
evidence; actual production migration/job inventory and old-worker drain;
production-like image/export capacity; backup plus approved sandbox restore and
retention/erasure reconciliation; authenticated staging/penetration scope and
privileged-MFA decision; final commit-addressed SBOM, reproducible image digest,
named patched rollback image, release window and deployment authorization. No
production release can pass while these and the fresh npm report are missing.

## Initial-round verdict (superseded after correction)

**CHANGES REQUESTED.** The B19 local/source gate does not pass because a
supported token-bearing MapLibre configuration reproducibly fails under the
new ESM bridge, and the added tests omit that production branch. Correct that
bounded integration defect and add real-loader/real-adapter coverage, then
return the complete diff for independent re-review and the remaining targeted
and integrated gates. B19 is not release-ready, the overall remediation is not
complete, and no deployment or external action is authorized.

The Rust web/concurrency/lifecycle/ecosystem/anti-pattern skills influenced the
review by focusing it on non-blocking web execution, permit ownership across
cancellation, explicit cleanup ownership across panic/error paths, active
Cargo-feature reachability rather than lockfile presence alone, and tests that
exercise the real integration rather than a mutable SDK stub.

## Correction re-review

### Findings after correction

No remaining local/source finding was identified.

The High finding is closed semantically, not merely made exception-free:

- The loader still exposes the direct MapLibre 6 ESM namespace. The MapLibre
  adapter no longer reads or writes `accessToken`, and non-Mapbox styles resolve
  to a token-free `MapConfig::MapLibre`. The real Axum search-browser run proved
  map construction through the immutable hashed loader and stable main,
  shared, and worker module graph, with no module failure, page error, or token
  property.
- A `mapbox://` style or an HTTPS style on `api.mapbox.com` now resolves to
  `MapConfig::Mapbox`. The dedicated `MAPBOX_MAP_ACCESS_TOKEN` wins, the
  documented legacy `MAPBOX_ACCESS_TOKEN` fallback remains supported, and
  absence of both fails configuration with
  `MissingEnv("MAPBOX_MAP_ACCESS_TOKEN")`. `PageLayout` and
  `SecurityHeaders` select the matching Mapbox scripts and CSP origins from
  that same variant.
- Actual Chromium coverage loaded the real vendored Mapbox SDK and production
  adapter with `browser-mapbox-test-token`. Every captured API/events request
  carried that exact `access_token`; the test fulfilled those requests locally,
  so it did not contact Mapbox. The production-like CSP allowed the same
  Mapbox origins and produced no CSP violation.
- The complete browser suite retained hard-load, delayed lazy navigation,
  same-page retry, enforced-CSP, failed-tile, worker, and real-SDK construction
  coverage. The real router result also proves executable-module MIME and hash
  routing: an incorrect MIME, loader hash, relative shared-module route, or
  worker route would not have reached successful map construction.

The other reviewed changes remain sound. Image concurrency rejects zero and
out-of-range capacity and keeps an owned permit inside started blocking work,
including after caller cancellation. The migration helper owns only a
validated, uniquely named loopback child database, catches and resumes panics,
awaits pool close, and drops that exact child. Its test applies committed
migrations 1–27 and then current 28–29 without editing history, while preserving
the seeded policy row and proving the new tables. No dependency, lifecycle,
web-boundary, stale-asset, or scope-drift issue was found in the complete diff
from `11cbf00`.

### Fresh independent correction gates

All commands below ran in `/tmp/bikesnest-audit-remediation` with
`DATABASE_URL` unset and
`CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target`. Database-backed
commands used only
`TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit_b19_20260923`;
browser/provider traffic was loopback-local or intercepted locally.

```text
node --check web/static/js/maplibre-loader.mjs
node --check web/static/js/map-provider-maplibre.js
node --check tests/browser/search-app.cjs
node --check tests/browser/navigation.test.cjs
  PASS

node scripts/verify-advisories.test.cjs
  PASS: 7/7

npm ls maplibre-gl --all --offline
  PASS: exactly maplibre-gl@6.11.1

cmp node_modules/maplibre-gl/dist/maplibre-gl.mjs \
  web/static/vendor/maplibre-gl.mjs
cmp node_modules/maplibre-gl/dist/maplibre-gl-shared.mjs \
  web/static/vendor/maplibre-gl-shared.mjs
cmp node_modules/maplibre-gl/dist/maplibre-gl-worker.mjs \
  web/static/vendor/maplibre-gl-worker.mjs
cmp node_modules/maplibre-gl/dist/maplibre-gl.css \
  web/static/vendor/maplibre-gl.css
  PASS: all four vendored files byte-identical to the locked package

cargo test -p bikesnest-infrastructure config::tests --locked
  PASS: 36/36, including Mapbox selection/token precedence/fallback/failure and
  token-free non-Mapbox selection

cargo test -p bikesnest-infrastructure admission_tests --locked
  PASS: 3/3

cargo test -p bikesnest-infrastructure \
  --test migration_upgrade_test --locked
  PASS: 1/1; committed schema 27 upgraded to 29

cargo test -p bikesnest-web --test search_browser_test --locked -- --ignored
  PASS: 1/1; real Axum router and real MapLibre loader/main/shared/worker path

npm run test:browser
  PASS: 28/28; hard/lazy/retry/worker/CSP/MapLibre and locally intercepted
  token-bearing Mapbox coverage

cargo check --workspace --all-targets --all-features --locked
  PASS

cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
  PASS

cargo test --workspace --locked
  PASS: 761 passed, 0 failed, 10 expected ignored
  The independently enumerated suite contained 771 tests. The run included the
  named disposable PostGIS, MinIO S3 round trip, and single-node Valkey tests.

cargo fmt --all -- --check
git diff --check
  PASS
```

Read-only database identity inspection through the named audit container
confirmed database `bikesnest_test_audit_b19_20260923`, role `bikesnest_test`,
and `29|29|t` for migration count, maximum version, and all-successful state.
The host did not have a `psql` binary, so the same read-only queries were run
with the container's client; this was an environment detail, not a review
blocker. Four isolated-race databases dated September 10/14 predated this
review. No child created by the September 23 reviewer runs remained, including
after the panic-cleanup test.

The handoff's six other explicitly ignored browser/renderer targets, pinned
Rust 1.95 Docker build, deterministic generated-asset rebuild, and fresh
RustSec scan were reviewed for internal consistency but were not independently
rerun in this correction round. They remain implementer evidence. In
particular, the local Docker digest is not a final commit-addressed image.

### Reconfirmed RSA exception decision

The initial RSA decision remains unchanged after the complete corrected diff.
Fresh reviewer runs of all six inverse trees—`rsa@0.9.10` and
`sqlx-mysql@0.8.6`, each for current target, `--target all`, and
`--all-features --target all`—again printed `warning: nothing to print`.
`sqlx-macros-core` remains actively needed for the retained derive/migration
stack, while its optional MySQL metadata is what keeps the lock-only
`sqlx-mysql -> rsa` chain. Owner `BikesNest security maintainers`, reviewer
`Audit remediation lead`, review date 2026-09-23, expiry 2026-10-23, rationale,
reachability trigger, and removal criteria remain exact and truthful.

Thus `RUSTSEC-2023-0071` is accepted only as a time-bounded, currently inactive
lockfile artifact. It is not acceptance of reachable RSA code or a patched
release. Expiry, a safe upstream resolution, or any feature change that makes
MySQL/RSA reachable must fail closed and force removal or re-review.

### Release blockers after source PASS

Fresh npm advisory evidence remains unsatisfied. I did **not** run `npm audit`
or contact the registry. Exact local resolution of MapLibre 6.11.1 is not a
substitute for a current scan. With explicit network approval, the
security/release owner must preserve a fresh npm v2 report and exit status and
run the fail-closed validator from the reviewed commit; any current or
unreviewed finding blocks release.

Every external gate listed earlier and in the handoff also remains unavailable:
owner/counsel approval; provider identities, regions, roles, DPAs/transfers and
live restrictions; edge/TLS/proxy/cache/CSP-report and DNS evidence; mail DNS
and delivery evidence; actual production migration/job inventory and old-worker
drain; production-like media/export capacity; backup and approved sandbox
restore plus retention/erasure reconciliation; authenticated staging and
privileged-MFA decision; and a final commit-addressed SBOM, reproducible image
digest, compatible patched rollback image, window, and deployment
authorization. No production release can pass while these remain missing.

## Final verdict

**PASS for the local/source B19 gate only.** The bounded correction resolves
the initial High finding, the corrected exact diff passes the proportionate
independent source, browser, database, dependency-feature, workspace,
formatting, check, and strict-Clippy gates, and no local/source finding remains.

This is not a release-readiness verdict. The fresh npm advisory scan and all
external evidence above remain hard blockers; B19 is not production-ready, the
overall remediation is not complete, and no deployment or external action is
authorized.
