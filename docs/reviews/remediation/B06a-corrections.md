# B06a correction implementation handoff

Date: 2026-09-10  
Implementer: GPT Sol (`b06a_corrections`)  
Baseline: `a3e6a2b`  
Disposition: ready for the independent reviewer to recheck; not accepted or deployed

## Corrections

- Removed recipient-derived domains from `EmailMessage` debug output and queue-
  admission events. Admission failures now log only allowlisted purpose and
  reason codes. A hostile-domain regression covers direct debug formatting and
  a captured, real closed-pool admission failure.
- Replaced the confounded delivery test with fresh fixtures for every case.
  Verify, reset and change now each have a positive case and independent
  account-state, exact-token, recipient, used-token and expiry rejection cases
  through both `SendEmailHandler` and `InlineEmailQueue`. Durable queue admission
  exercises the same positive/negative purpose matrix and SQL branches.
- Corrected touched application, queue, wiring and deployment comments: auth
  transition and enqueue are separate transactions; admission deduplication is
  not exactly-once delivery; lease expiry and ambiguous provider outcomes may
  duplicate delivery.

No migration, dependency, provider, asset, release, production or deployment
operation was added or performed. The tests use transaction-scoped fixtures;
no shared committed fixture or cleanup was introduced.

## Commands and results

All database-backed commands explicitly unset `DATABASE_URL` and used only
`postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit`.
The first sandboxed lifecycle invocation was denied before connecting, then the
same command was allowed for the loopback test database.

```text
cargo test -p bikesnest-application email::tests --locked
```

Result: 4 passed.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test job_test \
  queue_admission_database_error_tracing_excludes_hostile_recipient_data --locked
```

Result: 1 passed; this test uses a deliberately closed lazy pool and makes no
database connection.

```text
env -u DATABASE_URL TEST_DATABASE_URL=<dedicated-loopback-audit-db> \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test job_test --locked
```

Result: 14 passed, including the full delivery and admission matrices.

```text
cargo fmt --all -- --check
git diff --check a3e6a2b
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure \
  -p bikesnest-web --all-targets --locked -- -D warnings
```

Result: all passed.

The independent reviewer should inspect the complete B06a diff and re-run the
focused job/mail suite before changing the review decision.
