# B16a implementation handoff

Baseline: `0e38e94`. This handoff is for independent review; the implementation
is not self-approved or committed.

## Result

B16a moves every ordinary SQL adapter and real HTTP test router onto the same
transaction-scoped `Db::acquire()` seam. The pool accessor is now private to
`Db`; its only production use is the migration runner, which deliberately
detaches a connection before relaxing migration timeouts. Sequential tests seed,
exercise adapters, and assert through `tx.db()` and acquired scoped connections,
so the outer rollback owns every artifact.

The former committed-fixture restart and process-lifetime administrator-lock
APIs were removed. Tests that genuinely need independent transactions, database
global state, DDL, browser processes, or lock waits instead use an owned child
database. No test parks or deletes unrelated global administrators. The known
HTTP review-selection helper now follows the exact created response location
rather than finding a row by a non-unique display value.

Repository changes cover parking search/sitemap and seed paths, photo,
community/moderation/report/audit, privacy/request/retention, jobs and auth test
composition. Retention releases its SQL lease before object-storage calls and
reacquires only for the bounded delete. Report insertion uses an inner
transaction/savepoint so an expected constraint error does not poison the outer
test transaction.

## Behavioral evidence

- Real router scope tests perform registration, login, and account reads inside
  the scoped router; a separate read-only connection cannot see those rows.
  Success and intentional panic both leave zero account/token/job/session and
  related artifacts after rollback, and saved `Db` handles are invalidated.
- Administrator deletion/revocation, auth token consumption, and competing job
  claims use owned databases and observe real PostgreSQL lock waiters before
  releasing blockers and boundedly joining operations.
- The structural router test rejects ordinary infrastructure `.pool()` calls
  and restoration of the removed committed-fixture/process-lock APIs.
- The object-storage reconciliation path does not retain a connection while
  awaiting the provider.

Detailed lead-owned results and bounded slice review are recorded in
[`B16a-root-validation.md`](B16a-root-validation.md). Focused results are:

| Suite | Result |
| --- | --- |
| parking | 23 passed |
| photo | 11 passed |
| privacy | 22 passed |
| auth | 37 passed |
| jobs | 29 passed |
| community | 16 passed |
| moderation | 11 passed |
| dev-data | 1 passed |
| observability | 1 passed |
| search-state | 2 passed |
| CSP | 1 passed |
| router scope/structural guard | 3 passed |
| HTTP | 181 passed; two further parallel repeats also passed 181/181 |
| i18n catalog | 4 passed |
| email renderer exact-link regression | 1 passed |

All database commands used `DATABASE_URL` unset and the disposable loopback
base `bikesnest_test_audit_b15b_20260914`; owned-child helpers created and
removed their own databases for multi-connection/global-state cases. No seed
command, production database, provider, deployment, asset, or release action
was used.

Final static gates:

```text
cargo fmt --all -- --check
  PASS

env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo check --workspace --all-targets --locked
  PASS

env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy --workspace --all-targets --locked -- -D warnings
  PASS

git diff --check
  PASS
```

The local toolchain was Rust/Cargo 1.98.1. These checks are not evidence for the
pinned 1.95 container/CI toolchain; that integrated environment remains B19.

The first full-workspace run exposed an unchanged B14-era catalog test that
still required a `{link}` placeholder even though the renderer now owns link
placement. The correction keeps a meaningful split contract: both credential
catalogs are asserted free of embedded placeholders/URLs, while the renderer
asserts the exact queued action link in both plain-text positions and both
escaped HTML `href` positions for every mail kind and locale. Those focused
catalog and renderer suites pass as recorded above. The lead then started a
fresh `--no-fail-fast` workspace run against the corrected snapshot. It passed
753 tests with 0 failures and 6 ignored tests across 56 reported test/doc-test
suites; ignored browser/provider checks are not claimed. In that run HTTP passed
181/181 in 5.41s, router scope passed 3/3, and dev-data passed in 75.70s.

## Pool-write inventory

There are zero unexplained pooled test writes and zero ordinary production
adapter pool calls.

- `crates/infrastructure/src/db.rs`: the sole production `.pool()` call is
  inside `Db::migrate`; the accessor is private.
- `transaction_scope_test.rs`: `pool().await` is an intentional separate,
  query-only observer proving scoped work is invisible externally.
- `router_scope_test.rs`: the same query-only observer proves real-router
  isolation and rollback.
- `run_isolated_database_test` callers: deliberately independent pools owned by
  disposable child databases for races, DDL/schema upgrades, global-state, and
  real browser process tests.
- Production composition may create a `Db` from its pool, but adapters receive
  only `Db` and acquire through it; demo/fresh-seed commands were not executed.

## Deployment and limitations

There is no migration or runtime behavior change. Deployment requires no data
operation. B16a establishes deterministic sequential isolation; it does not
claim that a single-connection scoped test proves concurrency. Repeated parallel
execution and broader auth/approval reliability work remain B16b. Ignored
browser/provider suites are not implied by the normal workspace test.
