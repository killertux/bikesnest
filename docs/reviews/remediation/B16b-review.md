# B16b independent review

Baseline: accepted B16a checkpoint `a8b6058`. Review role: independent GPT
Sol. Round-one verdict: **CHANGES REQUESTED**. Final verdict after frozen
correction review: **PASS**.

## Scope reviewed

I reviewed the complete B16b worktree diff, including all tracked changes and
the untracked handoff, browser baseline, and response-only mutation helper. The
new password-reset, approval, suspension, and worker cases use
`run_isolated_database_test`, so each owns a uniquely named migrated child
database and real independent pool connections. The lock-sensitive cases pin
their competing futures, observe PostgreSQL lock waits in that child database,
release the blocker, and assert exact rows rather than using one scoped
connection as concurrency evidence.

The reset race proves exactly one credential wins, both reset tokens and the
live session become unusable, and exactly one matching audit and durable notice
are committed. The two approval races force both paths to the shared location
serialization point and verify the winner/loser domain outcomes, proposal vote
counts, supersession, one public version, and one revision. Job workers are
restricted to the test's two kinds inside an owned child database, so they
cannot claim foreign suite jobs. The worker test defers assertions until after
both worker tasks have been cancelled and joined, and its join-timeout branch
aborts and reaps both handles.

The browser mutation helper intercepts only loopback script responses, verifies
that its exact source anchor occurs once, and never rewrites a repository or
served asset. Normal CSRF and search validation ran with
`BIKESNEST_BROWSER_MUTANT` unset. The diff contains no production source,
migration, template, or persistent `web/static` asset change.

## Material findings

1. **The new two-worker regression is not reliable under the required parallel
   load.** A lead full-workspace run failed
   `two_workers_do_not_reclaim_work_behind_a_long_first_handler` at
   `crates/infrastructure/tests/job_test.rs:894`: the three-second poll after
   releasing the handler gate did not observe the blocked row durably reach
   `succeeded` (`the blocked job must durably finish`). That job binary reported
   29 passed and 1 failed. My narrower default-parallel targeted run passed all
   30 job tests, which makes this load-sensitive rather than resolved. B16b's
   acceptance criterion explicitly requires parallel-repeat reliability; a
   focused warm pass cannot override the integrated failure. The timeout is
   converted to `false`, after which both workers are still cancelled and
   joined, so this observation does not show a detached-task cleanup defect.
   It does show that the current three-second completion bound/evidence is not
   robust enough for the suite environment (or that a deeper loaded-worker
   delay still needs diagnosis).

2. **The suspension-first persisted-state evidence is incomplete relative to
   its handoff claim.** The test asserts that `suspend_by_admin` returns `true`
   and directly queries proposal status, location version, revision count,
   vote count, session revocation, reset-token consumption, and suspension
   audit count. Its `blocked_state` query at
   `crates/infrastructure/tests/parking_approval_test.rs:664` does not select the
   user's `account_state`, although the handoff says that the account itself
   shows the committed transition. The complementary vote-first branch does
   directly assert `SUSPENDED`. Add the same direct persisted-state evidence to
   the suspension-first branch and align the handoff with what is actually
   tested.

No other material correctness, isolation, browser-mutation, scope, or
documentation finding was identified in this round.

## Correction review

Both round-one findings are resolved.

The two-worker regression no longer depends on a 120ms lease or guessed sleep.
It uses notification-plus-counter boundaries for both first-worker handlers,
captures the blocked attempt's initial live lease deadline, and waits until
PostgreSQL's own clock has crossed that deadline while the row is still
`running`, still attempt 1, and has a heartbeat-renewed lease more than one
second into the future. Only then does it start worker B. The corrected test
uses a two-second lease, paced database polling with a 15-second observation
budget, a 45-second handler timeout, and a two-second worker shutdown grace.
It records exact running/succeeded state, attempt count, and lease ownership
before release, then exact terminal state/attempt/cleared ownership afterward.
No assertion is evaluated until both worker tasks have been cancelled and
joined; the bounded failure path aborts and reaps both handles.

I independently ran five copies of the corrected worker test simultaneously.
All five passed, with per-test execution times from 2.65s to 3.80s. The complete
affected binaries then passed at default parallelism, including all 30 job
tests. This resolves the integrated-load false failure without weakening the
old-bug shape: worker B is introduced only after the original lease really has
expired in database time and a heartbeat has demonstrably preserved worker A's
ownership.

The suspension-first persisted-state query now joins the exact `users` row and
asserts `account_state='SUSPENDED'` together with the pending proposal, version
1, zero revision, five votes, revoked session, consumed reset credential, and
one suspension audit. The updated handoff states this direct evidence
accurately.

## Independent validation

Every database command unset `DATABASE_URL`, unset
`BIKESNEST_BROWSER_MUTANT`, and used only:

`TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit_b15b_20260914`

with `CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target` and `--locked`.
The first targeted launch was denied loopback access by the sandbox before any
database behavior ran (`Operation not permitted`); the identical permitted
loopback rerun is the validation result below.

- `cargo test -p bikesnest-infrastructure --test auth_test --test job_test
  --test parking_approval_test --locked` at default test parallelism — auth 38
  passed (2.71s), job 30 passed (2.52s), parking approval 7 passed (1.04s);
  6.47s total warm command time.
- Lead integrated `cargo test --workspace --locked` under the same loopback test
  environment — the job binary failed 1 of 30 at the new worker assertion
  above (29 passed, 1 failed). The command stopped on the failure, so this is
  not reported as a complete workspace count or successful runtime baseline.
- `cargo test -p bikesnest-web --test csrf_browser_test --test
  search_browser_test --locked -- --ignored --nocapture` — CSRF 1 passed in
  3.66s; search 1 passed in 4.39s; 8.24s total warm command time.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` — passed in
  17.86s warm command time.
- `cargo check --workspace --all-targets --locked` — passed in 18.59s warm
  command time.
- `cargo fmt --all -- --check`, `node --check` for `mutation.cjs`,
  `csrf-app.cjs`, and `search-app.cjs`, and `git diff --check a8b6058` — passed.
- A path guard over the reviewed diff confirmed no change under production
  domain/application/infrastructure/web/i18n source, migrations, templates, or
  `web/static` assets.

After the frozen corrections, every database command retained the same exact
environment above:

- Five simultaneous processes running `cargo test -p bikesnest-infrastructure
  --test job_test two_workers_do_not_reclaim_work_behind_a_long_first_handler
  --locked -- --exact` — all 5 passed; test execution times 2.65s, 2.90s,
  3.80s, 2.89s, and 2.94s (warm total process times 2.92s, 3.24s, 4.01s,
  3.27s, and 3.20s).
- `cargo test -p bikesnest-infrastructure --test parking_approval_test
  sixth_vote_and_voter_suspension_have_a_consistent_eligibility_boundary
  --locked -- --exact` — 1 passed in 0.83s (1.02s warm command time).
- `cargo test -p bikesnest-infrastructure --test auth_test --test job_test
  --test parking_approval_test --locked` at default parallelism — auth 38
  passed in 2.98s, job 30 passed in 3.54s, parking approval 7 passed in 1.06s;
  7.77s warm command time.
- Lead corrected `cargo test --workspace --locked` under the same isolated
  environment — 758 passed, 0 failed, 6 explicitly ignored; the job binary
  passed 30 in 3.54s and HTTP passed 181 in 3.18s. Exact whole-command wall
  time was not captured and is not estimated here. The six ignored targets
  remain the separately exercised browser/provider cases, not silent passes.
- Final `cargo clippy --workspace --all-targets --locked -- -D warnings` and
  `cargo check --workspace --all-targets --locked` — passed in 0.88s and 0.93s
  warm command time respectively.
- Final `cargo fmt --all -- --check`, all three `node --check` commands,
  `git diff --check a8b6058`, and the no-production-path guard — passed.

The separate browser-baseline record also documents three 181/181 parallel HTTP
runs (5.40s, 5.47s, 5.33s), 28/28 browser fixtures (23.12s), and response-only
CSRF/search mutations that were proven applied and failed the intended
assertions. I inspected that evidence and the mutation mechanism; my normal
browser rerun above independently confirms the non-mutated paths.

## Disposition and limits

B16b passes independent review. The disposable real-connection races, exact
linearization outcomes, foreign-job isolation, failure-safe worker cleanup,
normal rendered browser journeys, response-only mutation evidence, measured
parallel baseline, and test/documentation scope satisfy this batch's acceptance
criteria. This is source/test acceptance only, not deployment.

These checks use local Rust/Cargo 1.98.1 warm debug artifacts; they are not
pinned Rust 1.95/container evidence, request latency, a load-test percentile, or
production evidence. No provider, production database, deployment, seed,
release, generated asset, or main-checkout mutation was used.
