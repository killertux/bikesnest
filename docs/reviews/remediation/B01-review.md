# B01 independent review

Date: 2026-09-08

Baseline: `6874af0`

Reviewed state: uncommitted B01 diff in `/tmp/bikesnest-audit-remediation`

Implementation model: GPT Sol (`b01_implement`)

Review model: GPT Sol (`b01_review_retry`)

## Checks performed

- Read `AGENTS.md`, `ARCHITECTURE.md`, `TESTING.md`, the B01 acceptance criteria in `docs/plans/2026-09-08-audit-remediation.md`, and the applicable Rust domain/concurrency review guidance.
- Inspected the application auth port/service changes, supporting fake implementations, SQLx account/token adapters, and application/infrastructure auth regressions against baseline `6874af0`.
- Traced all callers of `confirm_email_verification`, `issue_verification`, `issue_reset`, `suspend_and_revoke_security_tokens`, the legacy token-consume operation, and administrator restore/state transitions.
- Verified by inspection that confirmation and suspension share the `users` row lock as their serialization point. Confirmation consumes its token and updates the canonical email, password identity, verification timestamp, and eligible account state in one transaction. Suspension changes state and revokes sessions, verification tokens, and reset tokens in one transaction.
- Considered both lock orders: confirmation followed by suspension ends suspended with credentials revoked; suspension followed by confirmation makes confirmation observe an ineligible state and return no outcome. Competing confirmations are also constrained by the account lock plus the guarded token update.
- Confirmed the application layer returns token-invalid for suspended/deleted verification attempts, keeps resend/reset enumeration-neutral, requests active-state issuance for email changes, and delegates suspension revocation to the atomic repository operation.
- Confirmed the existing application tests exercise successful initial verification, successful active-account email change/session revocation, single use, suspended/deleted rejection, neutral resend behavior, and pre-suspension verification/reset invalidation after restore.
- Confirmed the new real-Postgres tests exercise atomic suspension revocation across restore and rollback of token consumption when an email-change confirmation conflicts. The latter completes successfully after the conflicting address is freed, covering the active email-change repository path.
- Ran `env -u DATABASE_URL TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target cargo test -p bikesnest-application --test auth_test`: passed, 25 tests.
- Ran the same environment with `cargo test -p bikesnest-infrastructure --test auth_test`: the sandboxed attempt failed before connecting with `Operation not permitted`; the unchanged command was rerun with approved loopback access and passed, 12 tests. The target was the plan's disposable audit database; no production database was accessed.
- Ran the same environment with `cargo check --workspace --locked`: passed.
- Ran the same environment with `cargo clippy -p bikesnest-application -p bikesnest-infrastructure --all-targets --locked -- -D warnings`: passed.
- Ran `cargo fmt --all -- --check` and `git diff --check 6874af0`: both passed.
- Re-reviewed the correction diff. The new real-Postgres suite now has 15 tests and directly covers successful pending confirmation, suspended/deleted confirmation with still-unused tokens, and rejected issuance cases; the module header now accurately distinguishes legacy pooled fixtures from scoped rollback tests.
- Reran the corrected infrastructure suite against the same approved disposable loopback database: passed, 15 tests. Reran the application suite: passed, 25 tests. Reran workspace check, targeted strict Clippy, fmt, and diff check: all passed.
- Re-reviewed the final issuance matrix: suspended and deleted database rows are each exercised with both otherwise-valid expected states (`PendingEmailVerification` and `Active`), while invalid expected enum states remain covered separately. Reran the real-Postgres infrastructure auth suite: passed, 15 tests. Reran strict Clippy for the infrastructure auth test target, fmt, and diff check: all passed.

## Findings

1. **Resolved — authoritative persistence guards have direct regression evidence.** `confirmation_rejects_unused_tokens_for_suspended_and_deleted_accounts` proves the confirmation guard using still-unused tokens. `token_issuance_rejects_mismatched_and_blocked_account_states` now proves pending/active mismatches, blocked reset issuance, and both valid expected verification states against each suspended/deleted database row, asserting rejection and no stored token. The blocked/valid combinations pass the Rust enum allowlist and therefore exercise the SQL account-state predicate.

2. **Resolved — successful initial verification against the real repository.** `pending_account_confirmation_activates_without_changing_identity` issues and confirms a real pending-account token, then asserts the outcome, active/verified account state, unchanged canonical email, matching password identity, and single use.

3. **Resolved — infrastructure auth-test module documentation.** The header now accurately describes both the older committed pool fixtures and the new transaction-scoped rollback regressions.

No production-code correctness defect was demonstrated in the reviewed atomic transition itself. The unreferenced `TokenStore::consume_verification` operation remains outside the suspension-safe aggregate operation, but no production caller uses it; retaining that broader port surface is a follow-up hardening consideration rather than a current B01 bypass.

## Known residual risk

The generic administrator restore/state path still sets `ACTIVE` without constraining the prior state, so it can activate deleted or unverified pending accounts, and suspension can overwrite deletion. This pre-existing lifecycle issue is recorded separately as queued B05r and is not treated as resolved by B01. B01 must nevertheless retain its own verification/issuance guards and tests.

## Disposition

**PASS**

The suspension-safe verification transition, guarded issuance, atomic suspension revocation, successful pending confirmation, active email change, rollback behavior, and old-token invalidation meet the B01 acceptance criteria. All review findings are resolved and all executed checks pass. The separately recorded generic administrator state-transition risk remains queued as B05r and is not represented as fixed here. This review grants no deployment or production-operation authority.
