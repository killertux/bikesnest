# B11 review corrections

The requested media-signing correction is complete.

## Behavior

- A failed primary URL signature for an approved review photo now marks only
  that review's media unavailable. Its review text and the published parking
  facts remain visible, and en/pt-BR localized status copy replaces the former
  silent empty-media result.
- A failed thumbnail signature remains a usable degradation: both gallery and
  review photos explicitly fall back to the successfully signed full image.
- Gallery primary-signing failure continues to mark the gallery unavailable.
- Review-photo fallback alt text is now catalogued in en and pt-BR rather than
  hard-coded.
- Primary review signing failures emit one bounded diagnostic with only the
  allowlisted category `review_media_signing_unavailable`, location id, and
  review id. Storage errors and object keys are not logged.

## Evidence

`detail_media_signing_failures_are_honest_and_thumbnail_fallbacks_remain_usable`
uses the actual router, a populated location/review/photo response, and a local
storage fake that independently fails each of:

- gallery primary;
- gallery thumbnail;
- review primary;
- review thumbnail.

It verifies retained published facts/review text, gallery and review
unavailable states, usable full-image thumbnail fallbacks, English and pt-BR
copy, and that neither the hostile provider marker nor failed key enters the
HTTP response. The production diagnostic call contains no error or key field.

## Commands and results

- Focused media matrix: **1 passed, 0 failed**.
- Sequential full HTTP suite: **178 passed, 0 failed**.
- Browser suite: **27 passed, 0 failed**.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web --all-targets --locked -- -D warnings`: passed.
- `git diff --check`: passed.

One parallel full-HTTP run had the unrelated existing
`review_create_updates_aggregate` return 200 instead of 303. Its immediate
focused retry passed, and the complete sequential 178-test rerun passed. No
migration, configuration, provider, production, asset, release, or deployment
action was required.
