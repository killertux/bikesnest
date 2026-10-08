# B12 independent review

Date: 2026-09-14  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `378b448`  
Decision: **PASS**

I read the B12 handoff, remediation-plan acceptance criteria and UX-03 through
UX-08 audit findings, inspected the complete frozen diff except the root-owned
plan ledger, viewed both supplied full-page screenshots, and independently ran
the required native and scripted browser, database-backed HTTP, integration,
unit and strict gates. I changed no application source, plan, generated asset,
fixture, provider, release, deployment or production state.

## Review disposition

No material findings remain.

- At desktop and 390px the published cost, open state, last verification,
  parking type, rating, known positive and negative security facts, and the
  single real map precede gallery and reviews. Unknown security values remain
  accessible in a native disclosure; pending cues for unknown security values
  remain attached there rather than being discarded. The supplied screenshots
  match the executable assertions and show the compact three-link navigation
  without wrapping.
- The gallery exposes one approved preview initially. Its disclosure count is
  explicitly the number loaded, while the existing footer independently says
  how many of the authoritative total were loaded; the bilingual integration
  test covers `2` loaded of `3`, so the UI does not imply that a bounded page is
  the complete gallery.
- Current/History/Pending remain ordinary GET links with `aria-current` rather
  than simulated ARIA tabs. With JavaScript disabled, keyboard activation opens
  the native gallery disclosure and follows History while retaining the selected
  state. The real search browser regression preserves list/map correspondence,
  filters, map state and history/native fallback behavior.
- Proposal next steps use only facts present in the view. `is_own` requires an
  actual `viewer_id`, preventing an anonymous `None == None` ownership result.
  Verified viewers retain other/current voting while own, stale and manual
  proposals remain ineligible; anonymous users receive a local login return URL
  carrying the proposal anchor, and authenticated unverified users receive the
  account-verification next step. Existing stale/manual badges and explanations
  remain truthful; there is deliberately no invented already-voted claim.
- History now describes saved published snapshots and keeps them in native
  disclosures. It neither fabricates missing old versions nor claims a computed
  diff. Proposed values remain separate from published current facts.
- Cycling directions use coordinates with `api=1` and
  `travelmode=bicycling`; OpenStreetMap remains an explicitly labeled location
  fallback. This is URL-shape and local-UI evidence only, not a claim about live
  Google routing, restricted keys or provider availability.
- The B11 contract is preserved: authentication-derived contribution, login and
  moderator state remains independent of the optional Current community read;
  favorites, own-review state, confidence/dispute/recommendation data remain
  reader-derived. The sequential suite retains the authenticated Approvals,
  exact read-count, outage and media-degradation regressions.
- Search production behavior is unchanged. Moving the broad browser harness to
  an owned migrated database removes interference from unrelated committed
  Curitiba rows and is appropriate for its multi-connection server/browser
  lifecycle. Local MapLibre style/media interception proves deterministic UI
  behavior, not external tile availability.

## Screenshot inspection

`/tmp/b12-profile-desktop.png` and `/tmp/b12-profile-mobile.png` were inspected
at original detail. Both are captured at scroll position zero with the gallery
closed. They show the asserted facts → security → map → gallery/reviews order,
two known security signals plus a collapsed unknown-security disclosure, a
settled MapLibre map, one preview, truthful `2 of 2` fixture totals and compact
navigation. The empty review state is consistent with the fixture and is not
presented as production evidence.

## Independent commands and results

```text
git diff --stat 378b448
git diff --name-status 378b448
git diff 378b448 -- <all non-ledger changed paths>
git diff --check 378b448
```

Result: complete source/test/template/i18n/generated-CSS inspection completed;
diff validation passed.

All Cargo database tests below used only the explicit disposable loopback
database, with `DATABASE_URL` removed and the shared debug target directory:

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test profile_browser_test \
  cyclist_profile_in_real_browser --locked -- --ignored --exact --test-threads=1
```

Result: real Axum/Chromium profile scenario **1 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test search_browser_test \
  search_state_in_real_browser --locked -- --ignored --exact --test-threads=1
```

Result: isolated real Axum/Chromium search scenario **1 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test parking_profile_test --locked -- --test-threads=1
```

Result: bilingual/pending/profile integration suite **1 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1
```

Result: sequential HTTP suite **179 passed, 0 failed**.

```text
npm run test:browser
```

Result: browser navigation/map/CSP suite **27 passed, 0 failed**.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --lib --locked
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-web --all-targets --locked -- -D warnings
git diff --check 378b448
```

Result: web unit suite **69 passed, 0 failed**; formatting, strict web Clippy and
final diff validation passed.

B12 satisfies UX-03 through UX-08 within its presentation scope. Live Google
cycling behavior, restricted-key configuration, provider outage behavior and
external tile availability remain B19 evidence and are not claimed here. The
known wider parallel database-fixture inventory remains B16; this review's
required sequential HTTP run was green and does not claim that work completed.
