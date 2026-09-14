# B13 independent review

Date: 2026-09-14  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `08af3fd`  
Decision: **PASS**

I read the B13 handoff, remediation-plan acceptance criteria and UX-09 audit
finding, inspected the entire frozen diff except the root-owned plan ledger, and
independently executed the authenticated real-browser/persistence journey and
the relevant HTTP, browser, unit and strict gates. I changed no application
source, plan, generated asset, provider, release, deployment or production
state.

## Review disposition

No material findings remain.

- Paid currency, amount and unit fields are Alpine-disclosed only for `paid`.
  Switching to `free` or `unknown` collapses but does not erase entered values;
  without JavaScript the native controls remain visible, named and submittable.
  Server/domain parsing remains authoritative for whether those values apply.
- Hours and security use native `details`/`fieldset` controls. Stored or
  rejected non-unknown values reopen their groups; hours validation reopens and
  focuses the affected day. Tri-state radios remain actual named controls, and
  the real browser journey selects definitive CCTV `no` with the keyboard and
  proves that value persisted.
- Rejected create/edit forms retain the submitted name, description,
  coordinates, cost values, hours and security. Coordinate/timezone inputs
  remain in Advanced; latitude, longitude and timezone errors reopen it and use
  localized field-level error relationships. The corrected timezone branch is
  exercised through the production router and browser.
- Full-body htmx rejection focus is stored on the exact request context after
  swap and applied at `htmx:finally:swap`, after htmx 4's boosted `show:top`.
  It is consumed once and requires the body target, a connected element and the
  still-current error marker. The browser regression proves visible focus after
  the final scroll, proves an unrelated fragment cannot claim it, and proves a
  later error-free navigation resumes ordinary heading focus. This introduces
  no global mutable request state or async lock.
- The harness uses actual 400 rejection and 303 success responses. It waits for
  Alpine's rendered visibility, operates the styled radio through its keyboard
  surface, and includes the formerly missing timezone markup; it no longer
  relies on the rejected redirect, hidden-control click or immediate-render
  assumptions described in the handoff.
- The authenticated 390px create flow corrects currency, overlapping hours and
  timezone errors before successful creation. Its desktop edit follows the
  normal proposal route. Post-browser SQL assertions prove the new location is
  free with retained description/timezone and CCTV `no`, while the pre-existing
  public location remains free at version 1 and receives exactly a pending
  `edit_details` payload proposing paid 500-cent cost and CCTV `no`. Existing
  facts therefore remain approval-gated rather than being published directly.
- Parking-photo upload now has explicit visible bilingual labels for the file
  and optional description, with the latter linked to concise guidance.
  Existing multipart/CSRF, verified-contributor and moderation-held behavior is
  preserved by the HTTP and real CSRF browser gates.

## Independent commands and results

```text
git diff --stat 08af3fd
git diff --name-status 08af3fd
git status --short
git diff 08af3fd -- <all non-ledger changed paths>
git diff --check 08af3fd
```

Result: complete tracked and untracked source/test/handoff inventory inspected;
diff validation passed.

All database-backed Cargo commands below removed `DATABASE_URL`, used only the
explicit disposable loopback database, and shared the debug target directory:

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test contribution_browser_test \
  contribution_forms_in_real_browser --locked -- \
  --ignored --exact --test-threads=1 --nocapture
```

Result: authenticated create/edit/native browser and persistence journey
**1 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1
```

Result: sequential HTTP suite **180 passed, 0 failed**.

```text
npm run test:browser
```

Result: browser navigation/map/CSP/form-focus suite **28 passed, 0 failed**.

```text
env -u DATABASE_URL TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test profile_browser_test cyclist_profile_in_real_browser --locked -- --ignored --exact --test-threads=1
env -u DATABASE_URL TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test search_browser_test search_state_in_real_browser --locked -- --ignored --exact --test-threads=1
env -u DATABASE_URL TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test csrf_browser_test --locked -- --ignored --test-threads=1
```

Result: retained real profile **1/1**, search **1/1**, and CSRF **1/1** browser
regressions passed.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --lib --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo check --workspace --all-targets --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-web --all-targets --locked -- -D warnings
cargo fmt --all -- --check
node --check tests/browser/contribution-app.cjs
git diff --check 08af3fd
```

Result: web unit suite **69 passed, 0 failed**; workspace check, strict web
Clippy, formatting, browser-driver syntax and final diff validation passed.

B13 satisfies UX-09 and its authenticated/native acceptance journey without
weakening proposal approval, CSRF, CSP or moderation boundaries. No committed
asset or release build was produced. The known wider parallel database-fixture
inventory remains B16; this required sequential HTTP suite was green and does
not claim that work completed.
