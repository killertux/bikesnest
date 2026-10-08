# B17 independent review

Date: 2026-09-23

Reviewer: independent GPT Sol (`b17_independent_review`)

Baseline: accepted B16b documentation checkpoint `2be17ae`

Round-one decision: **CHANGES REQUESTED**

Final decision after frozen correction review: **PASS**

## Material findings

1. **High — the advisory validator accepts incomplete/malformed report bodies
   and can silently omit an unreviewed npm advisory.**
   `scripts/verify-advisories.cjs:97-102` accepts any truthy JavaScript object as
   `vulnerabilities` (including an array), then recursively scrapes only GHSA
   strings without proving that every reported vulnerability resolves to an
   extracted advisory ID. `scripts/verify-advisories.cjs:120-132` derives both
   the expected scanner status and the reviewed/unreviewed set solely from that
   incomplete extracted set. Consequently, a report containing the currently
   reviewed GHSA plus a second vulnerability represented only by a CVE URL,
   while `metadata.vulnerabilities.total` is 2, passes with scanner exit 1 and
   only the GHSA exception. A malformed npm report with
   `vulnerabilities: []` is also accepted as a clean report. The RustSec parser
   has the same report-integrity gap at
   `scripts/verify-advisories.cjs:105-117`: it ignores the scanner's `count`, so
   `found: true`, `count: 99`, and a one-item list is accepted. The seven tests
   in `scripts/verify-advisories.test.cjs:26-83` do not exercise these cases.

   Independent reproduction against the frozen source produced:

   ```text
   npm vulnerabilities array: ACCEPTED []
   npm hidden second CVE / count mismatch: ACCEPTED ["GHSA-JRC7-96C5-Q579"]
   RustSec count mismatch: ACCEPTED ["RUSTSEC-2026-0001"]
   ```

   This violates B17's central fail-closed requirement for malformed reports
   and unreviewed/new findings. The validator must validate the supported npm
   v2 and cargo-audit report invariants, reject unsupported or unresolved
   vulnerability entries, and cross-check the scanners' summary/count fields
   before exceptions can authorize the result. Regression tests must cover the
   malformed array, inconsistent totals/counts, unresolved/unknown npm
   advisory representation, and mixed reviewed-plus-unreviewed report. I did
   not alter the implementation.

No other material finding was identified in round one. This finding blocked
initial acceptance even though the current real reports happened to match the
parser's expected happy path.

## Scope and acceptance observations

- The reviewed worktree was on `fix/audit-remediation` at `2be17ae`; all B17
  work was unstaged. The changed inventory was limited to
  `.github/workflows/ci.yml`, `ci/advisory-exceptions.json`,
  `scripts/verify-advisories.cjs`, `scripts/verify-advisories.test.cjs`,
  `docs/ci-testing.md`, the B17 handoff, and the remediation ledger. This review
  adds only this permitted review record. Cargo manifests, Rust production and
  test source, migrations, package manifests/locks, Docker inputs, templates,
  and generated assets are unchanged from the baseline. `Cargo.lock` and
  `package-lock.json` hashes exactly matched their `2be17ae` versions.
- The exception register has exactly seven entries: one npm GHSA and the six
  current RustSec IDs. Every entry has a nonempty owner and reviewer, a
  same-day review date, a future fixed expiry, a narrow rationale, and a stated
  upgrade/prune/re-review removal condition. There is no wildcard or scanner
  ignore flag. Same-ecosystem unused, expired, future-reviewed, duplicate and
  malformed configured entries are rejected; all configured entries are
  validated on either ecosystem invocation. The report-integrity defect above,
  rather than the current exception content, is the blocker.
- A fresh npm v2 report returned exit 1 with one critical direct
  `maplibre-gl` finding, `GHSA-jrc7-96c5-q579`, and a semver-major suggested
  fix to 6.11.1. A fresh RustSec database update with cargo-audit 0.22.2
  returned exit 1 with exactly `RUSTSEC-2026-0258`, `RUSTSEC-2023-0071`,
  `RUSTSEC-2026-0285`, `RUSTSEC-2026-0104`, `RUSTSEC-2026-0098`, and
  `RUSTSEC-2026-0099`; the validator accepted both current real reports against
  the register. Those are time-bound scan results, not a safety claim.
- `cargo tree -i h2@0.3.27 --locked` and
  `cargo tree -i rustls-webpki@0.101.7 --locked` confirm the documented legacy
  AWS HTTP/TLS paths. `rustls@0.23.43` is reachable through AWS, reqwest,
  lettre, and SQLx. `cargo tree -i rsa@0.9.10 --locked` prints no active current
  target path, and the only `Cargo.lock` package that names `rsa` is
  `sqlx-mysql` 0.8.6; the workspace selects SQLx Postgres rather than MySQL.
  The handoff therefore describes reachability without treating a reachable
  advisory as safe.
- The workflow retains Rust 1.95 and Node 22. The published
  cargo-audit 0.22.2 manifest declares Rust 1.88, and the validator/tests also
  pass under available Node 20.20.2, below the pinned Node 22 runtime. The
  scanner shell blocks preserve the nonzero exit, restore `set -e`, and hand
  the exact status plus saved JSON to the validator; unreadable, invalid JSON,
  explicit npm error output, and scanner statuses other than the expected 0/1
  fail. The internal report-shape checks remain insufficient as found above.
- The DB-free job explicitly compiles and then executes only
  `bikesnest-domain` and `bikesnest-application` on the same runner. The labels
  and `docs/ci-testing.md` distinguish possibly warm CI compilation, following
  warm execution, and a controlled empty-target local measurement, without
  presenting any of them as request, production, or cross-runner performance.
- Repository inspection finds exactly six actual `#[ignore = ...]`
  declarations: the infrastructure email renderer and the contribution, CSRF,
  policy, profile, and search browser tests. The service-backed job invokes
  each named target explicitly with `--locked -- --ignored` and
  `env -u BIKESNEST_BROWSER_MUTANT`. Its existing PostGIS, Valkey, MinIO,
  workspace-test setup is preserved. The frontend asset rebuild/diff and
  navigation job and Docker image gate are unchanged apart from surrounding
  line movement.

## Independent validation

Commands were run from `/tmp/bikesnest-audit-remediation`; no provider,
database, deployment, production, migration, generated-asset, dependency or
lockfile mutation was performed.

```text
node scripts/verify-advisories.test.cjs
node --check scripts/verify-advisories.cjs
node --check scripts/verify-advisories.test.cjs
```

Result on Node 24.16.0: **7 passed, 0 failed**; both syntax checks passed. The
same test and syntax commands under available Node 20.20.2 also passed, which
supports Node 22 compatibility.

```text
node -e '<three direct malformed/incomplete report cases using exported parser/verify functions>'
```

Result: all three invalid reports were accepted exactly as quoted in finding
1. This was read-only execution and created no fixture file.

```text
npm audit --json > /tmp/b17-review-npm.json
node scripts/verify-advisories.cjs --ecosystem npm \
  --report /tmp/b17-review-npm.json --scanner-exit 1
```

The first sandboxed attempt could not resolve `registry.npmjs.org`; its npm
error JSON and exit 1 were rejected by the validator. The authorized network
rerun returned the one critical MapLibre finding above with exit 1, and the
validator passed it against the exact current exception.

```text
CARGO_HOME=/tmp/bikenest-cargo-audit-home \
  /tmp/bikenest-cargo-audit/bin/cargo-audit audit --json \
  > /tmp/b17-review-cargo-audit-fresh.json
node scripts/verify-advisories.cjs --ecosystem rustsec \
  --report /tmp/b17-review-cargo-audit-fresh.json --scanner-exit 1
```

The binary reported cargo-audit 0.22.2. An authorized fresh advisory-database
update completed, the scan returned the six IDs above with exit 1 and no
warning categories, and the validator passed it against the exact current
exceptions. A preceding cached `--no-fetch --stale` scan returned the same six.

```text
cargo test -p bikesnest-domain -p bikesnest-application --no-run --locked
cargo test -p bikesnest-domain -p bikesnest-application --locked
```

Result with local Rust/Cargo 1.98.1 and existing warm target artifacts: compile
completed; execution **182 passed, 0 failed, 0 ignored**.

```text
cargo fmt --all -- --check
git diff --check
node --check scripts/verify-advisories.cjs
node --check scripts/verify-advisories.test.cjs
```

Result: all passed. A separate no-index whitespace check covered the new
validator/register files.

```text
git diff --check 2be17ae
git diff --check --no-index /dev/null <each untracked B17 file>
git status --short
git diff --name-only 2be17ae
git ls-files --others --exclude-standard
git -C /home/bruno/Projects/bikenest rev-parse HEAD
git -C /home/bruno/Projects/bikenest branch --show-current
git -C /home/bruno/Projects/bikenest status --short
```

Result: tracked and every untracked B17 file, including this review, passed the
whitespace check. The final path inventory contains only the seven B17 paths
listed above plus this review. The live checkout remained on `main` at
`7aa243c3d23085cc330a77e0297de5c4f9f79ef6`, with the same pre-existing
`?? docs/reviews/` status observed before review; no live-checkout file was
changed.

The six ignored renderer/browser tests were not rerun during this review. I
inspected all six declarations, their explicit workflow commands, and the
handoff's sequential pass evidence (email renderer 1/1; contribution 1/1; CSRF
1/1; policy 1/1; profile 1/1; search 1/1). The task explicitly permitted use of
that existing evidence. This review therefore does not independently claim
fresh browser timings or a fresh Rust 1.95 GitHub runner execution.

## Round-two correction review

The original High finding is resolved. The frozen correction makes the npm
reader require a plain v2 vulnerability inventory and a complete nonnegative
severity summary (`scripts/verify-advisories.cjs:99-114`), checks the inventory's
per-severity distribution (`scripts/verify-advisories.cjs:115-122`), and
resolves every direct or transitive `via` path to an exact GitHub GHSA while
rejecting missing references and cycles (`scripts/verify-advisories.cjs:123-151`).
The RustSec reader now requires a nonnegative count equal to list length, plain
finding/advisory objects, and exact `RUSTSEC-YYYY-NNNN` identifiers
(`scripts/verify-advisories.cjs:154-168`). The exception and scanner-status
checks remain fail closed.

I reran the exact three round-one reproductions without weakening their input.
All now reject:

```text
npm vulnerabilities array: REJECTED npm audit did not return an auditReportVersion 2 vulnerabilities report
npm hidden second CVE / count mismatch: REJECTED npm audit metadata.vulnerabilities must contain non-negative severity counts and total
RustSec count mismatch: REJECTED cargo audit did not return a RustSec vulnerabilities report
```

I then independently exercised the corrected supported shape rather than
relying only on those early-rejection cases. Valid direct and transitive npm
reports passed. The severity-sum/total mismatch, total/inventory mismatch,
per-severity inventory mismatch, CVE/unknown advisory object, missing
transitive reference, reference cycle, and mixed reviewed/unreviewed GHSA each
rejected with the intended distinct error. A null RustSec finding, count/list
mismatch, and non-RustSec ID rejected. Scanner status 0 with a finding, an
expired exception, a future review date, and an unused exception also rejected.
The new seven-test file covers the same cases at
`scripts/verify-advisories.test.cjs:11-47`; it passed **7/7** on Node 24.16.0
and again on Node 20.20.2. Both validator files passed syntax checks on both
runtimes.

The corrected reader remains compatible with real scanner output:

- Saved `/tmp/b17-npm-audit.json` passed with npm v2 total 1 and the exact
  MapLibre GHSA; saved `/tmp/b17-cargo-audit.json` passed with count 6 and the
  exact six registered RustSec IDs.
- Authorized fresh `npm audit --json` returned exit 1, total 1, critical 1,
  package `maplibre-gl`; the validator passed it against the exact exception.
- Authorized fresh cargo-audit 0.22.2 updated/checked RustSec commit
  `6477ec04375b913e13f38d966dc49eba9d178cb8`, returned exit 1 with the same six
  IDs and no warning categories; the validator passed it against the exact six
  exceptions.

Final correction commands also passed:

```text
node scripts/verify-advisories.test.cjs
node --check scripts/verify-advisories.cjs
node --check scripts/verify-advisories.test.cjs
/tmp/node20/bin/node scripts/verify-advisories.test.cjs
/tmp/node20/bin/node --check scripts/verify-advisories.cjs
/tmp/node20/bin/node --check scripts/verify-advisories.test.cjs
cargo fmt --all -- --check
git diff --check 2be17ae
```

The path inventory remains limited to the original B17 workflow, register,
validator/tests, CI documentation, handoff and ledger plus this review. Cargo
and npm lockfile hashes still exactly match `2be17ae`; there is no dependency,
production source, migration, generated asset, Docker, runner, provider or
deployment change. The live main checkout was not touched. Because the frozen
correction changes only the report validator, its tests and truthful review
documents, the previously passed DB-free and six explicit ignored-browser
evidence remains applicable and was not rerun in round two.

## Disposition

B17 **PASSES** independent correction review. The report parser now rejects the
original fail-open shapes and the broader malformed/inconsistent graph and
summary matrix while continuing to accept both supported real scanner schemas
and exactly the seven current reviewed findings. The current advisory records,
reachability explanations, CI lane coverage, measurement wording and scope
controls remain acceptable. B19 must still remediate or explicitly re-review
every advisory exception before release/deployment; this PASS does not claim
that any listed vulnerability is safe or fixed.

This is source/CI review only. Nothing was deployed, published, seeded, sent,
or changed in production or provider state.
