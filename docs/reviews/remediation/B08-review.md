# B08 independent review

Date: 2026-09-10  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `ad5fdd6`  
Decision: **PASS**

I read `docs/reviews/remediation/B08-handoff.md`, the B08 plan criteria, the SEC-05/ENG-07 audit findings, current architecture/testing guidance, and applicable Rust concurrency, lifecycle, and web guidance. I inspected the complete frozen diff against `ad5fdd6`. I did not edit application source, load `.env`, contact a provider, build release artifacts, or perform production/deployment actions.

## Findings and acceptance

No material findings remain.

- `CpuAdmission` has distinct finite running and waiting semaphores. A free running permit is taken immediately; otherwise a waiter slot must be acquired without waiting, and execution admission has a finite deadline. The waiter permit drops as soon as execution is admitted and also drops on cancellation, timeout, closure, or error. Queue capacity zero correctly selects immediate overload rejection.
- The owned running permit moves into the `spawn_blocking` closure for both password hashing and verification, and the image permit likewise moves into the complete decode/encode closure. Cancelling an async caller therefore cannot undercount already-started blocking work. Join, admission, and pool-closure failures map to bounded existing internal errors without incorporating password, hash, image, or provider data.
- Test hooks execute inside the actual blocking closures. Their returned RAII guards span the real Argon2 or decode/encode computation, so peak assertions count blocking work rather than merely async callers. Outer release guards open the synthetic gates during unwind, preventing a failed assertion from leaving an unabortable blocking task hung. Cancellation, queue-full, finite timeout, waiter cancellation/recovery, and capacity recovery all have direct evidence.
- The production composition root creates one cloneable Argon2 adapter and passes clones to auth and privacy services, so hash and verify share the same process budget. Seed commands use the same parsed configuration and a bounded adapter; their one-shot budget need not be shared with a concurrently running server process. Configuration is parsed before database connection, rejects malformed/negative values, zero execution capacity, zero timeout, and semaphore-overflowing capacities, while explicitly allowing zero waiters.
- Registration, login, reset/change, and privacy confirmation still await credential work synchronously. No credential verification became a durable job. `Argon2::default()` and the emitted PHC regression preserve Argon2id m=19456 KiB, t=2, p=1; reset hash failure remains covered without consuming the credential. HTTP login continues to render one generic response for every service error, and no new secret-bearing diagnostic was introduced.
- Image admission intentionally retains the existing upstream request/body controls and unbounded semaphore wait; B08 closes the cancellation-sensitive running-permit defect. Process budgets remain per replica and complement rather than replace distributed request rate limits.

The handoff's debug timing/RSS numbers are honestly scoped to already-built debug test executables. They demonstrate the selected test workload and capacity ownership only; they are not production sizing, p95 latency, a release benchmark, or an SLO. The deployment text correctly requires measurement under deployed CPU and memory limits.

## Independent commands and results

All database-backed commands explicitly unset `DATABASE_URL`, used only the dedicated loopback audit database, ran with one test thread, and reused debug artifacts.

```text
git diff --stat ad5fdd6
git diff --name-status ad5fdd6
git diff ad5fdd6 -- <every changed path>
git diff --check ad5fdd6
```

Result: all 12 changed paths and the new `cpu.rs` plus handoff were inspected; diff check passed.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --lib --locked
```

Result: the sandboxed first run passed 97 tests and the unrelated Resend response-matrix test could not bind its local test server (`Operation not permitted`). Repeating the same command with loopback permission passed **98/98**, including all four password and both image admission tests. No external provider was contacted.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-application --locked
```

Result: **109 passed, 0 failed** across application suites.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test photo_test --locked -- --test-threads=1
```

Result: **11 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1
```

Result: **172 passed, 0 failed**, including login response neutrality and credential/reset flows.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo check --workspace --all-targets --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web \
  --all-targets --locked -- -D warnings
git diff --check ad5fdd6
```

Result: formatting, workspace all-target check, strict Clippy, and final diff validation passed.

B08 satisfies SEC-05/ENG-07 batch acceptance. This is source/test acceptance only; it does not authorize deployment or establish production capacity sizing.
