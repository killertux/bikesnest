# B05r independent review — administrator account-state transitions

- Baseline: `a159dc3`
- Reviewed state: uncommitted B05r diff on `fix/audit-remediation`
- Implementer: GPT Sol (`b05r_implement`)
- Reviewer: GPT Sol (`b05r_review`)
- Scope: administrator suspension/restoration eligibility, atomic revocation/audit, and deletion-state races
- Disposition: **PASS after re-review**

## Acceptance review

The production service still performs the administrator-role authorization before calling the repository. The revised repository port returns whether an eligible transition occurred, and the service intentionally treats `false` as a successful no-op. The application fake matches that eligibility contract: suspension accepts only active or pending-verification accounts, restoration accepts only suspended accounts, verified restoration becomes active, unverified restoration becomes pending verification, and only actual transitions append an audit event.

The PostgreSQL suspension implementation obtains the account row lock, rechecks an authoritative active/pending predicate, and performs state change, live-session revocation, unused verification-token revocation, unused reset-token revocation, and the exact administrator audit insert in one transaction. Restoration uses a conditional update restricted to suspended rows and derives active versus pending state from `email_verified_at`; its state change, suspension-timestamp clearing, and audit insert are one transaction. Neither method can restore or overwrite a committed deleted state.

The isolated race test is genuine multi-connection lock-wait evidence for its deliberately narrow claim. In a uniquely created database, a direct deletion-state `UPDATE` holds the users-row lock while each administrator operation starts independently; the test observes a lock wait, commits deletion, and proves the waiter reports no transition. This correctly does not claim to exercise the full privacy anonymization workflow.

The existing native/admin route contract did not regress in the focused or full HTTP runs. The B01 and B05 guards also remained green in the full workspace run. The known intermittent legacy review foreign-key failure was not observed and remains a B16 concern, not a B05r fix.

## Finding

### P1 — persisted complete no-change/rollback evidence omits part of the transition

The new repository contract explicitly makes suspension state, three revocation classes, and audit one atomic transition, but its negative-path tests do not prove the complete persisted set.

- `administrator_transition_audit_failure_rolls_back_state_and_revocations` seeds and checks a session and password-reset token, but does not seed/check an email-verification token. It also does not assert that `suspended_at` and `updated_at` roll back after the failed suspension. Its restoration failure checks only `account_state`, so it does not prove rollback of the attempted `suspended_at = NULL` or `updated_at` change.
- `committed_deletion_wins_against_waiting_admin_transitions` similarly proves session/reset preservation and zero audit, but does not prove preservation of an unused email-verification token. It checks only the final enum state, not that deletion timestamps/account timestamps are untouched by the waiting no-op.
- The state/no-op matrix proves enum outcomes and exact successful audit rows, but does not snapshot persisted timestamps or security artifacts around the deleted, missing, already-suspended, active-restore, and pending-restore no-op cases.

The SQL transaction structure appears correct, but the batch acceptance criteria specifically require complete no-change and rollback evidence. The omitted verification-token and timestamp columns are fields this implementation itself mutates, so source inspection is not a substitute for the required behavioral proof.

Required correction: extend the scoped PostgreSQL regressions to snapshot and assert every persisted field/artifact affected by each operation. At minimum, seed an unused email-verification token alongside the reset token/session and prove it remains usable or unchanged after injected audit failure and after deletion wins the lock race; assert `account_state`, `suspended_at`, and `updated_at` rollback/preservation for failed/no-op transitions; and assert restoration audit failure preserves its pre-existing non-null suspension timestamp and prior `updated_at`. Keep the isolated race claim narrow and do not broaden it into full anonymization or the deferred B06b/B14/B16 work.

## Re-review status

The corrected tests resolve almost all of the finding. Distinct timestamp sentinels now prove eligible-state no-ops preserve `account_state`, `suspended_at`, and `updated_at`; injected suspension-audit failure preserves the complete state tuple, live session, unused verification token, and reset token; and injected restoration-audit failure preserves the exact post-suspension tuple. The real deletion lock-wait test now also preserves the state/suspension/update tuple, session, unused verification token, reset token, and zero audit for both operations.

The final correction closes the remaining omission. `persisted_account_state` now includes `deleted_at`, so every before/after tuple comparison covers account state plus suspension, deletion, and update timestamps. The deletion race additionally asserts the exact `2004-05-06T07:08:09Z` deletion sentinel alongside its exact suspension and update sentinels after each waiting administrator operation completes. The complete session, verification-token, reset-token, audit, and timestamp non-change set is now exercised. No review finding remains.

## Commands actually run

All Cargo commands ran in `/tmp/bikesnest-audit-remediation` with `env -u DATABASE_URL`, `TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit`, `CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target`, debug profiles, and `--locked`.

- `cargo test -p bikesnest-application --test auth_test --locked` — **PASS**, 30/30.
- `cargo test -p bikesnest-infrastructure --test auth_test --locked` — initial sandbox attempt could not open loopback (`Operation not permitted`); approved rerun against the dedicated audit database **PASS**, 23/23, including the observed isolated deletion lock-wait test.
- `cargo test -p bikesnest-web --test http_test admin_suspend_revokes_sessions_blocks_and_restore --locked` — **PASS**, 1/1.
- `cargo test -p bikesnest-web --test http_test --locked` — **PASS**, 172/172.
- `cargo test --workspace --locked` — **PASS**, 670 passed, 0 failed, 2 explicit real-browser tests ignored by their harness requirements. Its HTTP suite passed 172/172; the legacy B16 foreign-key flake was not observed.
- `cargo clippy -p bikesnest-application --all-targets --locked -- -D warnings` — **PASS**.
- `cargo clippy -p bikesnest-infrastructure --all-targets --locked -- -D warnings` — **PASS**.
- `cargo clippy -p bikesnest-web --all-targets --locked -- -D warnings` — **PASS**.
- `cargo check --workspace --locked` — **PASS**.
- `cargo fmt --all -- --check` — **PASS**.
- `git diff --check a159dc3` — **PASS**.

Focused correction re-review:

- `cargo test -p bikesnest-infrastructure --test auth_test --locked` — **PASS**, 23/23, including the strengthened rollback/no-op matrix and both observed isolated deletion lock waits.
- The previous full workspace run was not repeated because the correction changed tests only.

Final bounded re-review:

- `cargo test -p bikesnest-infrastructure --test auth_test --locked` — **PASS**, 23/23, including both observed isolated deletion lock waits with the complete persisted timestamp tuple.
- `cargo fmt --all -- --check` — **PASS**.
- `git diff --check a159dc3` — **PASS**.
- The previous successful full workspace, HTTP, and strict Clippy runs were not repeated for this one test-helper tuple/assertion change.

## Limitations and gate

No production, release, deployment, external-provider, email-delivery, or shared-cleanup action was performed. The race test was reviewed only as a direct deletion-state update competing with administrator transitions; full anonymization integration and broader security-transition races are not claimed. Authenticated `change_password` work remains deferred to B06b/B14, and broader race work remains B16.

The implementation and corrected evidence satisfy the administrator account-transition acceptance criteria. **B05r is approved for source acceptance only; this is not deployment authorization.**
