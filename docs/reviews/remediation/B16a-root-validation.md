# B16a lead validation

Source baseline: `0e38e94`. Worktree: `/tmp/bikesnest-audit-remediation`.
All database checks used the disposable audit service at `127.0.0.1:55439`,
base `bikesnest_test_audit_b15b_20260914`, with `DATABASE_URL` unset.
True races and schema/global-state cases create and destroy owned child databases.
No production database, deployment, policy publication or external provider was used.

## Completed checks

| Suite | Lead result | Independent bounded review |
| --- | --- | --- |
| Parking | 23 passed, 0.40s | 23 passed, 0.56s |
| Photo | 11 passed, 3.24s | 11 passed, 4.05s |
| Privacy | 22 passed, 3.34s | 22 passed sequentially, 12.73s |
| Auth | 37 passed, 5.36s | Final strengthened race: 37 passed sequentially, 7.16s |
| Job | 29 passed, 3.06s | 29 passed sequentially, 9.56s |
| CSP | 1 passed, 0.56s | 1 passed, 0.70s |
| Router scope | 3 passed, 0.37s | Two behavioral cases passed separately, 0.29s/0.31s |
| HTTP | 181 passed in parallel, 5.69s | Full review pending |
| HTTP repeat | 181 passed in parallel, 6.30s | Full review pending |
| HTTP second repeat | 181 passed in parallel, 6.23s | Full review pending |

Focused strict Clippy passed for parking/photo/privacy/auth/job/CSP/router.
Final combined workspace checks are recorded in the implementation handoff.
Times above are test execution, not compilation; shared build-cache waits are
not application performance measurements. Local toolchain is 1.98.1, not proof
of compatibility with the pinned 1.95 CI/container toolchain.

## Corrections and limits

- Final `cargo test --workspace --locked --no-fail-fast --quiet` passed:
  **753 passed, 0 failed, 6 ignored** across 56 reported test/doc-test suites.
  This includes the corrected catalog/renderer contract, final private-pool
  structural guard, and HTTP 181/181 (5.41s). The image-heavy dev-data test took
  75.70s. Ignored browser/visual harnesses were not run by this command.
- The first workspace run found an obsolete email-body `{link}` expectation.
  The corrected catalog test and stronger renderer test now verify the actual
  contract: renderer-owned exact links in both HTML and plaintext, every kind
  and locale. Independent Sol verified the correction and accepted B16a in
  [the full review](B16a-review.md).
- Privacy audit mutation failures use separate savepoints; the privacy-request
  assertion names its exact inserted ID.
- Administrator deletion/revoke and token consumption observe actual PostgreSQL
  lock waiters in owned databases, then release blockers and await bounded pinned
  operations. Competing job claims likewise overlap behind an owned table lock.
- HTTP routers, fixture SQL and repository calls share each test's scoped `Db`.
  Creation helpers now obtain the exact ID from the response `Location`, including
  handling its `?created=1` query, and verify the named row in the same scope.
- The first HTTP run after that helper change had 169 passes/12 failures because
  the helper initially tried to parse the query string as part of the numeric ID.
  This was a test-helper regression, corrected before the passing runs above.
- Router success and intentional-panic cases check seven artifact families from
  the scope and a separate query-only observer, then prove outer rollback and
  surviving-handle invalidation. The third test rejects ordinary adapter pool
  escapes and restoration of removed legacy commit/admin-lock helpers.
- Bounded reviews do not constitute full B16a acceptance. Broader independent
  approval/suspension/reset races and reliability measurements remain B16b;
  integrated release and external evidence remain B19.

The main checkout retains only its pre-existing untracked `docs/reviews/` status.
