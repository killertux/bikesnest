# B06a independent review

Date: 2026-09-10  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `a3e6a2b`  
Decision: **PASS** (after correction re-review)

This review covers the complete uncommitted B06a diff, not only the implementation handoff. I read `AGENTS.md`, `ARCHITECTURE.md`, `TESTING.md`, the remediation plan, the originating SEC-02/SEC-06 findings, and the Rust web/concurrency review guidance. I did not edit application source, run providers, or perform deployment/production actions.

## Material findings

1. **Sensitive tracing is not closed against a hostile recipient domain.** `EmailMessage`'s custom `Debug` implementation still emits `recipient_domain`, and the durable queue's admission-error event explicitly records `recipient_domain` (`crates/application/src/email.rs:117-124`, `crates/infrastructure/src/email/queue.rs:75-83`). That value comes from the message address and is not an allowlisted diagnostic code. A synthetic marker placed after `@` therefore reaches debug output and, when queue admission fails, captured tracing. The new debug test puts its marker only in the local part and the dead-letter test checks a normal `example.com` domain, so both pass while missing this path. This does not satisfy the handoff requirement that synthetic email markers be absent from captured tracing. Remove recipient-derived data from these diagnostics (or establish and test an explicit bounded allowlist that cannot echo input) and add a captured admission-failure test with the marker in the domain, plus a direct hostile-domain debug assertion.

2. **The required lifecycle matrix is incomplete and one negative assertion is confounded.** The handler has positive coverage for verify/reset/change, but inline delivery has only one positive verify case and no negative case. There are no positive/negative inline cases for reset or change. Further, the alleged suspended-account rejection for `change` runs after both email-verification tokens were expired; it can pass on expiry without exercising the account-state predicate. Reset is rejected only as used, and verify only as recipient-mismatched/expired; the matrix does not independently demonstrate the state, exact-token, recipient, used and expiry gates for every purpose. Add unconfounded positive and negative cases for each purpose through both handler and inline paths, resetting fixtures between predicates so each asserted reason is independently observable. Queue admission should likewise have explicit purpose coverage where its SQL branches differ.

3. **Touched deployment documentation overstates guarantees that B06b explicitly still owns.** `docs/deployment.md` still says queueing is a single insert that cannot fail registration after account creation and labels token-key deduplication “No double sends.” The current auth transition and enqueue are not atomic, and a lease expiry/ambiguous provider outcome can duplicate delivery (the handler's own module documentation acknowledges this). The new deletion-boundary paragraph does not cure those categorical statements. Reword this section to accurately describe current non-atomic enqueue and at-least-once delivery limits, leaving the transactional outbox/retry/idempotency improvement to B06b.

## Confirmed behavior

- Migration 0026 fails legacy pending/running mail closed, scrubs every legacy mail payload, preserves pre-existing terminal states, clears leases, and leaves unrelated jobs unchanged. The isolated upgrade test exercises the migration SQL on a fresh uniquely-owned database.
- Terminal success/failure persistence scrubs mail payloads and records redaction timestamps; unrelated one-shot/recurring jobs retain their payload behavior.
- Account anonymization redacts pending, running, succeeded and failed linked mail and cancels active states. Expiry retention redacts expired active reset mail without touching unexpired mail.
- The send/deletion race holds the account row through provider acceptance and independently observes deletion waiting on a PostgreSQL lock. After deletion commits, a stale preclaimed payload is rejected. The enqueue/deletion race independently observes enqueue waiting and proves deletion-first cannot persist a new mail payload.
- Token extraction hashes the URL-decoded token bytes, matching the canonical token-table hash rather than hashing the encoded string. Provider bodies, decode errors and terminal errors are bounded in the exercised paths.
- The implementation documents the provider-acceptance boundary, timeout ambiguity, disabled-worker/outage delay, maintenance cadence, migration quiescence, restoration risk and backup aging. These are appropriately stated as limits rather than distributed atomicity guarantees, subject to finding 3's older contradictory text.
- New sequential mail tests use transaction-scoped `Db`; real upgrade and concurrency checks use uniquely-owned disposable databases. I found no new committed shared-fixture cleanup in the B06a additions.

## Commands and results

All database commands explicitly unset `DATABASE_URL`, used only the handoff's loopback audit target, and reused the debug target directory.

```text
git status --short
git rev-parse --short HEAD
git diff --stat a3e6a2b
git diff --check a3e6a2b
git diff --find-renames a3e6a2b -- <all changed paths>
```

Result: baseline `a3e6a2b`; 21 changed implementation/documentation paths plus the handoff and migration were inspected. `git diff --check` passed. The lead's concurrent plan-only status update was identified separately and did not alter application review scope.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test job_test --locked
```

The first sandboxed attempt was blocked before connection (`Operation not permitted`), so it was rerun with approved loopback database access. Result: **12 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test privacy_test --locked
```

Result: **21 passed, 0 failed**, including migration upgrade and both real lock-wait race tests.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web \
  --all-targets --locked -- -D warnings
```

Result: both passed.

The initial passing suites established substantial lifecycle and race behavior, but did not override the missing/false acceptance evidence above. Those findings were returned to implementation and resolved as recorded below.

## Correction re-review

I read `docs/reviews/remediation/B06a-corrections.md`, inspected the complete corrected diff against `a3e6a2b`, and rechecked each finding above. No material B06a finding remains:

1. `EmailMessage` debug output no longer includes any recipient-derived field. Queue-admission failures now emit only the allowlisted mail-purpose code and a bounded reason (`invalid_credential`, `database_unavailable`, or `queue_error`), never the raw repository/sqlx error. A hostile marker after `@` is tested both through direct debug formatting and captured tracing from a real closed-pool admission failure.
2. Fresh independent fixtures now cover verify, reset and change. Each purpose has positive and independent account-state, token-hash, recipient, used-token and expiry cases through both `SendEmailHandler` and `InlineEmailQueue`; the durable admission path independently covers the same SQL branches and gates. Providers remain untouched on every rejected delivery.
3. Application, queue, wiring and deployment prose now states that auth transition and durable enqueue are separate transactions, enqueue failure can leave an already-created account/token without a job, idempotency only deduplicates admission, and lease/provider ambiguity can duplicate at-least-once delivery. B06b ownership is explicit and no exactly-once claim remains.

Correction validation used the same explicit loopback audit database and never sourced `.env`:

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test job_test --locked
```

Result: **14 passed, 0 failed**, including the full delivery/admission matrix and hostile-domain tracing regression.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test privacy_test --locked
```

Result: **21 passed, 0 failed**, reconfirming upgrade preservation, exact redaction persistence, expiry behavior, and both observed PostgreSQL lock-wait races.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-application email::tests --locked
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web \
  --all-targets --locked -- -D warnings
git diff --check a3e6a2b
```

Result: application email tests **4 passed**; formatting, strict Clippy and diff checks passed.

Lead validation additionally ran the corrected full HTTP suite: **171 passed, 1 failed**. The failure was the previously tracked B16 transaction-isolation flake `review_create_updates_aggregate` (`review_location_id_fkey`, expected 303 but got 200), not a mail path or a regression introduced by these corrections. This PASS is limited to B06a source acceptance; it does not claim that known B16 issue fixed, authorize deployment, or satisfy B06b's atomic-outbox/external-delivery semantics.
An isolated rerun of that test passed **1/1**, which characterizes the failure as intermittent but does not close the B16 work.
