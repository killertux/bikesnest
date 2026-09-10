# B08 implementation handoff

Baseline: `ad5fdd6`. This batch is intentionally uncommitted and requires an
independent review.

## Outcome

- Image decode/encode now moves its owned semaphore permit into the
  `spawn_blocking` closure. Cancelling the awaiting request can cancel a queued
  caller, but cannot release capacity held by work already running.
- Argon2 hash and verify share one cloneable process budget constructed by the
  composition root. New callers take a free execution slot immediately. When
  saturated, only `PASSWORD_HASH_QUEUE_CAPACITY` callers may wait, and only for
  `PASSWORD_HASH_ADMISSION_TIMEOUT_MS`; execution capacity remains owned by the
  blocking closure after caller cancellation. Queue-full, timeout, closed-pool,
  and join failures all remain the existing bounded `AuthError::Internal` with
  no password/hash/provider text.
- Interactive registration, login, password change/reset, and privacy
  confirmation still synchronously await hashing. No login or credential work
  became a durable job, and Argon2id parameters remain unchanged at m=19456
  KiB, t=2, p=1. Existing reset tests continue to prove a hash failure does not
  consume the reset credential.
- Configuration rejects malformed/negative values, zero execution capacity,
  zero timeout, and values above Tokio's semaphore limit before any database
  connection. Queue capacity zero is a supported no-wait mode. Defaults are
  concurrency 2, waiters 8, deadline 2000 ms.
- No migration, catalog copy, provider call, or deployment change was needed.

## Deterministic lifecycle evidence

Test-only hooks run inside the actual blocking closures and return RAII guards
held through the complete real computation. Fail-safe release guards prevent a
failed assertion from leaving an unabortable blocking task gated forever.

- Cancelling a running public `hash` future leaves capacity occupied until its
  real Argon2 closure completes; a public `verify` times out rather than entering
  early, then hash and verify recover afterward.
- A separate adapter test fills the one-slot waiting queue, observes immediate
  queue-full rejection, cancels the queued verify caller, observes the waiter
  slot return, and exercises the finite admission timeout.
- Six real hashes through the public adapter with capacity two reached an
  actual blocking-closure peak of exactly two.
- Image tests cancel both a running and queued public `process` caller and prove
  replacement work cannot enter early. Six real 1024×1024 PNG decode plus full
  and thumbnail JPEG pipelines with capacity two likewise reached a closure
  peak of exactly two.

## Measurement and defaults

These measurements ran the already-built debug test executable directly; they
are reproducible development evidence for capacity ownership, not production
sizing or an SLO:

- Six Argon2 hashes, capacity two: **1221 ms**, **52,012 KiB maximum RSS**.
- Six 1024×1024 PNG image pipelines, capacity two: **1144 ms**, **29,132 KiB
  maximum RSS**.

`/usr/bin/time -v` measured the isolated test process, excluding Cargo and
compilation. The Argon default's nominal working allocation is at least 38 MiB
for two 19 MiB operations; measured total process RSS and production allocator,
stack, request, and runtime overhead differ. The finite queue retains request
inputs but does not start additional Argon memory allocations. Operators must
measure within their deployed CPU/memory limits before changing the knobs. No
release application binary or shared release artifact was built.

## Verification

- Focused password admission: **4 passed**.
- Focused image admission: **2 passed**.
- Infrastructure library: **98 passed**.
- Application crate: **109 passed** across its suites.
- Infrastructure photo integration: **11 passed** using the dedicated
  disposable loopback test database.
- Web HTTP integration: **172 passed** using the same disposable test target.
- `cargo fmt --all -- --check`, workspace/all-target check, strict locked
  Clippy for application/infrastructure/web, and `git diff --check ad5fdd6`
  are the final gates.

## Review focus and limitations

- Inspect the immediate execution fast path versus finite waiter admission and
  confirm the waiter permit drops immediately after execution admission.
- Inspect that every production hasher clone in router wiring shares the same
  semaphores, and that the permit is captured by each blocking closure.
- Image waiting remains bounded by the upstream HTTP body/rate limits and the
  existing semaphore; B08 changes its cancellation-sensitive permit lifetime.
- Process-local budgets do not coordinate separate replicas. Per-IP/account
  rate limits remain complementary distributed abuse controls.
