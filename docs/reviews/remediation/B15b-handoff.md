# B15b implementation handoff

Status: implementation complete pending independent review. No policy was
published, no seed command was run against a deployed database, and no email or
provider action occurred. The acknowledgement workflow defaults off.

## Outcome

- Added forward migration `0028_terms_acknowledgement.sql`: exact policy-row
  presentation and acknowledgement evidence, personal-data foreign keys,
  immutable policy/proof rows, release-schedule uniqueness, and a fail-closed
  duplicate-schedule preflight.
- Added forward migration `0029_policy_version_id_immutable.sql` after review
  reproduced an ID-change loophole in 0028's otherwise-valid first
  supersession. The replacement trigger compares `NEW.id` with `OLD.id`;
  applied migration 0028 was not edited.
- Policy selection now distinguishes currently effective documents from
  scheduled documents. Publication is one atomic six-document bilingual
  release; partial/incoherent releases and changed replays are rejected.
- Added `TermsAcknowledgementStore` and kept it distinct from privacy consent.
  It returns both an unacknowledged current material release and the nearest
  upcoming material release. Future terms are reviewable but not
  acknowledgeable.
- Enabled signup records the exact shown terms row, version, locale and DB
  timestamp in the existing account/token/outbox/audit transaction. A release
  advisory lock and fresh post-lock DB timestamp make a concurrent activation
  either wholly precede or reject signup. Existing-email recovery never
  fabricates historical agreement or overwrites the existing credential.
  New, pending, and active email paths all validate the same submitted policy
  under the account-then-policy lock order before returning or repairing work,
  so concurrent activation cannot become an account-enumeration oracle.
- Existing users get persistent account notices and exact-version review/ack
  routes without service lockout. Presentation means only that the server
  prepared the response for return; it is not delivery, display, reading, or
  consent evidence. A bilingual release is satisfied once while retaining the
  exact localized row shown.
- Proof operations lock the account before the policy release, reject
  suspended/deleted accounts, and serialize with anonymization. Soft deletion
  explicitly removes proof rows; exports include the remaining proof fields in
  schema version 2.
- Added `POLICY_ACKNOWLEDGEMENT_ENABLED=false` and
  `POLICY_TERMS_MATERIAL_NOTICE=false`. Explicit invalid/blank values fail
  configuration rather than silently disabling the workflow. Enabling terms
  acknowledgement together with OAuth is rejected because the OAuth callback
  has no exact shown-terms handoff.
- Added bilingual notice/stale-flow copy, exact version links, HTTP cache/CSRF
  behavior through the existing router, architecture/testing/deployment docs,
  and the owner-controlled publication runbook.

## Validation

Commands use the explicitly owned loopback test database and unset
`DATABASE_URL`.

- `cargo check --workspace --tests --locked` — passed.
- `cargo test -p bikesnest-domain -p bikesnest-application -p bikesnest-infrastructure --lib --locked`
  — domain 71 passed, application 12 passed; the unprivileged infrastructure
  run had 99 passes and three expected loopback-bind permission failures.
- Escalated `cargo test -p bikesnest-infrastructure --lib --locked` — 102/102
  passed, including the local Valkey/Resend stubs and strict new flag parsing.
- `cargo test -p bikesnest-infrastructure --test policy_terms_test --locked`
  — 12/12 passed. These tests own fresh child databases and cover coherent
  release/replay/rollback, immutable rows, current plus nearest future,
  wrong-kind/version/future rejection, bilingual satisfaction, idempotent
  presentation, export/anonymization, deletion races, signup/release lock race,
  no recovery backfill, stale-policy parity across new/pending/active addresses,
  active-work and missing-work pending recovery, exact pre/post
  account/identity/token/job/audit/proof snapshots, pending-recovery activation,
  and direct lock-order evidence for acknowledgement. The same release test
  executes the exact forward 0029 SQL and rejects `id=DEFAULT` changing a
  published row ID during its first otherwise-valid supersession through the
  custom immutable-policy guard, while exact replay and ordinary supersession
  stay green.
- `cargo test -p bikesnest-infrastructure --test privacy_test --locked` — 22/22
  passed on the fresh B15b base. The earlier combined privacy/policy run was
  31/31 before the final three policy concurrency regressions were added.
- `cargo test -p bikesnest-infrastructure --test auth_test --locked` — 37/37
  passed.
- Focused HTTP policy gate/outage/CSRF matrix — 1/1 passed: disabled signup has
  no proof fields; submitting that pre-activation form after activation returns
  a localized recoverable 409 with email/display name retained, password
  cleared, and exact current proof fields without creating any account, proof,
  or mail job. Missing and wrong authenticated CSRF are 403 with no stored
  acknowledgement; stale/wrong version is 409; wrong-kind exact URL is 404;
  policy storage outage is localized 503.
- Full sequential HTTP suite — 181/181 passed.
- Web library tests — 69/69 passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — passed.
- Local browser JavaScript suite — 28/28 passed after granting its loopback
  listener permission. The first sandboxed attempt passed the adapter file but
  the navigation harness could not bind `127.0.0.1` (`EPERM`); this was an
  environment restriction, and the escalated rerun was fully green.
- Lead-owned actual browser acceptance — 1/1 passed (4.93 seconds): English
  boosted mobile and Portuguese native desktop pre-activation omitted-proof
  recovery, signup, stale form recovery with retained inputs,
  exact current/future documents, both account notices, current ack, future
  non-acknowledgeability, locale-switch satisfaction, privacy access, no
  external requests, four exact acknowledgement rows, idempotent presentation,
  and zero fabricated consent rows.

The migration check reconstructs the pre-0028 schema inside a newly migrated
owned child database, inserts legacy rows, then executes the exact 0028 SQL. It
proves legacy-row preservation and duplicate-preflight failure before schema
mutation; it is not a full historical binary/SQLx-ledger upgrade chain. The
older disposable base retained an earlier draft checksum and correctly raised
`VersionMismatch(28)` after the migration changed during implementation; it
was preserved. Final DB gates use the separately created
`bikesnest_test_audit_b15b_20260914` base. Migration 0028 was frozen before it
was applied there.

## Activation and remaining external gates

Follow `docs/policy-publication.md`. All six approved documents must share one
version/effective instant and the terms material flag must agree across locales.
Every release requires owner/counsel approval. A material activation additionally
requires a fleet-consistent acknowledgement-aware deployment; mixed or old
binaries can create unproved signups, and those gaps must never be backfilled.

No email notification exists in this batch. In-product presentation does not
fulfil the policy draft's advance-email statement, does not reach users who do
not visit, and is not receipt/read proof. Wording, recipients, timing,
publication, external delivery, production migration preflight, and rollback
approval remain owner/counsel/B19 gates. The browser harness is explicit opt-in
and CI lane adoption remains future work.
