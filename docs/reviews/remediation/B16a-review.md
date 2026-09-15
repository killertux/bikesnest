# B16a independent review

Baseline: `0e38e94`. Review role: independent GPT Sol. Verdict: **PASS**.

## Scope and findings

I reviewed the complete B16a diff, including the ordinary infrastructure
adapters, test-support lifecycle, sequential infrastructure and HTTP fixture
conversions, owned-database race tests, router-scope regression, and the
AGENTS/architecture/testing documentation. I found no remaining material
correctness, isolation, security, or lifecycle defect.

The production adapter changes preserve their operations while routing SQL
through `Db::acquire()`. The only remaining production `.pool()` call is the
private migration-only path in `db.rs`. Retention releases the scoped lease
before object-storage awaits and before recursively invoking the anonymizer.
Repository transactions therefore become savepoints under a scoped test DB
while preserving business operations and atomic outcomes. Report insertion now
deliberately adds an inner transaction/savepoint so an expected constraint
error cannot poison the outer test transaction.

The real router regression observes account, identity, role, verification
token, mail job, session, and audit artifacts inside the scope while an
independent query-only connection sees none. It then proves rollback and saved
`Db` handle invalidation after both normal return and an intentional panic. The
structural case rejects ordinary adapter pool escapes and restoration of the
removed committed-fixture and process-global administrator-lock APIs.

The former intermittent review helper no longer recovers a location by its
non-unique name. It parses the exact response `Location` (excluding
`?created=1`) and verifies that ID and name through the same scoped DB. My full
parallel HTTP run included both formerly colliding `Review Spot` cases and
passed.

Owned child databases are used where independent connections or global state
are essential. The administrator, token-consumption, and job-claim races poll
real PostgreSQL lock waiters, release their blockers, and boundedly await the
same pinned operations. They do not substitute one scoped connection for a
race or leave detached claim/delete futures. Sequential test conversions retain
their substantive assertions while removing commits, cleanup writes, and
process-lifetime locks.

During review I requested correction of the public test-pool helper comment,
which still described the retired HTTP-router escape pattern. It now restricts
that helper to explicit external query-only observers and directs fixtures and
routers to `TestTx::db`. I also verified the late workspace correction replacing
the obsolete B14 `{link}` catalog assumption with the actual renderer-owned-link
contract. No application behavior changed in either correction.

## Independent validation

Every database command below used `DATABASE_URL` unset and only:

`TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit_b15b_20260914`

with `CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target` and `--locked`.

- `cargo test -p bikesnest-infrastructure --test parking_test -- --test-threads=1`
  — 23 passed (0.56s).
- `cargo test -p bikesnest-infrastructure --test photo_test -- --test-threads=1`
  — 11 passed (4.05s).
- `cargo test -p bikesnest-infrastructure --test privacy_test -- --test-threads=1`
  — 22 passed (12.73s).
- `cargo test -p bikesnest-infrastructure --test auth_test -- --test-threads=1`
  — 37 passed (7.16s).
- `cargo test -p bikesnest-infrastructure --test job_test -- --test-threads=1`
  — 29 passed (9.56s).
- `cargo test -p bikesnest-infrastructure --test community_test --test moderation_test --test devdata_test -- --test-threads=1`
  — community 16 passed (0.28s), dev-data 1 passed (92.87s), moderation 11
  passed (0.32s).
- `cargo test -p bikesnest-web --test csp_test -- --test-threads=1`
  — 1 passed (0.70s).
- The two router lifecycle cases were first run independently: 1 passed in
  0.29s and 1 passed in 0.31s. The complete final router suite then passed 3/3
  in 0.36s, including the private-pool structural guard.
- `cargo test -p bikesnest-web --test http_test`
  — 181 passed in parallel (5.48s).
- `cargo test -p bikesnest-web --test observability_test --test search_state_test -- --test-threads=1`
  — 1 and 2 passed respectively (0.28s/0.37s).
- `cargo test -p bikesnest-i18n email_credential_bodies_do_not_embed_action_links`
  — 1 passed.
- `cargo test -p bikesnest-infrastructure --lib email::templates::tests::every_kind_and_locale_has_escaped_complete_alternatives --locked -- --exact`
  — 1 passed. The final assertion requires the exact action link twice in the
  plaintext alternative and twice as an escaped HTML `href` for every locale
  and email kind.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
  — passed on the final reviewed source.
- `cargo fmt --all -- --check` and `git diff --check 0e38e94`
  — passed.

The intentional-panic router test prints its expected panic message while the
test itself passes after checking cleanup. Reported durations are execution
times, not performance measurements; the dev-data run includes real local image
processing and build-lock timing is excluded where Cargo reported it separately.

## Limits

This accepts B16a source and test isolation only. A single scoped connection is
not race evidence. Wider approval/suspension/reset/worker races, measured
parallel reliability, and browser/full-stack gaps remain B16b. The local
Rust/Cargo 1.98.1 checks do not prove the pinned 1.95 CI/container toolchain.
Integrated release, ignored browser/provider suites, deployment, and production
evidence remain B19. No production database, provider, seed command, deployment,
publication, release build, or main-checkout mutation was used in this review.
