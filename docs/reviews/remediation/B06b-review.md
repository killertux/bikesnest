# B06b independent review

Date: 2026-09-10  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `5728a46`  
Decision: **PASS** (after correction re-review)

I reviewed the complete stable B06b diff and `docs/reviews/remediation/B06b-handoff.md`, with particular attention to `crates/infrastructure/src/auth/outbox.rs`. I also read the current `AGENTS.md`, `ARCHITECTURE.md`, `TESTING.md`, remediation plan/dependencies, ENG-06 audit evidence, and applicable Rust web/concurrency guidance. I did not modify application source, contact a provider, or perform a production/deployment action.

## Material findings

1. **Resend can permanently dead-letter a documented retryable 409 when the response has no `Content-Length`.** In `ResendEmailProvider::send_idempotent`, the response body is parsed only when `status == 409` and `content_length()` is `Some(n <= 1024)`. A valid bounded chunked/HTTP response with no declared length therefore leaves `name=None`; `retryable_response(409, None)` returns false and the job is immediately classified permanent. This violates the required distinction for Resend's `concurrent_idempotent_requests` response and can strand auth mail. The pure parser test explicitly treats `content_length=None` as unparseable and does not exercise the response path. Read a bounded body even when declared length is absent (with a hard streaming/read limit), or conservatively treat an unclassifiable 409 as retryable; add response-level tests for both documented 409 names with present, absent, oversized, malformed and hostile bodies, proving bounded reads and secret-free errors.

2. **The promised registration-recovery state matrix and final-audit rollback are not regression-tested.** `registration_retry_recovers_or_repairs_without_overwriting_account` proves recovery of an ordinary pending job and repair of one exhausted pending job. It does not exercise an expired credential/job, a missing job, a succeeded/failed inactive job, or a running job with an active lease, although these are explicit B06b acceptance/handoff cases and use materially different predicates in `recover_registration`. The job suite's active-lease test covers only inline dispatch after admission, not registration recovery. In addition, registration rollback is injected at the outbox insert, before the audit write; no B06b test makes the final `auth.register` or `auth.email_change_requested` audit insert fail after token and job insertion and proves the entire aggregate rolls back. Add fresh transaction-scoped cases for every recovery state, snapshotting password/display name/locale and exact old/new token/job rows, and inject an audit-write failure for each audit-bearing outbox transition to prove token/job/account changes cannot commit before the audit.

## Confirmed behavior

- New-account registration performs user, password identity, role, verification token, canonical mail admission and registration audit writes on one acquired connection/transaction. The existing injected background-job failure rolls all preceding registration rows back.
- Verification resend, password-reset request and email-change request share `enqueue_mail_on` inside their token transaction. The admission-failure test confirms token/job rollback for all three and audit rollback for the email-change path when admission itself fails.
- Registration no longer has the prior find-then-create/DUMMY_HASH race: hashing occurs before the authoritative insert, and conflict recovery locks the existing pending account without overwriting its credential, display name or stored locale.
- Exhausted inactive verification work is terminalized and redacted in the same transaction that inserts a replacement token/job. The source predicate preserves actively leased exhausted work and avoids duplicate admission; this still needs the direct recovery regressions in finding 2.
- Durable dispatch is post-commit and side-effect-free. Inline dispatch exact-claims only the admitted job, uses a unique owner, persists retry backoff, respects an active owner, immediately dead-letters permanent provider rejection, and scrubs terminal payloads. Broader worker supervision/fencing remains correctly assigned to B07.
- Provider I/O remains outside auth transactions. `SendEmailHandler` passes a stable per-message admission-derived key to providers; SMTP deliberately falls back to at-least-once `send`. Resend request construction uses the supplied key, and provider/decode errors exposed to job persistence remain bounded and secret-free.
- No schema migration was added. The existing B06a account/token/purpose/expiry lifecycle and deletion/send lock boundary are reused rather than duplicated. B14 remains responsible for templates/security notifications.

## Commands and results

All DB-backed commands explicitly unset `DATABASE_URL`, used only the handoff's dedicated loopback audit database, ran sequentially, and reused debug artifacts.

```text
git status --short
git diff --name-only 5728a46
git diff --stat 5728a46
git diff --check 5728a46
git diff 5728a46 -- <all changed paths>
```

Result: 18 changed paths plus the new outbox/handoff were inspected; diff check passed.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test auth_test --locked -- --test-threads=1
```

Result: **27 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test job_test --locked -- --test-threads=1
```

Result: **17 passed, 0 failed**.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure email::resend::tests --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-application --test auth_test --locked
```

Result: Resend **5 passed**; application auth **30 passed**.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web \
  --all-targets --locked -- -D warnings
git diff --check 5728a46
```

Result: formatting, strict Clippy and diff checks passed.

The implementer-reported sequential HTTP suite passed 172/172; I did not repeat that broad unchanged suite. The initial passing tests did not override the provider classification defect or explicit missing recovery/rollback evidence, so both findings were returned for correction.

## Correction re-review

I read `docs/reviews/remediation/B06b-corrections.md`, inspected the corrected diff against `5728a46`, and independently resolved both findings:

1. Resend now reads 409 response bodies incrementally with a hard 1 KiB ceiling whether or not `Content-Length` is present. Only the allowlisted `invalid_idempotent_request` name is permanent; `concurrent_idempotent_requests` and unknown, malformed, unreadable or oversized 409s are conservatively retryable. The response text is never returned in `EmailError`. A local ephemeral HTTP server exercises the real `reqwest::Response` path for both known names with present/absent length and for oversized, malformed and non-UTF-8 bodies.
2. Fresh scoped database tests now cover expired, missing, succeeded, failed, active-running and active-running-at-exhausted-budget registration work. Each snapshots the original account id, display name, locale, credential, token and old job. Active leases return the exact old mail without mutation or a new token; inactive/missing/expired states create a distinct token/job while preserving account credentials/profile. Separate triggers fail the final registration and email-change audit insert after mail admission and prove account/token/job/audit rollback.

Correction commands and results:

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure email::resend::tests --locked
```

Result: **5 passed, 0 failed**, including the response-level bounded 409 matrix. The server was an ephemeral loopback test stub; no provider was contacted.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test auth_test --locked -- --test-threads=1
```

Result: **35 passed, 0 failed**, including all recovery snapshots and both final-audit rollback cases.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web \
  --all-targets --locked -- -D warnings
git diff --check 5728a46
```

Result: formatting, strict Clippy and diff checks passed. The independently run pre-correction job suite remained source-stable across these bounded corrections and passed **17/17**.

No material B06b finding remains. This PASS is source acceptance only: it does not claim exactly-once external delivery, authorize deployment/provider calls, absorb B07's wider worker supervision/fencing work, or absorb B14's templates and security notifications.
