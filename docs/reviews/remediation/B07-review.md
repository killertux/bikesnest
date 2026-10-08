# B07 independent review

Date: 2026-09-10  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `fa3f92d`  
Decision: **PASS**

I read `docs/reviews/remediation/B07-handoff.md`, the B07 plan criteria and ENG-04/05/08 evidence, current `AGENTS.md`, `ARCHITECTURE.md`, `TESTING.md`, and applicable Rust web/concurrency guidance. I inspected every B07 source/document diff against `fa3f92d`, excluding the lead's ledger-only edits from the implementation assessment. I did not edit application source, run providers, build release assets, or perform production/deployment actions.

## Correction disposition

All material findings are resolved. All five multi-connection tests own disposable child databases; stale `fail`, retry/dead-letter write failures, and panic/timeout hook paths are exercised; and tests call the extracted production supervisor for graceful and unexpected exits.

The worker-only process test now arms an RAII `ChildGuard` immediately after spawn. Every unwind path kills and synchronously reaps the child, while a successfully observed exit disarms the guard; a live-child early-unwind regression directly verifies cleanup. Each temporary cwd contains an empty `.env` sentinel, terminating dotenv ancestor traversal. The timeout regression now bounded-polls for the durable `pending` state and exact bounded reason before asserting the handler is detached. Three consecutive full runs passed, resolving the previously reproduced finalization race.

## Confirmed behavior

- The worker claims one row per free slot with a unique claim owner, immediately spawns each admitted attempt, and continues polling while capacity remains occupied. The current capacity test demonstrates a newly arriving second job starts beside a blocked first job, a third stays pending at capacity two, peak execution is two, and both running leases remain live.
- Every started normal attempt owns a heartbeat using the claim-specific owner. Heartbeat loss cancels and joins the handler. Handler panic and timeout become bounded retry classifications; timeout aborts and joins the handler.
- Repository source fences heartbeat, success/reschedule, retry and fail by running state, exact owner, and unexpired lease, returning `LostOwnership` for zero rows. The isolated reclaim test verifies real expiry/reclaim behavior for all four paths.
- Worker shutdown stops admission, permits one natural grace interval, then cancels owned attempts; each attempt aborts and joins its handler and heartbeat. A second bounded drain and final `abort_all` prevent detached children.
- Success/retry/dead-letter logs occur only after successful persistence. Outcome, heartbeat, task, hook and shutdown failures increment a bounded in-process diagnostic and use allowlisted classifications; handler error text is not copied into structured worker events. Rust's process panic hook remains an explicitly documented console limitation.
- `JOBS_RUN_WORKER` and `JOBS_DURABLE_ENQUEUE` independently override the legacy value; legacy `JOBS_ENABLED` supplies either value only when its replacement is absent. The defaults preserve worker plus durable admission, while legacy false preserves inline/no-worker behavior. Validation rejects zero capacity, too-short leases, and zero handler/shutdown bounds before the serve/worker database path.
- The `worker` subcommand migrates and builds job services without constructing a router or binding `BIND_ADDR`. Web-only mode can retain durable auth admission. B04 recurring registration still passes its full suite, and B06's mail handler/outbox lifecycle remains reused.
- No migration or schema change was introduced. External side effects remain honestly at-least-once; B07 fencing cannot recall a provider-accepted message.

## Commands and results

All database commands explicitly unset `DATABASE_URL`, used only the dedicated loopback audit target, limited each database test binary to one test thread, and reused debug artifacts.

```text
git status --short
git diff --name-only fa3f92d
git diff --stat fa3f92d
git diff --check fa3f92d
git diff fa3f92d -- <all changed paths>
```

Result: 14 changed paths plus the handoff were inspected; diff check passed.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test job_test --locked -- --test-threads=1
```

Result after the second correction: **28 passed, 0 failed in three consecutive full runs**. Before this correction, one full run passed 27/28 because `worker_times_out_and_does_not_detach_the_handler` read `running` before the retry write; the new durable-state wait removes that race.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test recurring_job_test --locked -- --test-threads=1
```

Result: **4 passed, 0 failed**.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --bin bikesnest-web --locked
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test worker_cli_test --locked -- --test-threads=1
```

Result: production-supervisor unit tests **2 passed**; worker CLI tests **3 passed**, including the live-child unwind/reap regression.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web \
  --all-targets --locked -- -D warnings
git diff --check fa3f92d
```

Result: formatting, strict Clippy and diff checks passed.

B07 satisfies its batch acceptance criteria. B08 and later wider execution/delivery work remain outside this review.
