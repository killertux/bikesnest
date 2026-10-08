# B15b independent review

Date: 2026-09-14

Reviewer: independent GPT Sol (`b06a_independent_review`)

Baseline: `69e73b7`
Decision: **PASS**

I read the B15b handoff, plan acceptance criteria and updated architecture,
testing, deployment and publication guidance; inspected the complete frozen
tracked and untracked diff except the root-owned plan ledger; and independently
executed the policy lifecycle/race, auth, privacy, HTTP, real-browser, browser
JavaScript and strict workspace gates. I changed no application source,
migration, policy, plan, seed, publication, provider, deployment or production
state.

## Review disposition

No material findings remain.

- Migration 0028 adds exact terms presentation/acknowledgement evidence without
  treating it as privacy consent, preserves legacy policy rows, rejects
  duplicate schedules before schema mutation, restricts material flags to
  terms, and protects published content/release metadata and proof rows.
  During review I found that its first permitted supersession update could also
  replace a `GENERATED ALWAYS` identity with `id=DEFAULT`. I reproduced the
  bypass independently in a rollback-only disposable-database transaction
  (id 4 became id 5). The forward-only migration 0029 now adds explicit
  `OLD.id`/`NEW.id` comparison. Its focused regression exercises that exact
  `id=DEFAULT` plus otherwise-valid first-supersession case and preserves the
  original id. Migration 0028 was not rewritten after application to the test
  base.
- Release seeding requires exactly privacy, terms and cookies in both locales,
  with one version/effective instant and matching bilingual terms materiality.
  It takes the shared release lock, installs all six documents atomically,
  accepts only a byte-and-metadata-identical replay, rejects partial/changed or
  out-of-order releases, and schedules each predecessor's one immutable
  supersession. Current reads exclude future releases; notices expose an
  unacknowledged current material release and only the nearest upcoming
  material release separately.
- Enabled signup carries the exact displayed terms row, release version and
  locale into the existing account/credential/token/audit/outbox transaction.
  New, active-existing, pending-active-work and pending-repair branches all lock
  the account and then make the same fresh database-time policy decision under
  the release lock before any branch return or repair. A scheduled activation
  therefore either precedes that decision or makes every stale submission
  conflict, with identical response and complete persisted-state snapshots;
  it cannot recover the old account-enumeration signal. Only a genuinely new
  account receives signup proof, so recovery never backfills agreement or
  overwrites credentials.
- Pending notice, presentation and acknowledgement operations use the same
  account-then-release lock order and reject suspended/deleted accounts. Tests
  demonstrate actual lock waiting, activation crossing, competing deletion and
  completion without a lock-order deadlock. Acknowledgement is release-wide
  across locales while retaining the exact localized document shown. Future
  documents are reviewable and can record response preparation but cannot be
  acknowledged before becoming current.
- Presentation timestamp semantics are honest: the record means only that the
  server prepared a response for return, not that a browser received or
  displayed it or that a person read, understood or accepted it. Neither
  presentation nor acknowledgement fabricates `consent_record` data. Current
  and future notices remain recoverable account links and do not lock users out
  of privacy, deletion, security or logout controls.
- The initial enabled missing-proof path returned a bare conflict for a form
  opened before activation. The corrected route now gives the same localized
  stale-form recovery as a wrong old proof: it retains e-mail/display name,
  clears the password, supplies the current exact id/version/link, writes no
  account/token/proof artifact, and supports both boosted and native retry.
- The feature is globally off by default. Missing/blank/invalid explicit flag
  values do not silently enable or disable it, and acknowledgement-enabled
  OAuth is rejected because that flow lacks exact shown-terms evidence. Disabled
  signup has no proof fields or writes; enabled mode fails closed on missing
  current terms or policy storage failure.
- Personal-data export schema 2 includes exact presentation and acknowledgement
  fields. Soft deletion removes both kinds of account-linked proof in the same
  account transaction; foreign-key cascade also protects later shell deletion.
  No historical proof is inferred from account creation, verification, login,
  links or lack of objection.
- The publication runbook clearly withholds production authority and legal
  approval, distinguishes runtime enablement from seed materiality, requires
  six-document/current-future and deployed-fleet verification, and makes the
  limited legacy migration preflight explicit. It now warns that a pre-B15b
  rollback binary ignores the feature flag: after activation registration must
  use an acknowledgement-aware rollback target or be paused/drained until the
  fleet is consistent. It makes no email delivery/read claim and requires
  owner/counsel approval for wording, timing, recipients and publication.

Legacy partial or cross-locale policy inventory is an explicit production
preflight/activation gate, not represented as a runtime guarantee. New releases
can only enter through the coherent six-document seeder. The migration test
reconstructs the relevant pre-0028 schema and exact forward scripts in an owned
child database; it does not claim a complete historical binary/ledger upgrade.
The old disposable database with the earlier draft checksum was preserved and
never used, reset or rewritten.

## Independent commands and results

```text
git diff --name-status 69e73b7
git diff --stat 69e73b7
git status --short
git diff 69e73b7 -- <all non-ledger changed paths>
git diff --check 69e73b7 -- . \
  ':(exclude)docs/plans/2026-09-08-audit-remediation.md'
```

Result: the complete tracked/untracked implementation, migration, templates,
tests, handoff and runbook inventory was inspected; diff validation passed.

Every database-backed Cargo command below removed `DATABASE_URL`, used only
`TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit_b15b_20260914`,
used `CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target`, and used `--locked`.
Database suites were sequential; true race/browser tests created owned child
databases through the repository runner.

```text
cargo test -p bikesnest-infrastructure --test policy_terms_test -- \
  --test-threads=1
```

Result: policy release, migrations, identity, signup/recovery parity, lock race,
notice/proof, export and deletion matrix **12 passed, 0 failed**.

The original identity finding was independently reproduced before correction
with a rollback-only local-container transaction equivalent to:

```text
BEGIN;
INSERT INTO policy_version(kind,locale,version,effective_at,content) ...;
UPDATE policy_version
SET id=DEFAULT, superseded_at='2091-01-01T00:00:00Z'
WHERE version='review-id-bypass'
RETURNING id, superseded_at;
ROLLBACK;
```

Result before 0029: the update succeeded and replaced id 4 with id 5. No state
was retained. After 0029, the exact custom-guard regression in the 12-test
matrix rejects it.

```text
cargo test -p bikesnest-web --test policy_browser_test \
  terms_versions_and_acknowledgements_in_real_browser -- \
  --ignored --exact --test-threads=1 --nocapture
```

Result: actual local Chromium journey **1 passed, 0 failed** in 5.34 seconds.
It covers English htmx-boosted mobile and Portuguese JavaScript-disabled desktop
flows, omitted then stale proof recovery, exact current/future content and
timestamps, current plus future notices, current acknowledgement, future
non-acknowledgeability, locale-switch satisfaction, unrestricted privacy access,
four exact current-release acknowledgements, idempotent presentations, zero
consent records and zero external requests.

```text
cargo test -p bikesnest-infrastructure --test auth_test -- --test-threads=1
cargo test -p bikesnest-infrastructure --test privacy_test -- --test-threads=1
cargo test -p bikesnest-web --test http_test -- --test-threads=1
```

Result: auth **37/37**, privacy **22/22**, and sequential HTTP **181/181**
passed. The HTTP policy matrix includes disabled behavior, omitted/stale/wrong
proof recovery, exact-version kind checks, CSRF rejection with no proof, storage
outage and localized failure status.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --lib \
  policy_workflow_flags_are_explicit_and_fail_closed_on_invalid_values --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --lib \
  terms_acknowledgement_rejects_oauth_bypass --locked
npm run test:browser
```

Result: focused strict configuration checks **2/2** and local browser regression
suite **28/28** passed. Browser tests used loopback only.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo check --workspace --all-targets --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy --workspace --all-targets --locked -- -D warnings
git diff --check 69e73b7 -- . \
  ':(exclude)docs/plans/2026-09-08-audit-remediation.md'
```

Result: formatting, workspace all-target check, strict workspace Clippy and
final non-ledger diff validation passed.

B15b satisfies its local engineering acceptance criteria without conflating
terms evidence with privacy consent or weakening auth, deletion, CSRF or policy
history boundaries. No production policy was seeded, versioned or published;
no provider or notification action occurred. Owner/counsel wording, audience,
timing, deployment, production preflight and any external delivery remain
explicit gates. The green sequential HTTP run does not claim completion of the
B16 parallel-fixture work.
