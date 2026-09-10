# B09a implementation handoff

Baseline: `513da0b`. This batch is uncommitted and requires independent review.

## Outcome

- Security middleware now overwrites `Cache-Control` with `private, no-store`
  on every non-static response: public CSRF-bearing HTML, auth and token pages,
  personalized account/admin/moderation/privacy pages, htmx fragments, errors,
  redirects, and non-HTML dynamic responses. Only successful `/static/...`
  assets preserve their explicit immutable or short cache policy; static
  misses/errors are no-store.
- HTTP cache headers do not control browser-side history snapshots. The pinned
  and shipped htmx 4.0.0 does not implement a localStorage history cache and
  network-restores on back navigation. Installed-library browser evidence
  guards that version-dependent behavior and verifies restored content.
- `RateLimiter` now distinguishes ordinary `check` from explicit
  `check_sensitive`; its default preserves injected fake/in-memory behavior.
  `AuthService` uses the sensitive operation for every registration, login,
  verification-resend, and reset gate. `SharedRateLimiter` forwards the policy
  explicitly; no security behavior is inferred from bucket-key spelling.
- Every ValKey check has a 500 ms total timeout around connection mutex,
  connect/handshake, and Lua command. General checks retain
  `RATE_LIMIT_FAIL_OPEN`; sensitive checks always fail closed. Connect and
  command errors use the same policy. Degradation logs contain only allowlisted
  `reason` (`timeout`/`unavailable`) and `fail_open`; bucket keys, emails, URLs,
  credentials and provider error strings are neither logged nor returned.
- Trusted-proxy behavior is unchanged: zero trusted hops ignores forwarded
  headers, while configured trusted-hop tests retain their existing explicit
  client selection. No edge topology was changed.

No migration, catalog copy, provider action, CSP/MFA work, or deployment was
performed.

## Verification

- Application suites: **109 passed**; auth's explicit sensitive port call
  retains existing enumeration-neutral results and reset hash-failure safety.
- Infrastructure library: **101 passed**. A refused loopback ValKey connection
  proves sensitive closed/general open behavior; connected local RESP stubs
  prove both the total deadline and established-command error policy. The
  command stub parses complete RESP frames rather than TCP chunks, and captured
  tracing output proves hostile key, provider-error and endpoint markers are
  absent. All tests are bounded and provider-free.
- Web HTTP suite: **173 passed**, including public/auth/token/private/error/
  redirect cache matrix, shared authenticated GET/POST and htmx mutation
  no-store assertions, successful hashed/unhashed static caching, static-miss
  no-store, and existing spoofed/trusted forwarded-header cases.
- Complete `tests/browser/*.test.cjs`: **14 passed**, including real installed
  htmx navigation/back behavior with an observed restore network request,
  restored content assertion, and no localStorage history snapshot.
- Final gates: `cargo fmt --all -- --check`, workspace all-target locked check,
  strict locked Clippy for application/infrastructure/web, and
  `git diff --check 513da0b`.

## Review focus and limitations

- Inspect middleware ordering: security remains outside auth/styled errors, so
  short-circuit responses receive the cache policy.
- Inspect `check_sensitive` forwarding through the shared adapter and ValKey's
  single total deadline/policy helper.
- General fail-open is intentionally retained for non-credential workloads;
  credential-sensitive auth sacrifices availability during limiter failure to
  avoid silently disabling brute-force protection.
- Process tests exercise local refused and silent endpoints, not a production
  ValKey or external proxy. Edge ACL/hop configuration remains an operational
  validation outside this source batch.
