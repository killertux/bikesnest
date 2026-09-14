# B13 handoff — contribution forms and accessible recovery

## Scope delivered

- Paid currency, amount and unit controls are progressively shown only for a
  paid selection. Free and unknown choices keep them collapsed with scripting,
  while the native controls remain reachable when JavaScript is disabled.
- Hours and tri-state security are native, keyboard-operable disclosures.
  Previously entered non-unknown values and validation errors reopen the
  relevant optional group, without changing the submitted unknown/yes/no or
  opening-hours domain semantics.
- Rejected add/edit forms retain all submitted values and associate known
  errors with their controls. Advanced coordinate/timezone errors reopen their
  disclosure; the timezone override now has the same localized field-level
  error markup as latitude and longitude.
- Full-document htmx rejection focus is stored on the exact request context and
  applied at `htmx:finally:swap`, after htmx 4's boosted `show:top` scroll. The
  element must still belong to the current body and remain the current marked
  error. The reference is consumed once, so fragments and later navigations
  cannot claim stale focus.
- The parking-photo file input and optional description have explicit visible
  labels and guidance in English and pt-BR.
- Existing edit submissions still create a pending `edit_details` proposal;
  the public parking row is not changed before approval.

## Actual browser and persistence evidence

The ignored contribution regression runs a real loopback Axum router against a
fresh database created by `run_isolated_database_test`, with local fake email,
storage, limiter and MapLibre style responses. It exercises:

- 390px create flow across unknown, paid and free cost choices;
- invalid currency, overlapping optional hours and invalid timezone, with the
  correction visible and focused after each boosted full-body response;
- retained name, description, coordinates, Monday-hours choice and CCTV “no”;
- keyboard operation of the visually styled security radio;
- corrected final creation through the normal 303 route;
- desktop edit of cost, hours and security through a normal 303 proposal;
- JavaScript-disabled paid controls and keyboard-opened native Hours details.

After Chromium exits, the Rust test queries that same owned database. It proves
the new location persisted as free with its retained description, timezone and
CCTV “no”; the existing location remained free at version 1; and the browser
edit created one pending `edit_details` payload containing the proposed paid
500-cent cost and CCTV “no”. This is behavioral approval-invariant evidence,
not only redirect or rendered-string evidence.

The first inherited harness run reached the edit page but attempted to click a
screen-reader-only radio underneath its styled label. The journey now uses the
control's keyboard surface. A later run exposed missing timezone error markup,
which was corrected. An immediate visibility assertion after Alpine selection
was also changed to wait for Alpine's rendered state rather than racing its
next update. No arbitrary delay was introduced.

## Validation

All database-backed commands removed `DATABASE_URL`, used only
`TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit`,
and used `CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target`.

| Command | Result |
| --- | --- |
| `cargo test -p bikesnest-web --test contribution_browser_test --locked -- --ignored --nocapture` | 1 passed; real authenticated create/edit/native journey plus database assertions |
| `cargo test -p bikesnest-web --lib --locked` | 69 passed |
| `cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1` | 180 passed; includes form errors, pin/address, labels, CSRF, CSP and approval behavior |
| `npm run test:browser` | 28 passed; initial sandbox loopback denial executed no navigation checks, permitted rerun passed |
| `cargo test -p bikesnest-web --test profile_browser_test --locked -- --ignored` | 1 passed |
| `cargo test -p bikesnest-web --test search_browser_test --locked -- --ignored` | 1 passed |
| `cargo test -p bikesnest-web --test csrf_browser_test --locked -- --ignored` | 1 passed |
| `cargo check --workspace --all-targets --locked` | passed |
| `cargo clippy -p bikesnest-web --all-targets --locked -- -D warnings` | passed |
| `npx tailwindcss -i web/static/css/input.css -o /tmp/bikesnest-b13-css.lcLHMc/app.css --minify` | passed; disposable output only, no committed asset rebuilt |
| `cargo fmt --all -- --check`, `node --check tests/browser/contribution-app.cjs`, `git diff --check` | passed |

No migration, provider call, production data/configuration, generated committed
asset, release build, deployment or external action was performed. Source is
frozen for independent review; B14 is not included.
