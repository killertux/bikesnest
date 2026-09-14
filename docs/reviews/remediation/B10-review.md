# B10 independent review

Date: 2026-09-14  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `9887acb`  
Decision: **PASS**

I read the B10 handoff and plan criteria, current architecture/testing guidance, and inspected the complete frozen diff except the lead's ledger-only edits. I did not edit source, load `.env`, call providers, rebuild assets/releases, deploy, or touch production.

## Findings and acceptance

No material findings remain.

- Full-body htmx swaps parse request-local response metadata before swap and apply language, title, canonical/Open Graph metadata, heading focus and the localized announcement only after the body swap. Empty optional metadata removes stale head nodes. Fragment swaps do not carry this full-document transition or steal focus. History restoration exercises the same installed-htmx network path.
- Map asset admission has a finite ten-second failure bound and removes failed cache entries/elements so later attempts can load again. Loading/failure/retry UI is localized across search, details, create and edit. Retry destroys and unregisters only live map instances/observers, fences callbacks by element connectivity and attempt identity, and preserves form/filter/pin state. Search recenter and pin-set listeners remain single across retries.
- Provider construction, readiness and renderer/style/tile errors converge on the bounded failed state. Actual vendored MapLibre and Mapbox runtimes and blob workers load with local empty styles under the CSP candidate, and deterministic local tile failures surface. Google remains a provider-contract stub; no live-key compatibility or outage claim is made.
- Home, search and pin GPS flows request a finite ten-second browser timeout, distinguish loading/denied/timeout/unavailable in both locales, retain manual address/coordinate and native-keyboard paths, and ignore detached/superseded callbacks.
- B09a/B09b cache, nonce, script-stripping and manifest allowlist invariants remain intact. No generated asset, provider configuration, migration or dependency changed.

## Independent commands and results

```text
git diff --stat 9887acb
git diff --name-status 9887acb
git diff 9887acb -- <all changed paths>
git diff --check 9887acb
npm run test:browser
```

Result: all changed paths inspected; diff check passed; browser suite **27 passed, 0 failed**. This included full-body metadata/focus versus fragments, all GPS outcomes and detached callbacks, finite stalled assets, SDK/renderer failure, repeated retry and state/listener preservation, stale callback fencing, enforcing CSP, and actual local MapLibre/Mapbox load/tile-error cases.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1
```

Result: **175 passed, 0 failed** against only the disposable loopback database.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo check --workspace --all-targets --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-web -p bikesnest-i18n --all-targets --all-features \
  --locked -- -D warnings
git diff --check 9887acb
```

Result: formatting, workspace all-target check, strict Clippy and final diff validation passed.

B10 satisfies source/test acceptance. Live restricted-key Google failure behavior remains external deployment evidence and this PASS does not authorize release or deployment.
