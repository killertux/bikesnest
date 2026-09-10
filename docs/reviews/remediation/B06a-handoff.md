# B06a pending independent review

This is an implementation handoff, **not an approval record**.

- Worktree: `/tmp/bikesnest-audit-remediation`
- Branch: `fix/audit-remediation`
- Baseline: `a3e6a2b` (B05r accepted)
- Implementer: GPT Sol `b06a_implement`, completed
- Independent reviewer: not assigned; agent service rejects new/revived reviewer threads at its limit
- Source state: uncommitted B06a diff; do not deploy or start B06b before review passes

## Implemented scope to review

Migration 0026 introduces account/token/purpose/expiry metadata and redaction timestamps for mail jobs. Legacy mail payloads are scrubbed while preserving existing terminal history; pending/running legacy mail is stopped. Other job kinds must remain unchanged.

Queue admission and provider delivery validate canonical account/token/recipient state while holding the account lock. Delivery is bounded, with an explicit idle-transaction timeout policy. Canonical anonymization scrubs all associated queue states. Terminal outcomes scrub payloads; retention also handles expired never-claimed/retrying mail. Inline delivery uses the same guard. Debug/decode/provider errors are sanitized.

Deployment notes require quiescing every old sender before this migration, including inline senders. No mixed-version rolling mail delivery or routine backup restoration is safe. External provider acceptance, network ambiguity, disabled workers/outages, maintenance cadence and backup aging are documented limits, not solved distributed-atomicity guarantees.

## Reported checks

Implementer reports all passed:

- Job/mail: 12/12
- Privacy: 21/21
- Application auth: 30/30
- Infrastructure auth: 23/23
- HTTP: 172/172
- Strict application/infrastructure/web Clippy, all targets
- Workspace check, formatting, diff check

Lead independently ran the DB-free domain/application tests: 179 passed. These checks do **not** substitute for the required separate review.

## Required reviewer attention

Read `AGENTS.md`, `ARCHITECTURE.md`, `TESTING.md`, applicable skills, the remediation plan and audit SEC-02/SEC-06. Inspect the entire diff against `a3e6a2b`, not only this handoff.

Verify real positive/negative lifecycle tests for every mail purpose and inline delivery; exact terminal/deletion/expiry persistence; synthetic token/email markers absent from persisted errors and captured tracing; upgrade mixed-state/unrelated-job preservation; and observed lock-wait evidence for send/deletion and enqueue/deletion ordering. New sequential fixtures must roll back; real races/upgrades must own disposable databases. No new committed shared fixtures or cleanup should be introduced.

Lead draft findings included encoded-versus-decoded token hashing, pending-account reset eligibility, terminal-history rewriting, idle transaction timeout during sends, enqueue-after-deletion, and new shared test writes. Implementer reports all corrected; independently confirm.

Write `docs/reviews/remediation/B06a-review.md` with commands, findings and PASS/CHANGES REQUESTED. Reviewer must not edit application source. Return findings to implementation and re-review corrections before acceptance.

## Safe test environment

Use only the dedicated loopback audit database; never source `.env`:

```bash
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test job_test --locked
```

Use the corresponding privacy/auth/HTTP suites and bounded strict checks. Debug artifacts only. No production checkout assets, release builds, external email/provider calls, shared cleanup or deployment. The audit database already received migration 0026 during implementation tests; do not modify migration checksums to evade validation.

B06b remains responsible for atomic account/token/outbox creation and delivery retry/idempotency semantics; B07 for worker supervision/fencing; B14 for email visual templates/security notifications. Preserve these boundaries without hiding their remaining limitations.
