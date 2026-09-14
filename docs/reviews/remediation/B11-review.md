# B11 independent review

Date: 2026-09-14  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `1ef6a1f`  
Decision: **PASS**

I read the B11 handoff, plan/HX-06 criteria and current architecture/testing guidance, inspected the complete frozen diff except lead ledger edits, and independently executed the required gates. I changed no source, plan, provider, generated asset, release, deployment or production state.

## Reopened authentication-state gate

The earlier PASS was reopened after checkpoint `ce0aa52` because Approvals and
History no longer load the Current-tab community model, while the base page
builder defaulted `can_contribute`, `is_authenticated`, and `is_moderator` to
false. That made eligible proposal votes and shared Suggest, Report/modal, and
moderator controls disappear on those tabs. The follow-up correction is
independently **PASS**.

`DetailsPage::build` now derives only those three request-authoritative values
from `Auth`, before any optional community read and before proposal view models
compute `can_vote`. The community overlay still owns favorite state, own-review
state/rating, confidence/dispute data, parked-here count, recommendation reasons,
and reviews. `viewer_id` remains base-derived, so an eligible viewer can vote on
another user's current proposal but not their own; stale/unknown proposals and
unverified or anonymous viewers remain blocked.

The actual production-router regression also confirms that verified Approvals
and History render Suggest and Report/modal controls, the moderator sees the
pending-photo moderation link, and a Current community-reader outage retains
verified auth controls. Its exact facade counts prove the correction adds no
reader calls: Approvals `[0,1,0,1,1,0]`, History `[0,1,0,1,0,1]`, and Current
outage `[1,1,1,1,0,0]` for gallery, pending photos, community, summary,
proposals, and history respectively.

## Correction disposition

No material findings remain. Primary review-photo signing failure now marks only that review's `media_available` false, preserves the review text and published parking facts, and renders localized English/Portuguese unavailable copy. Its diagnostic contains only the allowlisted category, location id and review id—no provider error or object key. Gallery primary failure remains unavailable; gallery and review thumbnail failures consistently fall back to the successfully signed full image. The actual-router matrix populates all media and independently exercises all four branches plus Portuguese copy.

## Confirmed behavior

- The narrow `DetailReads` facade delegates to existing application ports and is only an orchestration/test seam. The populated actual-router counter test creates a real location, replaces only this facade/storage, sends all three route requests, and proves exact per-tab reads and signing counts.
- Every tab retains compact pending proposal cues and pending-photo count. SQL totals are independent, pages clamp to 1..50, use `limit + 1`, deterministic descending keyset cursors and preserve the requested limit in links. Gallery is bounded to 24 before signing. The legacy no-revision fallback builds compact current facts without requiring a migration.
- Current alone loads gallery/community; Approvals alone loads proposal detail; History alone loads revisions. Published parking facts survive categorized reader outages, and the affected sections show localized unavailable copy rather than false zero/empty states.
- Review pagination does not recompute the authoritative rating from its subset; recommendation uses the aggregate rating already on the location. Proposal/history totals and `has_more` are not inferred from page length after truncation.
- Existing proposal/vote publication rules are preserved; B11 does not redesign approval or public-fact writes. Reported call counts and outage response bytes are honestly labeled functional evidence, not SQL/latency or before/after benchmarks.

## Independent commands and results

```text
git diff --stat 1ef6a1f
git diff --name-status 1ef6a1f
git diff 1ef6a1f -- <all changed paths>
git diff --check 1ef6a1f
```

Result: complete changed-path inspection and diff validation passed.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-application --test community_test --locked
```

Result: **11 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test parking_approval_test --locked -- --test-threads=1
```

Result: **4 passed, 0 failed**.

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1
npm run test:browser
```

Result before correction: HTTP **177 passed** and browser **27 passed**. After correction, the exact `detail_media_signing_failures_are_honest_and_thumbnail_fallbacks_remain_usable` matrix passed **1/1**, the full sequential HTTP suite passed **178/178**, and browser passed **27/27**.

Authentication-state follow-up validation against `ce0aa52`:

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test \
  approvals_derive_eligibility_from_auth_without_loading_current_tab_data \
  --locked -- --exact --test-threads=1
```

Result: **1 passed, 0 failed** (178 filtered out).

```text
env -u DATABASE_URL \
  TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit \
  CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test http_test --locked -- --test-threads=1
npm run test:browser
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-web --all-targets --locked -- -D warnings
git diff --check ce0aa52
```

Result: sequential HTTP **179 passed, 0 failed**; browser **27 passed, 0
failed**; formatting, strict web Clippy, and follow-up diff validation passed.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web \
  --all-targets --locked -- -D warnings
git diff --check 1ef6a1f
```

Result: formatting, strict Clippy and final diff validation passed.

B11 satisfies its source/test acceptance criteria. The known parallel `review_create_updates_aggregate` fixture flake remains B16 work; this review's required sequential suite was green and does not claim that flake fixed.
