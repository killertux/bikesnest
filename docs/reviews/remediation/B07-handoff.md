# B07 implementation handoff

Baseline: `fa3f92d`. This worktree is intentionally uncommitted and requires an
independent review.

## Outcome

- `JOBS_BATCH_SIZE` is now a concurrency capacity. The worker claims one row
  per free slot with a unique claim token and starts every claimed row
  immediately; polling continues while blocked work leaves capacity available.
- Claims, heartbeats, and every outcome write are fenced by current owner,
  `running` state, and an unexpired lease. Zero affected rows return an explicit
  `LostOwnership` error. Lease/heartbeat calculations use integer milliseconds,
  and heartbeat cadence remains strictly inside validated lease TTL.
- Each active attempt owns a heartbeat and a bounded handler task. Handler
  panics become a bounded retry classification; timeouts abort and join the
  handler. Heartbeat failure cancels the handler. Dead-letter callbacks are
  likewise bounded and panic-contained. The standard Rust panic hook can still
  print a panic payload before the contained `JoinError`; worker logs and
  persisted classifications never include that payload.
- Outcome-write, heartbeat, task, and shutdown failures increment observable
  diagnostics and emit structured allowlisted classifications. Success/retry/
  dead-letter logs are emitted only after the corresponding write succeeds.
- Shutdown stops bootstrap/claim admission, gives active attempts one natural
  grace interval, then explicitly cancels and joins handler/heartbeat children;
  a final abort-on-drop guard protects abrupt outer cancellation. HTTP and
  worker lifetimes are supervised together, with bounded server and worker
  drains and nonzero exit on an unexpected component failure.
- Durable auth-mail admission (`JOBS_DURABLE_ENQUEUE`) is independent from
  worker execution (`JOBS_RUN_WORKER`). The legacy `JOBS_ENABLED` value maps to
  both only when the new knobs are absent, preserving `false` as the historical
  inline/no-worker mode. `bikesnest-web worker` migrates and runs jobs without
  constructing or binding the HTTP router.
- No migration or schema change was needed. B04 recurrence and B06 mail
  lifecycle/redaction behavior remain shared.

## Verification

All database suites used the dedicated disposable loopback audit Postgres at
port 55439. Genuine lease/reclaim fencing used the test harness's freshly
created independent child database.

- Infrastructure job integration: **24 passed**. This includes deterministic
  arrival during spare blocked capacity, capacity bounding, all-active
  heartbeat, panic, timeout, heartbeat ownership loss,
  child drain after grace, injected outcome-write failure, and independent
  expired/reclaimed lease fencing.
- Recurring-job integration: **4 passed**.
- Infrastructure unit suite: **92 passed**.
- Web HTTP suite: **172 passed**.
- Web binary supervision tests: **2 passed** (normal cancellation
  classification and abort-and-join after supervisor grace).
- Worker-only positive smoke: built the binary, created
  `bikesnest_test_b07_cli_smoke_final` in the dedicated test container, launched
  with a clean environment, fake email, and deliberately invalid `BIND_ADDR`,
  then sent SIGTERM. It reconciled jobs, never bound HTTP, drained, and exited
  0; the exact temporary database was dropped afterward.
- Negative configuration smoke used an invalid database URL plus
  `JOBS_BATCH_SIZE=0` and failed before any database connection attempt.
- `cargo fmt --all -- --check`, strict locked Clippy for application,
  infrastructure and web all targets, workspace check, and `git diff --check
  fa3f92d` are the final gates listed below.

An earlier exploratory positive smoke used the disposable audit database rather
than a fresh child. It encountered leftover disposable test rows and attempted
the configured localhost development S3 endpoint, which refused the connection;
it did not contact a real email/provider or production system. The final smoke
above replaced that evidence with a fresh database and clean environment.

## Review focus and limitations

- Inspect the capacity refill/select loop, two-stage worker drain, and main
  server/worker supervision race classification.
- Inspect the unexpired-lease predicates and per-claim owner returned with each
  `ClaimedJob`.
- Delivery remains at-least-once: fencing prevents stale database outcomes but
  cannot recall an external side effect accepted immediately before a crash.
- Diagnostics are in-process counters plus structured logs; exporting queue
  health as a new readiness contract was intentionally not added. B08/B09 and
  broader execution lanes remain outside B07.
