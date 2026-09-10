# B05 independent review — atomic password reset

- Baseline: `2bd8d35`
- Reviewed state: uncommitted B05 diff on `fix/audit-remediation`
- Implementer: GPT Sol (`b05_implement`)
- Reviewer: GPT Sol (`b05_review`)
- Scope: SEC-03 password-reset validation and atomic credential transition
- Disposition: **PASS after re-review**

## Acceptance review

The production application orders token decoding, password-policy validation, and hashing before the reset transaction. It captures the application clock after hashing. The SQL adapter uses `Db::acquire()`, begins one transaction, identifies the token's account, locks that account row before mutation, then conditionally consumes the token, replaces the password credential, revokes every live session, invalidates every unused reset token for the account, inserts the success audit event, and commits. Suspended and deleted accounts are rejected; pending and active states are not rewritten. A missing password identity returns an error and relies on transaction rollback. The former post-commit application audit write has been removed. Password-reset requests for an account without a password identity remain neutral and enqueue neither token nor mail.

The SQL lock and conditional token update correctly serialize two resets that already selected tokens for the same account: after the first transaction invalidates all tokens and commits, the waiter obtains the account lock and its conditional consume affects zero rows. This scoped suite does not claim a genuine multi-connection reset/suspension race; that remains the stated B16b obligation. The adjacent authenticated `change_password` split transition is likewise outside B05 and remains the B06b/B14 follow-up.

## Initial findings

### P1 — application fake does not implement the atomic port's expiry or audit contract

`FakeRepo::complete_password_reset` ignores its `at` argument, and `FakeDb::reset` stores no expiry. An expired token therefore succeeds in the fast application model even though the SQL adapter rejects it. The same fake mutates credential/session/token state but never appends the required `auth.password_changed` audit event. This is a semantic divergence in the principal non-database test double for a security-critical transactional port, not merely missing test polish.

Required correction: model reset-token expiry in the fake, enforce the same strict `expires_at > at` rule, and make successful completion append the exact durable audit representation used by the fake. Add application-level assertions that an expired token is rejected without credential/session/token/audit mutation and that a successful reset emits exactly one expected event. Preserve atomic fake behavior on all error paths.

### P1 — mandatory persisted non-change and exact-audit evidence is incomplete

The SQL happy-path test counts rows filtered only by actor and action. It does not assert the full event (`target_type`, `target_id`, `result`, metadata), nor prove that reuse adds no event. The expiry and blocked-state branches do not assert the old credential, live session, competing token, and absence of audit. The missing-password-identity branch proves only that the presented token rolls back; it does not include/assert a live session, competing token, unchanged credential state, and zero audit events. The audit-failure retry test strongly proves rollback of the old hash, live session, and both tokens, but does not assert the exact single audit row after the successful retry.

Required correction: extend scoped real-Postgres tests to assert the complete persisted change/non-change set and exact audit cardinality/content for success, reuse, expiry, suspended/deleted accounts, missing password identity, injected audit failure, and successful retry. Keep the existing transaction-scoped `tx.db()`/`Db::acquire()` pattern and do not add global cleanup or `commit_fixture`.

### P2 — expiration is evaluated against a pre-lock timestamp

Capturing `at` after hashing fixes the original slow-hash expiry gap. However, the adapter reuses that timestamp after connection acquisition and an unbounded wait for the account row lock. A token valid at repository entry can therefore be accepted after its real expiry if lock contention spans the boundary. That weakens the stated single-use, expiring-token guarantee at the actual credential transition.

Required correction: retain deterministic clock semantics while ensuring the final conditional consume checks expiry at (or immediately after) acquiring the account lock, and add a deterministic regression for a token that expires during the lock wait. Maintain the B01 account-first lock order. If the team intentionally defines validity at reset-transaction request time rather than mutation time, that weaker contract must be made explicit in the port/security acceptance criteria; the current comments instead claim the timestamp is the instant the atomic transition begins.

## Re-review resolution

All three findings are resolved in the corrected diff.

- The application fake now stores expiry with reset tokens, applies the same strict `expires_at > at` rule, and appends the exact password-change audit event as part of the same mutex-guarded transition. Its new expired-token regression proves credential, session, token, and audit state remain unchanged. Weak-policy and hash failures retain token usability and emit no audit; the successful retry emits exactly one fully asserted event.
- The real-Postgres matrix now checks exact audit fields and cardinality together with credential, session, all-token, and account-state changes or non-changes for pending/active success, reuse, expiry, suspended/deleted state, missing password identity, injected final-audit failure, and successful retry.
- SQL now applies `expires_at > GREATEST($2, clock_timestamp())` both during lookup and in the final conditional consume after the account row lock. A real independent-connection test observes the reset transaction waiting on that lock, moves the token deadline inside the blocking transaction, releases it after expiry, and proves zero credential, session, token, or audit mutation.

The isolated-database runner is appropriately bounded for this required race evidence. It starts from the existing validated `TEST_DATABASE_URL`, adds a loopback-only host guard, generates an allowlisted lowercase database name capped to PostgreSQL's 63-byte identifier limit, never derives a drop target from caller input, and force-drops only the exact generated database after its pool is closed. Connect/migration failures proceed to cleanup after creation; success and caught test panic are both covered by actual absence checks. Diagnostic panics omit URLs and credentials. The documented unavoidable process-kill limitation and `CREATEDB` requirement are accurate. It cannot drop the supplied shared database because the generated target is independent and fixed before the test closure is invoked.

No additional review findings remain.

## Commands actually run

All commands ran in `/tmp/bikesnest-audit-remediation` with the shared non-release target directory. Database suites used only `TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit` and explicitly removed `DATABASE_URL`.

- `cargo test -p bikesnest-application --test auth_test --locked` — **PASS**, 28/28.
- `cargo test -p bikesnest-infrastructure --test auth_test --locked` — initial sandbox attempt could not open loopback (`Operation not permitted`); approved rerun against the dedicated audit database **PASS**, 18/18.
- `cargo test -p bikesnest-web --test http_test --locked` — **PASS**, 172/172.
- `cargo clippy -p bikesnest-application -p bikesnest-infrastructure --all-targets --locked -- -D warnings` — **PASS**.
- `cargo check --workspace --locked` — **PASS**.
- `cargo fmt --all -- --check` — **PASS**.
- `git diff --check` — **PASS**.

Re-review commands, independently run against the corrected diff:

- `cargo test -p bikesnest-application --test auth_test --locked` — **PASS**, 29/29.
- `cargo test -p bikesnest-test-support --lib --locked` — **PASS**, 6/6.
- `cargo test -p bikesnest-infrastructure --test auth_test --locked` — **PASS**, 20/20, including the observed real lock wait and isolated-runner panic cleanup.
- `cargo test -p bikesnest-web --test http_test --locked` — **PASS**, 172/172. The previously reported legacy review foreign-key flake did not occur on this independent run; it remains a B16 follow-up and is not claimed fixed.
- `cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-test-support --all-targets --locked -- -D warnings` — **PASS**.
- `cargo check --workspace --locked` — **PASS**.
- `cargo fmt --all -- --check` — **PASS**.
- `git diff --check` — **PASS**.

## Limitations and gate

No production, release, deployment, email-delivery, or unrelated-container action was performed. The new isolated test establishes only the specific expiry-during-account-lock-wait behavior; broader reset/suspension and security-transition races remain B16b work. Transaction-scoped tests are not described as genuine concurrency evidence. The authenticated `change_password` split remains the B06b/B14 follow-up. B05r remains gated and was not reviewed here.

The corrected B05 source and test evidence satisfy the SEC-03 acceptance criteria. **B05 is approved for source acceptance only; this is not deployment authorization.**
