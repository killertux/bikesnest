# B04 independent review — recurring schedules and reconciliation

## Review identity and scope

- Baseline: `eede1b2`
- Reviewed worktree: `fix/audit-remediation` (corrected, uncommitted B04 diff)
- Implementer: GPT Sol (`b04_implement`)
- Reviewer: GPT Sol (`b04_review`), independent source and test review
- Scope: `ENG-01` / `PROD-01`; source acceptance only, not deployment approval

## Disposition

**PASS**

The corrected implementation satisfies the B04 acceptance criteria. Recurring
registration validates and persists a real schedule; the stable-key UNIQUE
constraint and row lock serialize reconciliation; exact mismatches remain
untouched; healthy future rows and running ownership are preserved; legacy
pending/terminal rows are repaired; running one-shot snapshots are deferred;
and scheduled failures remain available for explicit recovery and are excluded
from GC.

The worker retries incomplete bootstrap on a separate deadline while continuing
kind-scoped independent claims. A failed entry does not stop later registry
entries in the same bootstrap pass. Public `bootstrap` and `process_claimed`
exercise the production paths directly, while the read-only diagnostics and
kind-restricted worker loop allow deterministic scoped verification without
unscoped claims against the shared test database.

## Correction re-review

The first review requested changes. Each finding is resolved:

- The recurrence test now executes the full path twice: registry → bootstrap →
  `claim_kinds` → harmless handler → worker finish → future row → second claim
  and execution. Repeated bootstrap proves the same row and future `run_at` are
  retained.
- Legacy pending, succeeded, and failed rows now have persisted fields asserted
  after repair and a repeated registration proving stable row identity and run
  time. Scheduled failure is registered repeatedly and retains state, attempts,
  error, completion time, and identity through GC.
- Both unscheduled-running deferral and subsequent terminal repair are checked.
  A separate scheduled-running case proves claim identity, lease, attempts,
  schedule, row identity, and `run_at` are untouched.
- Kind, payload, and schedule conflicts are separate cases, so every guard is
  reached; complete stored rows are compared before and after. JSON NULL, empty
  object, zero interval, and scalar schedules are rejected without inserts.
- The production worker loop now applies an independent bootstrap retry deadline
  (at least 250 ms) even while work remains continuously claimable. The actual
  shared loop test proves at least three harmless jobs complete while bootstrap
  is attempted only once within that interval, then cancels and joins the worker.
- GC now excludes every scheduled terminal row rather than an allowlist of two
  keys. Its test uses a test-specific kind and key.

No remaining B04 finding was identified.

## Evidence run

All reviewer commands ran in `/tmp/bikesnest-audit-remediation`. No deployment,
migration, production data, real retention command, or real email action was
performed.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure \
    --test recurring_job_test --test job_test --locked
```

Result: **15 passed, 0 failed** — 4/4 corrected recurring tests and 11/11
existing job tests (1.18 s and 0.22 s respectively, excluding build time).
The first sandboxed attempt could not access loopback and executed no behavioral
tests; the successful rerun used approved loopback access to only the disposable
audit database.

```text
env -u DATABASE_URL TEST_DATABASE_URL=<disposable-audit-db> \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-infrastructure --all-targets --locked -- -D warnings
env -u DATABASE_URL TEST_DATABASE_URL=<disposable-audit-db> \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo check --workspace --locked
cargo fmt --all -- --check
git diff --check eede1b2
```

Result: all passed.

The lead separately ran the full HTTP suite on the final corrected diff against
the disposable database: **172/172 passed** in 2.50 seconds. That cross-check
was reported by the lead rather than executed by this reviewer; its initial
permission attempt created no process and the single successful retry produced
the stated result.

## Operational documentation

`docs/deployment.md` truthfully identifies the two exact built-in keys and
schedules, read-only preflight fields, mismatch stop condition, post-deployment
verification, active-lease preservation, scheduled-failure recovery policy,
rollback without schema rollback, and state-aware monitoring. It now clarifies
that `finished_at` is a last-success time only for a successfully rescheduled
pending row; after failure it is the failure completion time. No migration is
needed.

## Limitations and follow-ups

- No true multi-connection bootstrap race was run. Source inspection supports
  the safeguard: concurrent inserts serialize on the UNIQUE idempotency key,
  then the conflict path selects the exact row `FOR UPDATE`. Empirical dedicated
  race coverage remains B16b and is not claimed by B04.
- Broader worker lease/capacity/fencing and persistence-outcome handling remain
  B07. The correction does not regress or claim to solve them.
- This approval covers reviewed source only. It is not authorization to deploy
  or reconcile production rows.
