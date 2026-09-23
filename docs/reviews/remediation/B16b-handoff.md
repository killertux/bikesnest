# B16b implementation handoff

Baseline: accepted B16a checkpoint `a8b6058`. This is an implementation
handoff for separate review, not self-approval. B16b changes tests and evidence
only; the final diff contains no production source or migration change.

## Independent database races

All new races create and remove an owned child database through
`run_isolated_database_test`. Operations are pinned in the test future,
observed at real PostgreSQL locks where applicable, bounded by timeouts, and
inspect only their exact rows/kinds.

- Two independently connected password-reset consumers start with different
  live credentials for one account and both reach the account lock. Exactly one
  transition commits: its credential hash wins, both reset credentials become
  unusable, the live session is revoked, and exactly one audit plus one durable
  password-change notice carries the winner's immutable notification identity.
- A sixth vote and moderator approval both wait on the same location. Exactly
  one publishes; the loser returns the specific serialized domain conflict,
  the sibling becomes superseded, and only one version/revision exists.
- Sixth votes on two independently actionable proposals for one location both
  reach the location lock. Exactly one succeeds; the loser observes a version
  conflict, with exact `APPROVED`/six-vote and `SUPERSEDED`/five-vote rows and a
  single public revision.
- In the suspension-first ordering, the sixth vote waits on the location while
  suspension commits. The resumed vote is `NotVerified`, the proposal remains
  pending with exactly five votes and no revision, and the account, session,
  reset credential, and suspension audit all show the committed transition;
  the database assertion reads `users.account_state='SUSPENDED'` directly.
  The complementary vote-first ordering proves a legitimately published
  revision remains history after later suspension. This follows the repository's
  documented final-tally statement-snapshot boundary; it does not demand
  retroactive removal of a previously committed fact.
- The two-worker regression recreates the old failure shape: worker A has a
  two-job capacity and its first handler remains blocked past the short lease.
  Its second job executes concurrently rather than waiting un-heartbeated.
  The test captures that attempt's live lease deadline and starts worker B only
  after PostgreSQL's clock has crossed it while a heartbeat still leaves more
  than one second on the renewed lease. Worker B subsequently executes a unique
  notified probe (proving it actually polled) but never reclaims either
  protected job. Handler counters remain exactly one, attempts/leases/outcomes
  are exact before and after release, and both worker tasks are cancelled and
  joined. No assertion runs while either worker is live; a bounded join timeout
  explicitly aborts and reaps both handles before failing, and a short shutdown
  grace lets each worker cancel and reap owned handler/heartbeat children.

Existing B07/B16a worker tests already cover disjoint claims, expired reclaim,
every stale outcome fence, heartbeat ownership loss, panic/timeout/dead-letter,
outcome-write failures, capacity refill, and forced shutdown. B16b adds only the
missing two-worker long-first integration rather than duplicating those cases.

## Original-bug sensitivity

An executed, temporary mutation removed only the winning reset transaction's
competing-token invalidation. The new race failed as intended because both
consumers returned success (`exactly one reset may commit`). The statement was
restored byte-for-byte, `git diff --exit-code --
crates/infrastructure/src/auth/outbox.rs` passed, and the normal test passed.

Approval and worker sensitivity are reasoned from their deliberately forced
execution shapes, not misreported as executed source mutations: removing the
shared location/version serialization permits multiple publication outcomes,
while the pre-B07 claim-a-batch/process-serially behavior lets the unstarted
second job expire and be reclaimed by the polling worker. The exact assertions
would reject those states.

The lead-owned full-stack evidence is in
[`B16b-browser-baseline.md`](B16b-browser-baseline.md): real CSRF and search
browser journeys pass; two response-only JavaScript mutations were proven
applied and failed their intended assertions; HTTP passed 181/181 in three
8-thread repeats (5.40s, 5.47s, 5.33s; median 5.40s). Those are warm local debug
test execution measurements, not request latency, load percentiles, cold-build,
CI, or production evidence.

## Validation

Every DB command unset `DATABASE_URL` and used only the disposable loopback
base `bikesnest_test_audit_b15b_20260914`; child databases owned each real race.

```text
cargo test -p bikesnest-infrastructure \
  --test auth_test --test job_test --test parking_approval_test \
  --locked
  auth: 38 passed
  job: 30 passed
  parking approval: 7 passed

cargo clippy -p bikesnest-infrastructure \
  --test auth_test --test job_test --test parking_approval_test \
  --locked -- -D warnings
  PASS

cargo fmt --all -- --check
git diff --check
  PASS
```

After the first handoff draft, a lead full-workspace repetition exposed one
two-worker failure in 30 runs: the test-only 120 ms lease required a heartbeat
every 40 ms, so database/runtime contention could cause a genuine expiry and
let worker B consume the blocked handler's sole release permit. This was a test
mechanics defect, not evidence of a production queue defect (the production
default lease is 600 seconds). The corrected test uses a two-second lease and
does not sleep for a guessed multiple: notification-backed handler boundaries
and the database-observed crossed-and-renewed lease establish the ordering.
Seven focused executions passed after correction, including five processes run
simultaneously (individual test times 3.28-6.50s); the full three-binary command
above then passed with the default parallel runner (auth 2.97s, job 3.53s,
approval 1.01s). The focused suspension-first test also passed with the direct
account-state assertion. Targeted clippy, formatting, and diff checks passed.

The focused final reset and two-worker tests also passed after the mutation was
restored and the exceptional worker cleanup path was tightened. During initial
drafting, one probe counter preceded its durable outcome and the assertion
observed `running`; the final test boundedly waits for the persisted `succeeded`
state.
A discarded suspension setup held the same account row needed by a vote's
foreign-key check and therefore made both operations wait; it was interrupted
and replaced by the authoritative location-block/suspension-first ordering
above. Neither draft test issue was a production defect.

## Limits

No live provider, production database, deployment, seed, generated asset,
release, or migration action occurred. These tests establish the selected race
linearization and rendered browser regressions; they do not establish load-test
SLOs or pinned Rust 1.95/container compatibility. Broader integrated release
and external evidence remain B19.
