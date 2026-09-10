# B06b implementation handoff

Baseline: `5728a46` (accepted B06a). This worktree is intentionally uncommitted.

## Outcome

- Added one application `AuthOutbox` port and a SQLx adapter that commits each
  associated auth transition, token, audit event, and canonical `email.send`
  outbox row on one acquired connection and transaction.
- Registration retries recover the exact valid admitted job. Expired, missing,
  or inactive exhausted work is repaired with a fresh token/job while retaining
  the stored password and locale; an actively leased job is treated as existing
  work and is not duplicated.
- Post-commit dispatch is explicit: worker-enabled wiring leaves the durable row
  for the worker; worker-disabled wiring claims only the admitted job. Inline
  delivery persists retry backoff, respects lease ownership, immediately
  dead-letters permanent failures, and scrubs terminal payloads.
- Reused the B06a mail admission validation through `enqueue_mail_on`; there is
  no second SQL enqueue path and no pool reacquisition inside the auth
  transaction.
- Resend sends a stable per-message `Idempotency-Key`, distinguishes its two
  documented 409 names, and reads only a bounded allowlisted error name. SMTP
  retains at-least-once semantics without claiming portable idempotency.
- Updated architecture, agent orientation, and deployment semantics. No schema
  change or migration was needed.

## Main files

- `crates/application/src/auth.rs`, `crates/application/src/email.rs`
- `crates/infrastructure/src/auth/outbox.rs`
- `crates/infrastructure/src/job/repo.rs`
- `crates/infrastructure/src/email/{queue,resend}.rs`
- `crates/infrastructure/src/job/email.rs`
- `crates/web/src/wiring.rs`
- `crates/application/tests/auth_test.rs`
- `crates/infrastructure/tests/{auth,job}_test.rs`
- `AGENTS.md`, `ARCHITECTURE.md`, `docs/deployment.md`

## Verification

All database commands used only the disposable loopback database
`postgres://bikesnest_test:…@127.0.0.1:55439/bikesnest_test_audit`, ran
sequentially, and used `/home/bruno/Projects/bikenest/target`.

- `cargo test -p bikesnest-application --test auth_test` — 30 passed.
- `cargo test -p bikesnest-infrastructure email::resend::tests` — 5 passed.
- `cargo test -p bikesnest-infrastructure --test auth_test -- --test-threads=1`
  — 27 passed.
- `cargo test -p bikesnest-infrastructure --test job_test -- --test-threads=1`
  — 17 passed.
- `cargo test -p bikesnest-web --test http_test -- --test-threads=1` — 172
  passed.
- `cargo check -p bikesnest-web` — passed.
- `cargo fmt --all -- --check` — passed.
- `cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p
  bikesnest-web --all-targets -- -D warnings` — passed.
- `git diff --check` — passed.

The first database invocation inside the restricted sandbox was denied loopback
access; the identical command was rerun with scoped approval and passed. No
production, release, deployment, asset, provider-send, or external mutation was
performed.

## Review focus and residual semantics

- Review the registration recovery query and exhausted-job terminalization for
  active-lease fencing and exact-job behavior.
- Review the inline dispatch no-claim state mapping: succeeded/actively owned is
  neutral, persisted backoff is unavailable, and missing/terminal work is not
  reported as handed off.
- Provider idempotency cannot guarantee exactly-once delivery. Resend retains
  keys for 24 hours, and rendering at send time means a catalog/from change can
  produce a payload conflict for the same key. SMTP remains explicitly
  at-least-once. Broader worker modes and supervision remain B07; template and
  security-notice expansion remains B14.
