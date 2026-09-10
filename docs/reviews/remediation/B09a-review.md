# B09a independent review

Date: 2026-09-10  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `513da0b`  
Decision: **PASS**

I read the B09a handoff, plan and audit cache/abuse/proxy criteria, inspected every frozen diff against `513da0b`, and reviewed the installed htmx 4.0.0 source. I used no `.env`, provider, live service, release build, deployment, or production action.

## Correction disposition

No material findings remain. The inert `hx-history="false"` markup and its source-string assertion were removed. Architecture, deployment, and handoff documentation now accurately identify network restoration and absence of a localStorage snapshot cache as a version-dependent property of the pinned and shipped htmx 4.0.0, not an application control. The installed-library browser regression remains as the appropriate upgrade guard and independently passed after correction.

## Confirmed behavior

- Security middleware runs outside downstream handling and overwrites every non-static response with `Cache-Control: private, no-store`, including full pages, fragments, personalized responses, errors, redirects and non-HTML dynamic responses. Only successful `/static/` responses preserve their explicit cache policy; static misses are no-store. The HTTP matrix and shared GET/POST helpers exercise this broadly.
- `AuthService` routes all of its rate-limit gates through `check_sensitive`; `SharedRateLimiter` explicitly forwards that method. Injected/in-memory doubles preserve normal counter semantics through the port default.
- ValKey wraps connection mutex wait, connect/handshake and command execution in one 500 ms deadline. Sensitive checks fail closed regardless of configuration; general checks follow `RATE_LIMIT_FAIL_OPEN`. Refused, silent-connected and buffered RESP command-error cases are bounded. Degradation logs contain only allowlisted reason and policy fields; captured hostile key, endpoint and provider-error markers are absent.
- Trusted-proxy behavior is unchanged and existing HTTP evidence covers spoofed forwarding with zero trusted hops and explicit client selection with configured hops. This does not validate an external proxy/ACL topology, and the docs correctly leave that operational boundary explicit.

## Independent commands and results

Database tests explicitly unset `DATABASE_URL`, used only the disposable loopback audit database, and ran with one test thread.

```text
git diff --stat 513da0b
git diff --name-status 513da0b
git diff 513da0b -- <every changed path>
git diff --check 513da0b
rg -n 'hx-history|history-cache|localStorage|restoreHistory|cacheMiss' \
  node_modules/htmx.org/dist/htmx.js web/static/vendor/htmx.js tests/browser/navigation.test.cjs
```

Result: all changed paths inspected; diff check passed. Installed/shipped htmx contains restoration code but no `hx-history` or localStorage implementation, establishing the finding above.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --lib --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-application --locked
```

Result: infrastructure **101/101** and application **109/109** passed. Infrastructure used only local loopback RESP stubs/refusal, not ValKey or a provider.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1
```

Result: **173 passed, 0 failed**.

```text
npm run test:browser
```

Result: the sandboxed attempt passed the non-server file then failed because loopback bind was denied; the permitted local rerun passed **14/14**, including installed-htmx network restoration/content/localStorage evidence. No external network was used.

After the correction, I reran `npm run test:browser`: **14/14 passed** with the renamed pinned-version behavior guard and no inert attribute. I also reran the exact `dynamic_cache_policy_covers_auth_tokens_privacy_errors_and_redirects` HTTP test against the disposable database: **1/1 passed**.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo check --workspace --all-targets --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web \
  --all-targets --locked -- -D warnings
git diff --check 513da0b
```

Result: formatting, workspace all-target check, strict Clippy and diff validation passed.

B09a satisfies its batch acceptance criteria. External edge topology remains an operational validation outside this source review.
