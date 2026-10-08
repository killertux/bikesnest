# B09b correction handoff

Baseline: `3882b3a`. These uncommitted corrections require independent re-review.

## Correction

- Added the response nonce to all twelve executable runtime/provider/pin-picker
  script tags in the create and edit templates. Inert application/json scripts
  remain nonce-free.
- Added a repository-wide template inventory regression: every executable
  script tag must use `{{ layout.csp_nonce }}`; only application/json is exempt.
- Shared rendered-response assertions now match the report-only header nonce to
  the rendered document and require every executable rendered script to carry
  that exact nonce. Existing verified-user create/edit journeys exercise the
  actual rendered pages.
- The enforcing-CSP browser matrix loads both new and edit page manifests for
  Google, Mapbox and MapLibre and executes the real `pin-picker.js`; clicking
  each map updates the latitude/longitude controls. Provider SDKs remain local
  fakes in those pin-picker journeys, while the separate token-free tests use
  the actual vendored MapLibre/Mapbox runtimes and local empty styles.

No live provider, key, console, edge, production, asset build, release, or
deployment action was performed. Google live-key and Cloudflare edge validation
remain external promotion gates.

## Verification

- `npm run test:browser`: **17 passed**.
- Full disposable-loopback-database HTTP suite: **175 passed**.
- Template nonce inventory is included in the HTTP suite and passed.
- `cargo fmt --all -- --check`, workspace all-target locked check, strict locked
  web Clippy, and `git diff --check 3882b3a`: passed.
