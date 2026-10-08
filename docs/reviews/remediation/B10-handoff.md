# B10 implementation handoff

## Outcome

B10 adds recoverable map and geolocation states and makes full-document htmx
navigation update document metadata and focus deterministically. Fragment swaps
retain their local focus/state contract.

## Implementation

- `navigation.js` carries parsed response metadata on the request-local htmx
  context, then updates `lang`, title, canonical/Open Graph metadata, heading
  focus, and the page-change announcement only after a full body swap. Empty
  optional metadata removes stale head elements.
- Map assets have a finite 10-second load bound. Localized loading/failure/retry
  overlays cover details, search, create, and edit maps. Same-page retry destroys
  and replaces only live map instances, observers, and handlers while preserving
  form/search state.
- Map completion/error callbacks are fenced to their element's current attempt;
  detached and superseded maps cannot change a later page. Search recenter and
  pin-set listeners remain single across repeated retries.
- MapLibre and Mapbox adapters surface SDK error events. Existing provider-neutral
  consumers classify construction, readiness, style/tile, and renderer failures
  into the bounded localized failed state.
- Home, search, and create/edit pin flows announce loading, permission-denied,
  timeout, and unavailable outcomes, request geolocation with a 10-second bound,
  retain typed/address input fallback, and ignore callbacks after page removal.
- The English and Brazilian Portuguese messages live in the shared i18n catalog.
  Architecture/testing documentation records the lifecycle contract.

## Validation

- `npm run test:browser` — **27 passed**. Uses the shipped htmx/Alpine/assets at
  mobile (390px) and desktop sizes. It covers boost/history metadata in both
  languages, empty optional metadata, full-page heading focus/no fragment focus
  transition, the complete GPS outcome matrix, detached callbacks, stalled and
  failed assets, repeated in-place retry/state preservation, stale callbacks,
  observer/listener uniqueness, navigation during dependency loading, and map
  consumer lifecycle. It constructs the actual vendored MapLibre and Mapbox SDKs
  with local styles and observes actual failed local tile requests.
- `for run in 1 2 3; do node --test --test-name-pattern='map retry preserves' tests/browser/navigation.test.cjs || exit 1; done` — **3/3 passed**.
- `env -u DATABASE_URL TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit cargo test -p bikesnest-web --test http_test` — **175 passed** against the dedicated loopback audit database.
- `cargo fmt --all -- --check` — passed.
- `cargo clippy -p bikesnest-web -p bikesnest-i18n --all-targets --all-features -- -D warnings` — passed.
- `git diff --check` and removal search for the superseded metadata globals/debug
  hook — passed.

## Boundaries and remaining external evidence

No migration, dependency, production configuration, provider call, or generated
asset changed. Browser provider failures are local and deterministic. The actual
vendored MapLibre/Mapbox runtimes are covered without credentials; Google Maps
uses the local provider contract stub because a live restricted key and provider
network are intentionally unavailable. A live Google outage/style/tile exercise
therefore remains deployment evidence, not something this handoff claims.
