# B03 independent review — search query and request synchronization

Date: 2026-09-08  
Baseline: `1d7cbab`  
Reviewed state: current uncommitted B03 diff and new files in `/tmp/bikesnest-audit-remediation`  
Implementation: GPT Terra, completed/escalated by separate GPT Sol (`b03_finish`)  
Reviewer: independent GPT Sol (`b03_review`)

## Disposition

**PASS.** All four findings from the first review round are resolved. The corrected implementation and deterministic real-browser evidence satisfy B03's query parsing, latest-intent, clear, history, pagination, and native-fallback acceptance criteria. This approval covers source changes only; nothing was deployed.

## Findings

### Resolved — stale intent protection survives history body replacement

Request ownership and both response guards now live at document lifetime in `web/static/js/app.js`. History restoration and cleanup invalidate prior generations, while the installed htmx 4 request context is still marked at `htmx:config:request` and checked at both `htmx:before:response` and `htmx:after:request` (after the awaited body read).

The corrected browser test wraps the real fetched `Response.text`, suspends an older fragment during body consumption, performs a full htmx history restoration, releases the body, waits for that exact request's `htmx:finally:request`, and verifies URL, results, visible controls, hidden mirrors, and a follow-up request remain restored. The implementer also supplied useful mutation evidence: removing only the document-level `htmx:after:request` guard makes this assertion fail by restoring the stale type; the restored code passes. No vendor asset was modified.

### Resolved — rapid-response test proves the newest response commits

The newest combined type/security response now has its own real response gate and an `htmx:after:settle` barrier. Distinct paginated fixtures make its result set observable. An aborted older request is released first to exercise the installed htmx 4 queue-owner `finally` race; after the newest request commits, the remaining older responses are released adversarially.

The test asserts one sort-only GET and then verifies the final 20-item result page, exclusions distinguishing earlier intents, committed URL, visible filters/sort, OOB hidden mirrors, clear URL, and pagination URL all belong to the newest intent. This closes the earlier request-only proof gap.

### Resolved — changed destination and native fallback use real interactions

The browser journey now changes the destination through the persistent rendered htmx form in the same document, waits for settlement, then exercises both the persistent clear link and the empty-result fragment clear link. It proves the new destination remains committed while type/security/sort and hidden state reset, and a subsequent interaction does not revive old state.

The JavaScript-disabled journey now operates and submits actual rendered checkbox and sort forms, verifies browser-generated repeated/comma-normalized query values, result filtering and checked state, then exercises native clear, pagination, and back navigation. The narrowly scoped `pointer-events-none` addition only prevents the htmx loading overlay from intercepting interaction; it is appropriate and introduces no custom behavior.

### Resolved — malformed query response is localized and non-disclosing

Malformed/duplicate scalar queries now use `search.invalid` from the en/pt-BR catalog through the established `error_page` path. The scoped HTTP regression asserts status, English full-page and Portuguese fragment shapes, localized copy, and absence of the former parser-detail text.

## Positive observations

- Repeated `type` and `security` keys, comma-delimited links, deduplication, UTF-8 form decoding, and strict duplicate known scalar handling are narrowly implemented and covered by four parser tests.
- The scoped HTTP regression uses `tx.db()`, `Db::acquire()`, releases the fixture lease, injects the scoped DB, and relies on outer rollback; it adds no committed cleanup fixture.
- The five authorized search read sites alone move from `Db::pool()` to `Db::acquire()`; no broader adapter migration was found.
- The application uses the exact installed htmx 4 context (`ctx.sourceElement`) and guards both `htmx:before:response` and `htmx:after:request`, matching the installed `Fetch`/awaited-body lifecycle. No vendor customization was introduced.
- One-sort-request behavior, visible/OOB state, clear semantics, pagination, history, and native fallback now have committed-state behavioral evidence through the actual rendered app.

## Validation actually run

All commands ran from `/tmp/bikesnest-audit-remediation`; database commands explicitly removed `DATABASE_URL` and used only the loopback disposable database `bikesnest_test_audit` on port `55439` with debug artifacts under `/home/bruno/Projects/bikenest/target`.

- `cargo test -p bikesnest-web routes::search::tests --locked` with the safe environment: **4 passed**.
- Scoped HTTP test, exact filter, with the safe environment: initial sandbox attempt failed before connection with `Operation not permitted`; rerun with approved loopback access: **1 passed**.
- `cargo test -p bikesnest-web --test search_browser_test --locked -- --ignored --exact search_state_in_real_browser` with approved loopback/Chromium access: **1 passed** in 2.62s.
- `npm run test:browser` with approved local browser access: **13 passed** in 7.32s.
- `git diff --check 1d7cbab`, `node --check tests/browser/search-app.cjs`, `node --check web/static/js/app.js`, and `cargo fmt --all -- --check`: **passed**.

Correction re-review:

- `cargo test -p bikesnest-web routes::search::tests --locked` with the safe environment: **4 passed**.
- Exact scoped localized HTTP regression with approved loopback access: **1 passed** in 0.23s.
- Exact real Axum/Chromium search journey with approved loopback access: **1 passed** in 3.43s.
- `npm run test:browser` with approved local browser access: **13 passed** in 7.36s.
- `cargo clippy -p bikesnest-i18n -p bikesnest-infrastructure -p bikesnest-web --all-targets --locked -- -D warnings`: **passed**.
- `git diff --check 1d7cbab`, both Node syntax checks, and `cargo fmt --all -- --check`: **passed**.
- Lead separately reported the current full HTTP suite **172/172** and the cross-batch real CSRF browser regression **1/1**; these are recorded as lead evidence, not claimed as reviewer-run commands.

I did not independently run the full parallel HTTP suite. Its known legacy `review_create_updates_aggregate` FK flake remains tracked under B16 and is not claimed fixed by the lead's passing run. Nothing was deployed or changed outside this review record.
