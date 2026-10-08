# B11 implementation handoff

## Result

B11 now loads parking-detail collaboration data by selected tab. Every tab gets
only compact pending-proposal field cues and the pending-photo count. Current
loads a bounded gallery and a bounded review/community page; approvals loads a
bounded proposal page; history loads a bounded revision page. Totals are counted
independently of the loaded page and next cursors preserve the selected limit.
Published parking facts remain visible when a collaboration reader fails, while
the affected section renders localized unavailable copy instead of a false zero
or empty state.

The web `DetailReads` facade is a narrow orchestration/test seam which delegates
to the existing application ports; it does not add a persistence/provider port
or move business rules into web. `RouterDeps` can replace only this facade in
tests, allowing actual router requests to count every selected-tab read and
gallery signing operation.

## Notable files

- `crates/application/src/community.rs`, `ports.rs`: compact pending summary,
  required bounded page reads, stable authoritative rating totals.
- `crates/infrastructure/src/community/contribution.rs`: bound SQL cursors,
  independent totals, deterministic order, compact field cues (including
  legacy locations with no initial revision).
- `crates/infrastructure/src/parking/photos.rs`: gallery limit is applied before
  rows leave SQL/signing begins.
- `crates/web/src/routes/details.rs`, `state.rs`, `wiring.rs`: selected-tab
  orchestration, categorized degraded states, injectable read facade.
- templates/i18n: en and pt-BR unavailable, loaded-total, load-more and photo-alt
  copy.
- `ARCHITECTURE.md`, `AGENTS.md`: detail-read lifecycle and seam.

No migration or configuration change was required.

## Evidence

Actual populated-router counters (`detail_tabs_count_actual_reader_calls_through_the_router`):

- Current: gallery 1, pending photos 1, community 1, compact summary 1,
  proposals 0, history 0; storage signing 1.
- Approvals: gallery 0, pending photos 1, community 0, compact summary 1,
  proposals 1, history 0; signing 0.
- History: gallery 0, pending photos 1, community 0, compact summary 1,
  proposals 0, history 1; signing 0.

The application test also records `[summary, proposals, history] = [1,0,0]`
for selected Current versus `[1,1,1]` for an explicitly simulated former eager
orchestration on the same fake fixture. That is a call-count comparison, not a
latency benchmark.

The isolated outage HTTP case measured actual response bodies of 12,991 bytes
for History and 21,868 bytes for degraded Current. These are cross-tab/outage
response sizes, not a before/after performance claim. The test proves detailed
proposal failure affects Approvals but not Current, and malformed review data
affects Current but not History; published facts remain present.

## Validation

- `cargo test -p bikesnest-application --test community_test --locked`: 11 passed.
- loopback DB `cargo test -p bikesnest-infrastructure --test parking_approval_test --locked`: 4 passed.
- loopback DB `cargo test -p bikesnest-web --test http_test --locked`: 177 passed.
- `npm run test:browser`: 27 passed.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy -p bikesnest-application -p bikesnest-infrastructure -p bikesnest-web --all-targets --locked -- -D warnings`: passed.

The first browser attempt was sandbox-blocked at loopback bind (`EPERM`); the
approved loopback-only rerun is the passing result above. An early scoped outage
draft renamed the review table and aborted the scoped transaction; it was
removed. The final fault matrix owns a disposable child database and cleanup.
An initial history fixture also lacked a revision; the final fixture creates an
explicit revision, while the separate infrastructure regression intentionally
covers compact summary behavior for a legacy no-revision location.

## Limits / review focus

- Pagination is cursor-based and bounded to 50; gallery currently uses 24.
- Compact proposal cues expose only a bounded field vocabulary and one proposal
  reference per field. A cue links to an anchor only when that proposal is in
  the loaded approvals page, avoiding misleading page-one anchors.
- Provider/object-store latency was not benchmarked; only actual call counts,
  response bytes, and functional test duration were observed.
