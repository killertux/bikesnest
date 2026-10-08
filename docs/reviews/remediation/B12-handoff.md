# B12 handoff — compact cyclist-first profile/search

## Scope delivered

- Reordered the current parking profile so published cost, open status, last
  verification, parking type, security, and the real map precede the gallery
  and reviews at both desktop and 390px. Unknown security attributes are
  available in a native disclosure rather than inflating the first screen.
- Kept the existing current/history/pending native GET links and `aria-current`,
  with profile-scoped compact labels. The pending badge remains the existing
  server-derived count.
- Added truthful contribution next steps only for facts already in the view:
  anonymous sign-in return links (including proposal anchor), authenticated
  unverified account verification, own-proposal exclusion, and existing stale/
  manual-review states. No already-voted assertion was added because that fact
  is not read by this page.
- Made recommendations and saved-history snapshots native disclosures. History
  explicitly describes its coverage as saved published snapshots, without
  inventing diffs or old versions.
- Added Google Maps cycling directions using the documented Maps URL shape
  `https://www.google.com/maps/dir/?api=1&destination=…&travelmode=bicycling`,
  plus the existing OpenStreetMap location fallback. The parameter was checked
  against [Google Maps URLs documentation](https://developers.google.com/maps/documentation/urls/get-started);
  no live Google call was made.
- Reduced search intro/CTA visual weight without changing search form, filter,
  sort, pagination, map numbering, htmx, or B10 map lifecycle contracts.
- Gallery shows one approved preview by default. Its disclosure says “View
  loaded photos (N)”; the existing `Showing N of total` footer remains the
  truthful total boundary, so a paged gallery never claims all photos are
  present.

## Browser evidence

`profile_browser_test` starts a real loopback Axum app against its own migrated
database. It inserts two `APPROVED` photo rows and matching `TestObjectStorage`
keys; the browser intercepts only the test media and an in-process MapLibre
style response. It asserts:

- desktop and 390px vertical order facts → security → map → reviews;
- one visible gallery preview, then the native disclosure exposes its second
  loaded photo;
- a single map, cycling URL with `api=1` and `travelmode=bicycling`;
- no-JavaScript keyboard activation of the gallery disclosure and History link.

Latest captures were taken from scroll position zero with the gallery closed:

- `/tmp/b12-profile-desktop.png`
- `/tmp/b12-profile-mobile.png`

The final 390px capture shows two known security signals, a collapsed unknown-
security disclosure, a settled local-style map, one approved preview, and no
tab wrapping. The fixture has no review, so the intentionally truthful review
empty state remains visible; gallery media is populated evidence, not a
production image claim.

## Validation run

All database commands used only
`TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit`
with `DATABASE_URL` removed and the shared debug target directory.

| Command | Result |
| --- | --- |
| `cargo test -p bikesnest-web --lib --locked` | 69 passed |
| `cargo test -p bikesnest-web --test parking_profile_test --locked` | 1 passed (both locales, honest gallery loaded/total boundary, native profile order, pending-summary fixture) |
| `cargo test -p bikesnest-web --test profile_browser_test --locked -- --ignored --exact cyclist_profile_in_real_browser` | 1 passed (real Axum/Playwright desktop, 390px, populated gallery, native keyboard) |
| `cargo test -p bikesnest-web --test search_browser_test --locked -- --ignored --exact search_state_in_real_browser` | 1 passed (real Axum/Playwright B03 search state/history/native fallback) |
| `cargo test -p bikesnest-web --test http_test --locked` | 179 passed |
| `node --test tests/browser/*.test.cjs` | 27 passed (local loopback rerun after sandbox bind denial) |
| `cargo clippy -p bikesnest-web --all-targets --locked -- -D warnings` | passed |
| `cargo fmt --check` and `git diff --check` | passed |

## Test isolation correction

The broad search browser fixture previously used a shared transaction database.
After the fake geocoder moved `Changed destination` to its fallback Curitiba
coordinates, unrelated committed `parking_facility` rows could make an
intentionally empty-result assertion nonempty. The test now uses
`run_isolated_database_test` and `Db::from_pool`, matching the required
multi-connection/browser test contract. This is a test determinism correction,
not a production search behavior change; B16 should retain the broader
parallel-fixture reliability inventory.

## Remaining external evidence

- The Google Maps URL parameter is documentation-verified only. Restricted key,
  provider outage, and live routing behavior remain B19 external/provider
  evidence, not claims made by this batch.
- Local MapLibre style/media responses support deterministic UI checks only;
  they are not tile-provider availability evidence.

Source is frozen for independent review. No migrations, provider sends,
deployment, production configuration, demo/provenance data, or B13 form work
were performed.
