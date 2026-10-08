# B07 correction handoff

Baseline: `fa3f92d`. This correction remains uncommitted and is ready for the
same independent reviewer to recheck. The reviewer record and remediation plan
were not edited by this correction pass.

## Corrections

- The five new worker concurrency/lifecycle tests identified by the reviewer
  now use `run_isolated_database_test`. Each owns a freshly migrated child
  database, uses only its child pool, awaits disposal, and has no shared
  `clear_kind` cleanup or committed fixture.
- The real lease/reclaim test now exercises stale `fail` as well as heartbeat,
  success, and retry fencing. Separate fault-injection regressions verify that
  retry and dead-letter outcome-write failures remain observable and do not
  falsely transition the row. Panicking and timing-out dead-letter hooks are
  also invoked directly and verified as contained, bounded diagnostics.
- Server/worker selection and drain classification now live in the production
  `supervise` routine. Unit tests call that routine for a normal signal,
  unexpected worker return, worker panic, server failure, and forced task
  abort-and-join. A server failure discovered while draining also remains a
  failure rather than being reported as a clean signal exit.
- `worker_cli_test` supplies process-boundary evidence. Its positive case owns
  a fresh child database, starts in a new empty working directory after
  `env_clear` (so no repository `.env` is loaded), uses fake email and a
  harmless loopback storage endpoint, waits for durable bootstrap evidence,
  sends SIGTERM, and enforces a hard kill/reap bound. An invalid `BIND_ADDR`
  proves the worker command does not bind HTTP. The negative case likewise
  starts in an empty directory and proves invalid job configuration is
  reported before an intentionally unreachable database is contacted. URL
  query options, including an overriding `dbname`, are retained when targeting
  the child database.

### Second review pass

- The worker subprocess is now owned by an armed RAII guard immediately after
  spawn. Every return, assertion failure, database error, signal error, timeout,
  or panic kills and synchronously reaps the child; observing a successful exit
  explicitly disarms the guard. A direct early-unwind regression starts a live
  sentinel child and proves it no longer exists after guard unwinding.
- Every temporary process working directory contains an empty `.env` sentinel,
  preventing `dotenv` from walking into an ambient ancestor file. This applies
  to both positive and negative command tests.
- The handler-timeout regression now polls the durable row under a hard bound
  until the worker has persisted `pending` plus its bounded timeout reason,
  then verifies the handler is detached. It no longer treats handler `Drop` as
  evidence that the later outcome write has completed.

No schema or migration change was needed.

## Verification

Database commands unset `DATABASE_URL` and used only the dedicated loopback
audit PostgreSQL target:

```text
TEST_DATABASE_URL=postgres://bikesnest_test:…@127.0.0.1:55439/bikesnest_test_audit
```

Credentials are elided here; the exact commands supplied the full disposable
test-role URL locally.

- `cargo test -p bikesnest-infrastructure --test job_test --locked --
  --test-threads=1`: **28 passed, 0 failed in three consecutive full runs**.
- `cargo test -p bikesnest-infrastructure --test recurring_job_test --locked
  -- --test-threads=1`: **4 passed, 0 failed**.
- `cargo test -p bikesnest-web --bin bikesnest-web --locked`: **2 passed, 0
  failed**.
- `cargo test -p bikesnest-web --test worker_cli_test --locked --
  --test-threads=1`: **3 passed, 0 failed**. The positive test created and
  disposed its own child database; the third case is the explicit unwind
  cleanup proof.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p
  bikesnest-web --all-targets --locked -- -D warnings`: passed.
- `git diff --check fa3f92d`: passed.

## Remaining limitations

- The process test relies on Unix `kill` semantics, matching the Linux
  deployment environment. It uses a hard ten-second reap bound so a broken
  shutdown cannot hang the suite.
- The standard Rust panic hook may print the synthetic panic payload in tests;
  persisted and structured worker diagnostics remain allowlisted and do not
  include it.
- Delivery remains at-least-once across an external provider boundary. These
  corrections do not expand B07 into B08 or later worker execution work.
