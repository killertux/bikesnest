# B17 implementation handoff

Baseline: accepted B16b checkpoint `2be17ae`. This is an implementation
handoff for separate review, not self-approval. It changes CI, its small
report validator, and measurement documentation only. It makes no dependency,
lockfile, production-source, generated-asset, Docker, migration, provider, or
deployment change.

## Advisory gate

`ci/advisory-exceptions.json` is the sole exception register.
`scripts/verify-advisories.cjs` consumes a saved npm v2 or cargo-audit JSON
report and fails closed when the report cannot be read/parsed, has an unexpected
shape, the scanner exits without a reported advisory, an advisory is unreviewed,
or a same-ecosystem exception is stale or unused. A malformed entry in either
ecosystem also fails every invocation. Entries are exact advisory IDs and
require `ecosystem`, `owner`, `reviewed_by`, `reviewed_at`, a substantive
`rationale`, and an ISO calendar-date `expires` value strictly after the run
date; duplicate IDs are rejected. No wildcard, ignore flag, or silent
scanner-error path exists.

The `Advisory checks` job first runs the validator’s seven fixture/unit checks,
then installs the official RustSec `cargo-audit` crate at the exact `0.22.2`
release with `--locked` (its published MSRV is Rust 1.88, within CI’s retained
Rust 1.95), and checks both RustSec and npm reports. It does not run an upgrade
or `cargo audit fix`. Network, registry, advisory-database, installation, and
report-format failures remain failures rather than clean findings.

## Independent-review correction

The first independent review found that the original report reader merely
scraped GHSA text and did not prove every npm vulnerability entry resolved to
an advisory; it also omitted RustSec's reported count. The corrected parser now
requires the supported npm v2 plain-object inventory, nonnegative severity
summary whose sum, total, and per-severity entry distribution agree with that inventory, and valid direct or
transitive `via` resolution for every entry. Missing references, cycles, and
non-GHSA/CVE-only advisory objects fail closed. RustSec now requires an exact
nonnegative `count`, matching list length and `found` flag, plain findings, and
exact `RUSTSEC-YYYY-NNNN` IDs. Scanner status remains 0 only for no findings
and 1 only for findings. The seven direct regressions cover valid direct and
transitive npm reports; arrays, summary/inventory mismatches, unknown/missing/
cyclic npm references, mixed reviewed/unreviewed findings, RustSec count/ID/
entry errors, date/config errors, and status disagreement.

Current scans identify the following findings; the reviewed exact-ID entries
allow CI to proceed while the scanners still return their advisory status:

| Ecosystem | Finding | Resolved package | Status |
|---|---|---|---|
| npm | `GHSA-jrc7-96c5-q579` | direct `maplibre-gl` 4.7.1 | critical; npm suggests breaking 6.11.1 |
| RustSec | `RUSTSEC-2026-0258` | `h2` 0.3.27 | patched in 0.4.16 |
| RustSec | `RUSTSEC-2023-0071` | `rsa` 0.9.10 | inactive `sqlx-mysql` lockfile artifact; upstream reports no patch |
| RustSec | `RUSTSEC-2026-0285` | `rustls` 0.23.43 | patched in 0.23.45 |
| RustSec | `RUSTSEC-2026-0104` | `rustls-webpki` 0.101.7 | patched in 0.103.13 |
| RustSec | `RUSTSEC-2026-0098` | `rustls-webpki` 0.101.7 | patched in 0.103.12 |
| RustSec | `RUSTSEC-2026-0099` | `rustls-webpki` 0.101.7 | patched in 0.103.12 |

The current local npm result came from Node 24.16.0 / npm 11.16.0; CI remains
pinned to Node 22. The RustSec result came from cargo-audit 0.22.2 and a fresh
advisory-database update; the local Rust compiler was 1.98.1, not a substitute
for the retained CI Rust 1.95 gate. These are time-bound advisory-service
results, not a claim that the dependency tree is vulnerability-free.

The remediation lead reviewed and accepted the seven narrow entries shown above. They do not
claim safety: each carries its accountable owner/reviewer, fixed expiry, and
concrete removal criterion. The patched RustSec findings and the breaking
MapLibre upgrade remain separately reviewed dependency work rather than changes
smuggled into B17. Every exception is a B19 release blocker: it must be removed
by remediation or explicitly re-reviewed before release/deployment.

`cargo tree -i rsa@0.9.10 --locked` is empty for the current target. The only
lockfile parent is `sqlx-mysql` 0.8.6, while the workspace selects SQLx Tokio,
Rustls, Postgres, migration, and Chrono features. Thus `RUSTSEC-2023-0071` is
currently a lockfile-only inactive MySQL-driver artifact, not a claimed runtime
path. B19 must prune that unused resolution if feasible or upgrade when fixed;
enabling a feature that reaches it requires fresh review.

## Test lanes and timing

The new `DB-free tests` job explicitly targets only domain and application.
Its `--no-run` compile step and subsequent same-runner test-execution step are
labeled separately. `docs/ci-testing.md` documents that CI cache reuse can make
the first phase warm, provides controlled local cold/warm commands using an
empty temporary target directory, and states what those timings do not measure.
The implementation does not change the test runner.

The local B17 measurement used the existing checkout target (therefore neither
a controlled cold measurement nor a CI measurement):

```text
rustc/cargo: 1.98.1
compile phase: 10.71 s elapsed, 688156 KiB peak
immediately-following execution: 0.27 s elapsed, 226568 KiB peak
domain + application: 182 passed, 0 failed
```

## Complete ignored-browser coverage

The existing service-backed test job retains its PostGIS/Valkey/MinIO setup,
frontend asset rebuild/diff/navigation job, and Docker image gate. It now
invokes exactly all six `#[ignore]` declarations with `--locked -- --ignored`:

1. infrastructure email renderer visual test;
2. web contribution browser test;
3. web CSRF browser test;
4. web policy browser test;
5. web profile browser test; and
6. web search browser test.

Every command explicitly uses `env -u BIKESNEST_BROWSER_MUTANT`, while retaining
the existing disposable CI database isolation. All six tests were rerun
sequentially: the DB-free email renderer visual test passed in 4.47 s; the
owner-provided loopback audit database ran contribution in 2.92 s, CSRF in
3.10 s, policy in 3.97 s, profile in 1.96 s, and search in 4.05 s. Each
service-backed test used the explicit disposable `TEST_DATABASE_URL`, left
`DATABASE_URL` unset, and created/removed only its harness-owned scope.

## Validation and limits

```text
node scripts/verify-advisories.test.cjs
  7 passed
cargo-audit audit --no-fetch --stale --json (cached DB) + validator
  cargo-audit exit 1 with 6 findings; validator PASS against exact exceptions
npm audit --json + validator
  npm exit 1 with 1 finding; validator PASS against exact exception
cargo test -p bikesnest-domain -p bikesnest-application --no-run --locked
  PASS (10.71 s local existing-target compile phase)
cargo test -p bikesnest-domain -p bikesnest-application --locked
  182 passed, 0 failed (0.27 s immediately following execution)
PLAYWRIGHT_BROWSERS_PATH=/tmp/b17-playwright \
  env -u BIKESNEST_BROWSER_MUTANT cargo test -p bikesnest-infrastructure \
  --test email_renderer_visual_test --locked -- --ignored
  1 passed (4.47 s; required an unsandboxed Chromium launch)
env -u DATABASE_URL -u BIKESNEST_BROWSER_MUTANT \
  TEST_DATABASE_URL=postgres://…@127.0.0.1:55439/bikesnest_test_audit_b15b_20260914 \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  PLAYWRIGHT_BROWSERS_PATH=/tmp/b17-playwright cargo test -p bikesnest-web \
  --test <each explicit browser test> --locked -- --ignored
  contribution 1 passed (2.92 s); CSRF 1 passed (3.10 s)
  policy 1 passed (3.97 s); profile 1 passed (1.96 s); search 1 passed (4.05 s)
cargo fmt --all -- --check
git diff --check
  PASS
```

`npm audit --json` required authorized registry access locally and returned the
one finding above. `cargo-audit audit --json` required a writable temporary
`CARGO_HOME` in this sandbox and authorized advisory-database access; it
returned the six RustSec findings above. Before the remediation-lead decision,
both validator invocations failed as expected against the empty register; after
the exact reviewed entries were recorded, both passed while the scanners still
returned nonzero for their live findings. No dependency upgrade, real provider
action, database action, or deployment occurred.
