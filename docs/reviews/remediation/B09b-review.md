# B09b independent review

Date: 2026-09-14  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `3882b3a`  
Decision: **PASS**

I read the B09b handoff, plan/audit criteria, current architecture/testing guidance, the complete frozen diff, and the actual pinned/shipped htmx 4 source. I did not use real keys, providers, consoles, live edge systems, release builds, generated assets, deployment, or production state.

## Correction disposition

No material findings remain. All twelve create/edit runtime, provider and pin-picker tags now carry `{{ layout.csp_nonce }}`. A repository-wide inventory permits only inert application/json scripts without it; my independent inventory scan found exactly the two intended JSON tags and no executable omission. Rendered-response assertions bind every executable tag to the exact report-only header nonce, and enforcing browser journeys cover create/edit pin-picker behavior across provider profiles while separate tests retain actual local MapLibre/Mapbox runtime coverage.

## Confirmed behavior

- Nonce generation occurs in security middleware and is passed through a typed request extension into auth/PageLayout. The implementation does not rewrite arbitrary HTML bodies, accept client nonce headers, or set htmx `inlineScriptNonce`; streaming/body bytes remain untouched.
- The installed htmx builds response fragments before `htmx:before:swap`; the navigation listener removes executable scripts from the entire fragment before insertion while preserving `type="application/json"`. Current browser fixtures cover ordinary/root/OOB malicious response content, but not the omitted add/edit template scripts above.
- Navigation and Google loaders capture only their original trusted document script nonce. Map manifest assets are constrained to exact same-origin application/vendor families without query or fragment; swapped manifest nonce/src input cannot authorize a remote or injected script.
- The enforced baseline remains provider-compatible. The report-only candidate removes script `unsafe-inline`, is eval-free for MapLibre/Mapbox, and retains the documented Google-only `unsafe-eval` plus style-inline compatibility. Actual local vendored MapLibre/Mapbox map+worker evidence is meaningful; Google remains a stub/documentation check. Restricted-key Google console behavior and Cloudflare parser-inserted beacon policy remain explicit B19/external gates.

## Commands and results

```text
git diff --stat 3882b3a
git diff --name-status 3882b3a
git diff 3882b3a -- <every changed path>
git diff --check 3882b3a
rg -n --pcre2 '<script(?![^>]*\\bnonce=)' templates --glob '*.html'
```

Result: diff check passed. The inventory found only intended inert JSON plus the 12 missing executable script tags identified above.

After correction I independently ran:

```text
npm run test:browser
```

Result: **17 passed, 0 failed**, including enforced candidate gadget rejection, actual vendored MapLibre/Mapbox local map loads, and create/edit pin-picker journeys.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1
```

Result: **175 passed, 0 failed**, including the repository inventory and exact rendered nonce/header assertions.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web security::tests --locked
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-web --all-targets --locked -- -D warnings
git diff --check 3882b3a
```

Result: security policy tests **11 passed**, formatting, strict web Clippy and diff validation passed. The browser used only its local fixture; HTTP used only the explicit disposable database.

B09b satisfies source acceptance. Live restricted-key Google validation and the Cloudflare edge-beacon decision remain external promotion gates; this PASS does not authorize rollout.
