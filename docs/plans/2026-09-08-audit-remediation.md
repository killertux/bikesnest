# BikesNest audit remediation plan

Date: 2026-09-08. Baseline: `7aa243c`. Working branch: `fix/audit-remediation`, isolated worktree `/tmp/bikesnest-audit-remediation`.

Source: [full review](../reviews/2026-09-08-app-review.md). This is the execution ledger, not a replacement for the audit evidence.

## Scope and decisions

- Fix the audit's engineering/product defects and factual documentation drift, preserving the modular monolith and approval invariants.
- **Excluded by the user:** synthetic reviews/demo provenance (`PROD-02`, `UX-10`). The data is intentionally present for validation. Do not delete, relabel, reseed, change its ratings, or otherwise reconcile it in this program.
- Preserve all existing user changes. Work outside the live main checkout: production serves its static files directly.
- No production writes, deployments, restarts, policy seeding, external emails, provider-console changes, real retention runs, notification campaigns, or PR publication without separate authorization.
- New sequential DB tests use transaction injection and automatic rollback. Real multi-connection/worker/browser tests use an explicitly disposable isolated database; no cleanup of user databases.
- New migrations are forward-only. Document data/backfill/rollback implications; never edit an applied migration.
- User-facing text stays in en + pt-BR catalogs. Keep native HTML fallback, unknown states, and separation of published/proposed facts.
- Legal applicability, contracts, DNS/edge operations and restore drills require evidence/authority outside source code. Prepare implementation/runbooks and list pending decisions honestly; do not mark those obligations satisfied merely because documents were edited.

## Execution and independent review protocol

One batch at a time; no implementation of the next batch before the current gate passes.

1. Lead assigns a bounded batch with acceptance criteria, ownership and allowed resources.
2. **GPT Terra** implements straightforward/local changes; **GPT Sol** implements security, transaction, concurrency or multi-layer changes. Escalate a Terra batch to Sol if complexity exceeds its scope. No model is asked to approve its own implementation.
3. Implementation agent records files, tests actually run, outcomes, residual risks, migrations and deployment considerations. Lead checks scope and prepares a stable diff.
4. Spawn a **separate GPT Sol reviewer** to inspect the diff independently, run targeted validation and check regressions/security/invariants. Reviewer does not implement the next batch.
5. Any correctness, security, untested acceptance criterion or unrelated-change finding returns to implementation. Reviewer rechecks corrections. A missing test environment is not a pass.
6. Lead accepts only when required checks pass and material findings are resolved. Save a review record under `docs/reviews/remediation/`, update this ledger, and checkpoint a local commit.
7. Advance to the next queued batch. Operational/legal gates can be marked `awaiting-authority/evidence` with concrete owner/input; do not pretend they are fixed or block unrelated safe implementation.

Statuses: `queued`, `implementing`, `reviewing`, `changes-requested`, `accepted`, `awaiting-authority/evidence`. Accepted means reviewed code, **not deployed**.

## Ordered batches and acceptance criteria

| Batch | Implementation model | Scope / audit mapping | Required acceptance evidence | Status |
|---|---|---|---|---|
| B00 | Terra | Safe DB-test target selection; truthful quick commands (`ENG-02`) | Missing/invalid/non-test targets fail before connecting/migrating without leaking URLs; explicit isolated target works; no `DATABASE_URL` fallback; CI/docs updated; pure guard tests | accepted |
| B01 | Sol | Suspension-safe verification and token invalidation (`SEC-01`) | Suspended/deleted cannot reactivate; legitimate initial verification and active email change work; repository predicate handles races; old verification tokens cannot bypass suspension | accepted |
| B02 | Sol | CSRF token lifecycle and usable expiry handling (`HX-01/02/03`) | Real middleware/browser matrix: cold/boosted forms, invalid→valid retry, login/logout, two tabs, history, expiry, multipart; invalid tokens still fail; no automatic mutation replay; token-free diagnostics | accepted |
| B03 | Terra → Sol | Search query contract and request synchronization (`HX-04/05`) | Multiple type/security filters work natively and through htmx; exactly one sort request; stale delayed response cannot win; filters/sort/pagination/history persist; explicit clear semantics | accepted |
| B04 | Sol | Recurring-job schedule persistence/reconciliation (`ENG-01`, `PROD-01`) | Actual registry→bootstrap→claim→finish→future recurrence; restart idempotence; repair exact existing built-in rows safely; no duplicate scheduling; operational repair documented, not run in production | accepted |
| B05 | Sol | Password-reset validation and atomic credential transition (`SEC-03`) | Weak password leaves valid token usable; consume/update/revoke commit atomically; persistence-failure and competing-reset tests; preserve suspended/deleted states | accepted |
| B05r | Sol | Administrator account-state transition guards (additional B01 finding) | Restore only suspended accounts, never deleted; preserve email-verification requirements on restore; suspension cannot overwrite deletion; authoritative predicates and rollback/race regression evidence | accepted |
| B06a | Sol | Mail-queue privacy and sensitive logging (`SEC-02/06`, `LEG-05` implementation) | Account-linked jobs; scrub terminal sensitive payloads; cancel/redact deletion copies; delayed/expired mail not sent; race tests; vendor error bodies cannot leak email/token; explicit retention/backups limits | accepted |
| B06b | Sol | Transactional auth outbox and delivery semantics (`ENG-06`, email retry findings) | Account/token/outbox commit together; enqueue failure/crash retry cannot strand registration; permanent vs retryable provider failures; supported provider idempotency; no exactly-once SMTP promise | accepted |
| B07 | Sol | Worker leases, supervision, execution modes (`ENG-04/05/08`) | Claim only active capacity; heartbeat all active leases; stale owner cannot silently complete; panic/timeouts/outcome-write failures observable; graceful shutdown; separate enqueue vs run config and worker-only command | accepted |
| B08 | Sol | CPU admission control and cancellation (`SEC-05`, `ENG-07`) | Owned image permit lives inside blocking closure; bounded hash work/admission; cancellation/burst tests assert actual running maximum; no durable login work | accepted |
| B09a | Sol | Sensitive-response cache and abuse-limit hardening (security additional checks) | Auth/private/token HTML no-store, fragment/full consistency, public cache policy explicit; credential-sensitive limiter failure has bounded safe behavior and monitoring; test trusted-proxy assumptions without changing edge | accepted |
| B09b | Sol | Provider CSP hardening (`SEC-04`) | Strict nonce-based strategy verified against installed htmx and all map providers; dynamic loader propagates nonce safely; legitimate SDKs work; required eval exceptions documented; report-only rollout path | queued |
| B10 | Terra | Map/GPS recovery, document metadata and navigation accessibility (`UX-01/02/11`, `HX-07`) | Localized loading/failure/retry, finite GPS timeout and manual destination fallback; blocked SDK/style/tiles handled; lang/title/focus/history sync; no reintroduced menu/map bugs | queued |
| B11 | Sol | Tab-specific detail reads and honest degraded states (`HX-06`) | Count/reader tests prove unused history/gallery not fetched; cheap pending-field metadata retained; bounded pagination/totals; failed collaboration read shown unavailable, never zero; compare query/response work before/after | queued |
| B12 | Terra | Compact cyclist-first profile/search presentation (`UX-03/04/05/06/07/08`) | 390px + desktop task checks; security/access/cost/map before reviews; short current/history/pending links; eligibility next steps; precise trust/history copy; cycling directions with fallback; compact results retain map/list mapping | queued |
| B13 | Terra | Contribution forms and accessibility (`UX-09`, acceptance journeys) | Paid fields conditionally disclosed; optional groups readable; file/description labels; pin/address validation; errors retain values and focus; keyboard/native fallback; no weakening approval | queued |
| B14 | Terra (Sol for security-notification logic) | Branded transactional HTML + plaintext and security notifications | Shared escaped bilingual template, actual expiry, accessible CTA/fallback URL; SMTP multipart + Resend text/html; both locales/kinds; blocked images/narrow/dark checks; password/old-address warnings queued without credentials | queued |
| B15a | Terra | Factual policy/runbook corrections (`LEG-01/03/05`, `UX-12`) | Browser storage/analytics and moderation accurately described in both languages; attribution vs anonymization precise; minors exemption claim removed; incident deadlines sourced; drafts not seeded/published automatically | queued |
| B15b | Sol | Terms-version acknowledgement/material-change notices (`LEG-04`) | Version + timestamp recorded for applicable agreement, no conflation with blanket privacy consent; recoverable notification/ack flow; migrations and tests; actual notification/policy publication awaits owner-approved wording | queued |
| B16a | Sol | Complete transaction-scope migration (`ENG-03`) | Remaining sequential repositories use acquire; HTTP router shares scoped Db; remove cleanup only after migration; panic leaves no fixture; no leaked process-global locks; explicit inventory reaches zero unexplained legacy writes | queued |
| B16b | Sol | Independent races and full-stack behavior tests (test gaps) | Disposable DB lanes for approval/suspension/reset/worker races; real rendered CSRF/search journeys; fail original bug cases; tests cannot claim foreign jobs; measured runtime baseline and parallel-repeat reliability | queued |
| B17 | Terra | CI advisories and measured test ergonomics (dependency/test findings) | RustSec + npm advisory checks, reviewed expiring exceptions; fast DB-free lane; builds vs execution measured; preserve asset/image checks; no blind dependency upgrades or test-runner churn | queued |
| B18 | Sol | Additional worker use for expensive noninteractive work | Measure export/media latency and sizes; queued exports with pending/ready/auth download lifecycle if material; media queue only with durable quarantine/privacy controls; record explicit measurement-backed decision, not an unjustified rewrite | queued |
| B19 | Sol reviewer + lead/owner | Integrated release and external evidence gates | Full isolated suite, browser matrix, image build, migrations fresh+upgrade; counsel/provider/edge/DNS/restore checklist; staged rollout/rollback plan; production deployment/reconciliation only after explicit authorization | queued |

## Batch dependencies and scope boundaries

- B00 is first because all subsequent DB regression evidence must be safe.
- B01–B05 are the immediate user-visible/security/retention corrections; separate commits let them be reviewed/released without the redesign.
- B06a/B06b may share a minimal account-aware outbox schema; define it once and use forward migrations. Do not duplicate transactional ports or store raw tokens indefinitely.
- B06a handoff constraints: verify both stored payloads and already-claimed in-memory copies against canonical account/token validity before provider handoff; define the deletion/send linearization boundary honestly (already accepted external mail cannot be recalled). Legacy rows need an explicit forward-upgrade policy, not an assumption that every existing payload has new metadata. Sanitize decode/provider/terminal-error paths, not just successful-message logging. B06b must reuse that lifecycle when making account/token/outbox atomic; B14 must reuse its expiry/locale metadata.
- Logging reference for B06a: [OWASP Logging guidance](https://cheatsheetseries.owasp.org/cheatsheets/Logging_Cheat_Sheet.html#data-to-exclude) recommends excluding credentials/tokens and treating personal data deliberately; test malformed and hostile provider responses through both persisted errors and captured events.
- B07 builds on B04; B08 is independent of worker throughput. B09b depends on B02's document/token contract, and B10 must preserve it.
- B11 supplies reliable read models for B12. Do not let a pretty empty state conceal a failed query.
- B14 uses B06/B07 delivery guarantees. Introduce structured expiry/locale metadata before rendering it into copy.
- B15 factual edits can be prepared without a legal opinion; provider contracts, legal bases, liability/age scope and material-change wording cannot be invented.
- B16 extends tests continuously added in every batch; it is not permission to defer required regressions until the end.
- B18 is explicitly conditional on evidence: findings recommended considering queues, not queuing every task. Interactive search/auth and atomic proposal publication stay synchronous.

## Coverage ledger

Every actionable audit ID is assigned; duplicates share batches.

| Audit IDs | Batches |
|---|---|
| HX-01, HX-02, HX-03 | B02 |
| HX-04, HX-05 | B03 |
| HX-06 | B11 |
| HX-07, UX-11 | B10 |
| PROD-01, ENG-01 | B04 |
| PROD-02, UX-10 | Excluded by user; validation/demo data unchanged |
| ENG-02 | B00 |
| ENG-03 | B16a, B16b |
| ENG-04, ENG-05, ENG-08 | B07 |
| ENG-06 | B06b |
| ENG-07, SEC-05 | B08 |
| SEC-01 | B01 |
| SEC-02 | B06a |
| SEC-03 | B05 |
| SEC-04 | B09b |
| SEC-06 | B06a |
| UX-01, UX-02 | B10 |
| UX-03, UX-04, UX-05, UX-06, UX-07, UX-08 | B12 |
| UX-09 | B13 |
| UX-12, LEG-01 | B15a |
| LEG-02 | B19 external evidence + B15a accurate wording |
| LEG-03 | B15a + B19 counsel applicability |
| LEG-04 | B15b + owner approval |
| LEG-05 | B06a, B15a, B19 |
| Additional security cache/proxy/limiter/MFA recommendations | B09a, B19; privileged MFA choice recorded as an owner decision |
| Email visual/security-notice recommendations | B14 |
| Tests/performance/advisory recommendations | B16a, B16b, B17 |
| More workers / queued exports / optional media | B07, B18 |

## Validation environment and baseline

- Use a new disposable PostGIS database/container with dedicated test credentials and loopback-only access. Never source the production `.env` into test commands.
- Pure tests: explicit domain/application packages. Full suite: explicit `TEST_DATABASE_URL` only after target validation and test-service setup.
- External providers use fakes or local mail/object-store/limiter services; no real emails/uploads in validation.
- Build/debug artifacts may reuse a known compiler cache, but never rebuild the running release binary or live generated static assets.
- Baseline audit: fmt and strict Clippy passed; 172 domain/application tests; 13 browser tests (~7.38s); no npm advisories at audit time; DB suite/RustSec not yet validated.
- Every review record lists commands, result counts, actual environment, diff/commit reviewed, issues and final disposition. Source-string guards do not substitute for behavioral proof.

## External evidence / authority checkpoints

These remain explicit until the owner supplies evidence or approves action:

1. Actual provider legal entities/regions/subprocessors, signed DPAs/transfer mechanism, counsel's legal/age/geographic-scope assessment.
2. Approved terms/privacy wording, effective versions, notification recipients and permission to publish/seed/send notices.
3. Edge cache/TLS/proxy allowlist topology, map-key restrictions, bucket privacy, SPF/DKIM/DMARC and provider idempotency support.
4. Backup retention and approved restore drill target; incident owner/escalation contact; authenticated security staging accounts and penetration-test scope.
5. Release window, migration/backfill review, exact production recurring-job reconciliation and separately approved deployment.

## Progress log

- B09a gate passed: independent Sol re-review confirms accurate pinned-library history behavior and no inert control claims; browser14/14, focused cache1/1, formatting/diff pass. Full infrastructure101/application109/HTTP173/workspace-check/strict-Clippy evidence remains applicable. Sensitive no-store and explicit bounded limiter policies accepted; edge topology still requires operational evidence. No migration/deployment. B09b is next.

- B09a correction sent for independent re-review: inert history markup/assertion removed; documentation attributes network restoration correctly to pinned htmx4, with browser test retained as upgrade guard. Implementer reports browser14/14, focused cache1/1, formatting/workspace check/strict Clippy/diff checks passed. See [B09a corrections](../reviews/remediation/B09a-corrections.md). Acceptance pending.

- B09a independent review requests one correction: pinned/shipped htmx4 does not read `hx-history`; its history already restores over network without localStorage snapshots. Remove inert markup and inaccurate causal claims, retain browser upgrade regression. Other checks accepted: infrastructure101/101, application109/109, HTTP173/173, browser14/14 and strict gates. Returned bounded correction; no acceptance yet.

- B09a stable handoff sent for independent Sol review: HTTP173/173, browser14/14, infrastructure101/101, application109/109 and strict gates reported passing. Authenticated cache helpers, actual network-backed history restoration, buffered RESP command errors and captured hostile-log exclusions close lead draft gaps. See [B09a handoff](../reviews/remediation/B09a-handoff.md). Acceptance pending.

- B09a draft validation: explicit sensitive-limiter policy replaces key-prefix inference; finite check deadline covers connection/command, dynamic no-store overrides and htmx history suppression implemented. Implementer reports HTTP173/173, browser14/14, application109/109, infrastructure101/101 and strict checks passed. Lead requires final captured-log redaction, authenticated HTML cache and actual back-content/network assertions before handoff. No acceptance yet.

- B08 checkpoint: `513da0b`. B09a assigned to Sol for sensitive cache headers, bounded credential-sensitive limiter failure and trusted-proxy tests only. Design and inventory required before edits; no CSP/edge changes or production action.

- B08 gate passed: independent Sol confirms closure-owned capacity, finite shared password admission, cancellation/burst evidence, startup/seed configuration, unchanged Argon2 and interactive auth. Independent infrastructure98/98, application109/109, photo11/11, HTTP172/172, formatting/workspace check/strict Clippy/diff checks passed. Initial sandbox loopback denial was rerun with permitted test access and recorded honestly. No material findings or migration; source accepted, not deployed. B09a is next.

- B08 frozen handoff sent to independent Sol reviewer: implementer reports password4/4, config1/1, image-admission2/2, infrastructure98/98, application109/109, photo11/11, HTTP172/172, workspace all-target check, strict locked Clippy, formatting and diff checks passed. Debug-process burst timing/RSS recorded with explicit non-production limitations in [B08 handoff](../reviews/remediation/B08-handoff.md). No migration. Acceptance pending.

- B08 draft validation: shared hash/verify execution and finite waiter budgets, closure-owned image permits and whole-operation test guards implemented. Implementer reports infrastructure98/98, application109/109, image11/11, HTTP172/172; capacity-two real hash/image bursts observed peak2. Debug timing is explicitly non-production evidence. Final checks/handoff and independent acceptance remain pending.

- B07 checkpoint: `ad5fdd6`. B08 assigned to Sol implementation for blocking image permit lifetime and bounded shared password hashing admission/cancellation. Actual running-work tests and honest measurement context required; no durable login, B09 work, production changes or shared release rebuilds.

- B07 gate passed: independent Sol re-review resolved all findings, passing three consecutive full job28/28 runs, CLI3/3, supervisor2/2, formatting, strict Clippy and diff checks. Recurrence4/4 remains applicable; lead DB-free domain/application suite passed. Accepted code has no migration; operational deployment remains unauthorized. B08 is next.

- B07 second corrections sent for independent re-review: ChildGuard kill/reap on all early failures with live-child unwind regression, empty dotenv sentinel, and durable timeout-state polling. Implementer reports three consecutive full job28/28 runs, CLI3/3, supervisor2/2, formatting/strict Clippy/diff checks passed. See updated correction handoff; acceptance pending.

- B07 correction re-review resolved original findings but requests two final test fixes: CLI early failures can leave an unreaped worker and ancestor dotenv traversal must be blocked; timeout test raced finalization (full job suite27/28, exact rerun1/1). Recurrence4/4, supervisor2/2, CLI2/2 and strict checks passed. Returned bounded fixes to implementer; no gate passed.

- B07 corrections sent for independent re-review: implementer reports job28/28, recurrence4/4, production-supervisor checks2/2, automated fresh-child CLI/preconnection2/2, workspace all-target check, strict Clippy, formatting and diff checks passed. Five worker cases now own child databases; missing fencing/finalization branches have regressions. CLI uses isolated cwd, readiness observation and bounded signal/kill/reap. See [B07 corrections](../reviews/remediation/B07-corrections.md). Acceptance pending.

- B07 independent review requests changes despite passing job24/24, recurrence4/4, config1/1, binary2/2, formatting/strict Clippy/diff checks: five new worker tests require owned database isolation, and distinct stale-fail/outcome-write/hook/supervision/CLI branches require direct reproducible evidence. Returned bounded corrections to implementer; see [B07 review](../reviews/remediation/B07-review.md). Lead DB-free domain/application run passed. B08 remains queued.

- B07 stable handoff sent to independent Sol reviewer: implementer reports job 24/24, recurrence 4/4, infrastructure unit 92/92, HTTP 172/172, supervision 2/2, fresh disposable worker-only CLI smoke, invalid-config preconnection smoke, formatting, strict locked Clippy, workspace check and diff checks passed. See [B07 handoff](../reviews/remediation/B07-handoff.md). Earlier disposable-row localhost S3 attempt is recorded there; no real provider or production action. Acceptance pending.

- B07 draft checks identified capacity-poll starvation, detached child-task cancellation, unbounded finalization, and normal-signal/worker-exit supervision races. Corrections and direct isolated regression evidence are in progress. Core code compiles; initial recurrence 4/4 and isolated stale-lease/reclaim checks passed. These are partial results, not acceptance; independent review remains pending.

- B07 design approved: claim only free concurrent capacity, per-execution claim identity and explicit lost-ownership outcomes, bounded handler/heartbeat/task lifecycle, worker supervision alongside HTTP, and separate durable-enqueue/run-worker modes with legacy configuration compatibility. Lead requires persisted panic outcomes, no detached shutdown tasks, in-flight claim handling and actual isolated race/failure evidence. Implementation underway; no gate passed.

- B06b checkpoint: `fa3f92d`. B07 assigned to GPT Sol implementation thread after independent acceptance; worker capacity/leases/supervision/shutdown and execution modes only. No deployment or production actions.

- B06b gate passed: independent Sol re-review resolved both findings, passing response-level Resend 5/5, infrastructure auth 35/35, formatting, strict application/infrastructure/web Clippy and diff checks; unchanged job 17/17 remains valid. Lead accepts source only, no migration. Prior sequential HTTP172/172 does not resolve B16 parallel fixture issue. B07 is next; no deployment/provider action authorized.

- B06b corrections ready: bounded response-level Resend parsing supports absent Content-Length; fresh recovery-state and final-audit rollback regressions added. Implementer reports Resend 5/5, infrastructure auth 35/35, job 17/17, formatting/locked strict Clippy/diff checks passed. Same independent reviewer reactivated; see [correction handoff](../reviews/remediation/B06b-corrections.md). Acceptance pending.

- B06b independent review round 1: Sol passed infrastructure auth 27/27, job 17/17, Resend 5/5, application auth 30/30, formatting/strict Clippy/diff checks. Changes requested for retryable 409 responses without Content-Length and complete direct recovery-state/final-audit rollback evidence. Returned bounded corrections to implementer; see [B06b review](../reviews/remediation/B06b-review.md). Lead DB-free domain/application run also passed. B07 remains queued.

- B06b stable implementation handed to independent Sol reviewer: implementer reports application auth 30/30, infrastructure auth 27/27, job/dispatcher 17/17, HTTP 172/172, Resend 5/5, web check, formatting/diff and strict application/infrastructure/web Clippy passing. No migration. See [B06b handoff](../reviews/remediation/B06b-handoff.md). Draft recovery/provider findings reported resolved; acceptance awaits independent verification.

- B06b draft milestone: one application `AuthOutbox` port and post-commit dispatcher; SQL adapter shares connection-local mail admission and registration recovery preserves existing credentials. Initial compile checks reported passing. Lead draft checks require active-lease recovery without new mail, unique inline claim ownership, permanent-error terminal handling, bounded retry/backoff/no-claim semantics, and secret-free decode errors. Provider semantics and real rollback/retry tests remain in progress; no review gate passed.

- B06a checkpoint: `5728a46`. B06b assigned to GPT Sol `b06a_corrections` (reused implementation thread, independent reviewer remains separate) after acceptance. Scope is transactional auth outbox and delivery semantics; B07 execution modes/supervision and B14 templates/notices remain later batches.

- 2026-09-10 B06a gate passed: independent Sol re-review resolved all three findings and passed job/mail 14/14, privacy 21/21 (isolated upgrade and observed races), application email 4/4, formatting, strict Clippy and diff checks. Lead accepts source only. Known parallel HTTP fixture failure remains B16; external mail/backup/rollout limits remain documented. Migration 0026 requires quiescing old senders; no production action performed. B06b is next.

- B06a corrected-source lead HTTP run: 171 passed, 1 failed (`review_create_updates_aggregate`, SQLSTATE `23503`, `review_location_id_fkey`, `review.upsert_review`, expected 303/actual 200). This matches the previously tracked B02/B16 legacy fixture failure; no mail-related HTTP failure. Isolated rerun passed 1/1 (~0.33s); do not report a clean full HTTP suite or claim the fixture issue fixed.

- B06a corrections ready for re-review: removed recipient-derived diagnostics and bounded database admission errors; expanded fresh-fixture purpose/credential matrices for handler, inline and durable admission; corrected current delivery guarantees. Implementer reports application email 4/4, job/mail 14/14, formatting/diff and strict application/infrastructure/web Clippy passed. Same independent Sol reviewer reactivated; acceptance pending. See [correction handoff](../reviews/remediation/B06a-corrections.md).

- 2026-09-10 B06a independent review: Sol passed job/mail 12/12, privacy 21/21 (including isolated upgrade and observed lock waits), formatting and strict application/infrastructure/web Clippy. Changes requested for recipient-domain diagnostic leakage, complete unconfounded purpose/inline/admission lifecycle evidence, and misleading delivery guarantees in touched deployment documentation. Separate Sol `b06a_corrections` assigned; same independent reviewer will recheck. Lead also reran DB-free domain/application tests successfully. See [review](../reviews/remediation/B06a-review.md). B06b remains queued.

- 2026-09-10: Resumed at the user's request. Fresh independent GPT Sol reviewer `b06a_independent_review` successfully assigned against `a3e6a2b` using the B06a handoff; the earlier agent-capacity blocker is cleared. B06a remains unaccepted and B06b onward remain queued until its review gate passes. No deployment or production action authorized.

- B06a independent-review gate blocked by agent-service capacity, not a source/test failure: three lead attempts to spawn a fresh GPT Sol reviewer and one attempt to reactivate the earlier Sol reviewer returned `agent thread limit reached`, even after the implementer completed. Interrupting completed agents did not release slots; no close/remove tool is available. Live listing retains root plus three completed agents. Requires a released/reset agent slot before the required separate Sol review can run. Stable implementation remains uncommitted and unaccepted; B06b onward remain queued. See [B06a handoff](../reviews/remediation/B06a-handoff.md). Do not bypass the review gate or claim the full plan complete.

- B06a implementation reports job/mail 12/12, privacy 21/21, application auth 30/30, infrastructure auth 23/23 and HTTP 172/172, strict application/infrastructure/web Clippy, workspace check, formatting and diff checks passed. Draft isolation issues corrected; new sequential fixtures roll back and real upgrade/send/deletion/enqueue races own disposable databases. Migration 0026 requires quiescing all old senders before application. Independent review assignment pending transient agent capacity; no source acceptance or deployment.

- B06a lead draft checks: domain/application 179/179 passed. Flagged legacy successful-mail history preservation, raw-token decoding before hashing, pending-account reset parity, lock-through-send versus idle-transaction timeout, and queue-admission/deletion serialization. A draft new race test used the legacy committed-fixture pattern; required conversion to the owned isolated-database runner before acceptance, with observed lock waits and bounded cleanup. No batch gate passed yet.

- B05r checkpoint: `a3e6a2b`. B06a assigned to GPT Sol `b06a_implement` after independent acceptance, with explicit account/token lifecycle, legacy-row upgrade, in-memory claimed-copy and deletion/send-boundary requirements. No production changes.

- B05r gate passed: two bounded test-only correction rounds added verification-token preservation and exact state/suspension/deletion/update timestamp snapshots. Independent Sol final re-review confirmed all findings resolved and reran infrastructure auth 23/23 (both real deletion lock waits), formatting and diff checks. Prior independent application 30/30, HTTP 172/172, workspace 670 passed/2 ignored and strict Clippy/check results remain valid for unchanged behavior. Accepted source only; B06a next.

- B05r review round 1: source inspection found no material behavior defect and independently passed application auth 30/30, infrastructure auth 23/23, focused admin HTTP 1/1, HTTP 172/172, full workspace 670 passed/2 ignored, strict Clippy, workspace check, formatting and diff checks. Reviewer requested complete persisted verification-token and affected-timestamp no-change/rollback evidence. Returned bounded correction to implementer; see [B05r review](../reviews/remediation/B05r-review.md). The workspace run does not replace the two explicitly ignored real-browser harnesses or settle B16 legacy-isolation risks.

- B05r implementation reports application auth 30/30, infrastructure auth 23/23 and focused admin HTTP 1/1, strict application/infrastructure/web Clippy, workspace check, formatting and diff checks passed. Includes exact audit/state matrices, final-audit rollback and independent lock-wait deletion-state races for both transitions; this does not claim full anonymization integration. Fresh GPT Sol `b05r_review` assigned. Full HTTP count will be independently confirmed because implementer capture was incomplete.

- B05 checkpoint: `a159dc3`. B05r assigned to GPT Sol `b05r_implement` after independent acceptance; no production changes.
- B05r lead checks: existing browser fixtures passed 13/13 (~7.45s), including menu and all three map-provider navigation lifecycles. Explicit state transitions with atomic audit are under implementation; requested real isolated lock-wait evidence in addition to scoped state and rollback matrices. Original production checkout remains unchanged apart from its pre-existing untracked audit directory.

- B05 gate passed: independent Sol re-review confirmed all corrections and reran application auth 29/29, test-support 6/6, infrastructure auth 20/20 and HTTP 172/172, strict Clippy, workspace check, formatting and diff checks. Real account-lock expiry and owned disposable-database cleanup are covered. Source accepted only; adjacent authenticated password changes and broader races remain explicitly queued for B06b/B14 and B16b. B05r is next.

- 2026-09-10: user resumed the full remaining sequence with the same implementation/independent-review gate. Baseline `2bd8d35` (B00–B04 accepted), worktree clean. Disposable audit container and loopback binding reverified; B05 assigned to GPT Sol `b05_implement`. Production and fake reviews remain untouched.
- B05 design: reset policy/hash validation precedes one account-locked transaction covering token consumption, credential update, all-session and competing-reset invalidation, and durable audit. Scoped SQL tests cover failure rollback; independent multi-connection reset races remain an explicit B16b obligation. Adjacent authenticated `change_password` still splits credential/session/audit writes; include its atomic expected-credential/session guard when adding transactional security-event outbox behavior in B06b/B14, rather than claiming B05 fixes every credential transition.
- B05 implementation reports application auth 28/28, PostgreSQL auth 18/18, HTTP 172/172, strict targeted Clippy, workspace check, formatting and diff checks passed. Missing password identities roll back; OAuth-only reset requests remain neutral without mail. A connection-local audit failure proves full rollback and subsequent successful retry. Fresh independent GPT Sol `b05_review` assigned; no migration or production change.
- B05 review round 1: independent Sol reran all reported suites/checks successfully but requested fake expiry/audit parity, stronger persisted non-change/exact-audit assertions, and authoritative expiry after account-lock waiting. Returned to Sol implementation; B05r remains queued. See [B05 review](../reviews/remediation/B05-review.md).
- B05 correction support: approved a bounded, uniquely owned loopback-only disposable-database runner for real lock-wait evidence, with awaited success/panic cleanup and explicit CREATEDB requirements. Ordinary sequential tests remain rollback-scoped. Expiry uses a fresh database wall clock rather than transaction-start time; see [PostgreSQL 17 current-time semantics](https://www.postgresql.org/docs/17/functions-datetime.html#FUNCTIONS-DATETIME-CURRENT).
- 2026-09-08: plan created; isolated branch/worktree created; demo-data exception recorded; B00 assigned first. No production changes.
- Plan/audit checkpoint: `78b0b1d`.
- Local validation infrastructure: new container `bikesnest-audit-test-20260908`, loopback `127.0.0.1:55439`, database `bikesnest_test_audit`, role `bikesnest_test`; identity verified with `current_database()`/`current_user`. It contains no production data and must not be confused with either existing database.
- Isolated-worktree baseline: `npm ci --no-audit --no-fund` succeeded; all 13 browser tests passed in ~7.39s. Package lock and generated assets unchanged.
- B00 implementation agent: GPT Terra (`b00_implement`); reports 5 pure guard tests, fmt, targeted Clippy and one rollback DB smoke passed. Independent GPT Sol (`b00_review`) assigned; acceptance pending.
- B00 review round 1: behavioral checks passed, but changes requested for an overstated isolation claim in TESTING.md and required cleanup of legacy annotations in the touched test-support file. Returned to Terra; B01 remains queued.
- B00 corrections: legacy annotations removed and guard documented as defense in depth; 5 guard tests/fmt/diff check passed again. Sol re-review pending.
- B00 gate passed: independent Sol re-review confirmed both findings resolved, reran 5 guard tests and fmt, and recorded PASS in [B00 review](../reviews/remediation/B00-review.md). Lead accepted source changes; nothing deployed.
- B00 checkpoint: `6874af0`. B01 assigned to GPT Sol (`b01_implement`) only after this gate passed.
- B01 implementation reports 25 application auth tests, 12 disposable-Postgres auth tests, workspace check, targeted strict Clippy, fmt and diff checks passed. No migration. Independent GPT Sol review assigned to `b01_review_retry` after implementation finished and reviewer capacity became available; acceptance pending.
- Additional lifecycle risk identified during B01: administrator restore currently sets Active without restricting the prior state. Track a bounded follow-up with deleted/pending-account guards and tests; B01 verification protection must not be mistaken for fixing every administrator state transition.
- B01 review round 1: all executed checks passed, but Sol requested direct PostgreSQL coverage for blocked-account confirmation, guarded verification/reset issuance and successful initial pending-account confirmation. Returned to implementer; B02 remains queued. See [B01 review](../reviews/remediation/B01-review.md).
- B01 corrections: three direct adapter regressions added using scoped transactions and automatic rollback; 15 infrastructure auth tests, targeted Clippy, fmt and diff checks reported passing. Same independent Sol reviewer assigned to recheck; acceptance pending.
- B01 review round 2: one remaining test gap returned to implementation—blocked database rows must reject otherwise valid expected issuance states, not only invalid expected-state values rejected before SQL. All other findings resolved; current gate remains pending.
- B01 gate passed: independent Sol final re-review confirmed the complete blocked-state/expected-state matrix exercises the SQL guard, reran all 15 infrastructure auth tests, strict targeted Clippy, fmt and diff checks, and recorded PASS. Lead accepted. No migration and nothing deployed; B05r remains separately queued.
- B01 checkpoint: `7c927e3`. B02 assigned to GPT Sol (`b02_implement`) after acceptance, with real middleware/browser lifecycle coverage and explicit no-automatic-mutation-replay requirements. Security reference: [OWASP CSRF guidance](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html); verify behavior against the exact installed htmx source.
- B02 lead-owned integration harness added alongside Sol's implementation: real Axum + Chromium with scoped DB and fake external providers. Boosted forms, stale-head poisoning, explicit headers, tabs/history and retained-input/no-replay recovery passed twice (~1.6s execution each). These focused journeys do not replace review of every middleware bypass path. Lead source inspection flagged an unsafe broad multipart-deferral draft for correction before independent review.
- B02 draft multipart deferral removed; fail-closed header/query native fallback retained, with an adversarial non-upload multipart regression. Full HTTP suite reports 168 passing tests. Expanded real-browser run passes (~2.7s), adding registration/login context changes, logout/stale-form rejection and deterministic first-cookie race recovery. The logout browser test deliberately avoids following the unrelated homepage redirect because that legacy search reader is not transaction-scoped yet; it observes the real logout response and cookie removal, then verifies the stale form is rejected. Native non-GET action recovery URL correction requested before independent review.
- B02 native recovery corrected to the safe source/parent page with login preserving the return destination; targeted regression, Clippy, fmt and expanded browser check passed. Fresh GPT Sol reviewer `b02_review` assigned; acceptance pending.
- Lead cross-batch check while B02 is reviewed: explicit domain/application test run passed all 174 tests (no database), confirming the B01 auth changes remain compatible with the rest of those packages.
- B02 review round 1: independent Sol reran HTTP 168/168, real browser 1/1 (~2.67s), browser fixtures 13/13 and diff check. Changes requested for direct expired/revoked/unknown/cross-session middleware evidence and for distinguishing session-store failure from a stale session. Returned to Sol implementer; B03 remains queued. See [B02 review](../reviews/remediation/B02-review.md).
- B02 corrections: scoped real-SQL middleware coverage for presented expired/revoked sessions and crossed live-session tokens added. Transaction-local search-path fault injection proves session lookup errors produce localized 503, no anonymous downgrade/new cookie or misleading stale-session recovery, and token-free unavailability diagnostics. HTTP 171/171, actual browser 1/1, targeted Clippy/fmt/diff checks reported passing. Same Sol reviewer assigned to recheck.
- B02 gate passed: independent Sol re-review confirmed both material findings resolved, reran focused session tests 3/3 and real Chromium 1/1 (~2.77s), checked the diff, and recorded PASS. Lead accepted; nothing deployed. Reviewer also observed a legacy `review_create_updates_aggregate` foreign-key failure in one full parallel HTTP run (170 passed, 1 failed); immediate isolated rerun passed. Track the intermittent fixture/isolation failure under B16a/B16b; its precise cause is not established merely by a retry, and it is not claimed fixed by B02.
- B02 checkpoint: `1d7cbab`. B03 assigned to GPT Terra (`b03_implement`) only after acceptance.
- B16 follow-up evidence: the observed failure was SQLSTATE `23503`, `review_location_id_fkey`, in `review.upsert_review`. Both `review_create_updates_aggregate` and `review_with_an_empty_body_flags_the_body_field` create `Review Spot`; the legacy `add_location` helper recovers IDs by name with `ORDER BY id DESC LIMIT 1`, while cleanup deletes by creator. Cross-selection/cleanup interference is plausible, not established by a trace. Reviewer clarified this limitation in the B02 record.
- B03 design: native htmx shared replacement queue plus a small live sort/filter mirror; preserve committed destination state. Lead approved a bounded migration of the search reader's five read sites from `pool()` to `acquire()` so new behavior tests use transaction injection and rollback. No broader adapter migration authorized in B03.
- Cross-batch CSP compatibility regression passed (1 test, ~0.48s) against the disposable target after B02 acceptance.
- B03 server checks reported passing: four parser regressions, one scoped real-router/PostGIS query-contract regression, formatting and strict targeted Clippy. Lead held the review gate because required real-browser ordering/clear/history acceptance evidence was incomplete; Terra continues that work. Lead also flagged fragment-clear visible-control reset and stale destination-link risks for correction and regression coverage before independent review.
- B03 complexity escalation: Terra's added actual browser harness exposed a remaining delayed-clear/control-state failure and did not pass. GPT Sol (`b03_finish`) assigned to finish implementation and deterministic browser acceptance; a separate fresh Sol reviewer will follow only once ready. Lead identified a plausible installed htmx queue-ownership race (an aborted older request's `finally` clears a newer request slot); distinguish that source evidence from the precise cause of the current failing assertion until diagnosed.
- B03 diagnosis correction: Sol found the last reported failure was the harness checking history controls before htmx restoration completed, not proof that a delayed response repopulated cleared filters. The installed queue-ownership issue was separately confirmed. Deterministic real-response gates replace time guesses; initial actual-browser run passes (~1.51s), with destination/empty/pagination acceptance still being completed before review.
- B03 implementation complete: Sol reports actual Chromium acceptance 1/1 (~2.63s), parser 4/4, scoped PostGIS HTTP 1/1, existing browser fixtures 13/13 (~7.35s), strict targeted Clippy, formatting, Node syntax and diff checks passed. Fresh GPT Sol `b03_review` assigned to independently validate before acceptance. No production changes.
- B03 review round 1: independent Sol reran parser 4/4, scoped HTTP 1/1, actual Chromium 1/1 (~2.62s), existing browser fixtures 13/13, formatting/syntax/diff checks. Changes requested for stale-request guards surviving body/history replacement, proof of newest response commit, actual fragment destination-change/native submission journeys, and catalogued malformed-query errors. Returned to Sol implementation; B04 remains queued. See [B03 review](../reviews/remediation/B03-review.md).
- B03 corrections: document-lifetime request ownership, real post-body/history restoration gate with matching request completion, distinct newest-response commit assertions, actual fragment destination changes and no-JavaScript form submissions, localized malformed-query rendering. Sol reports Chromium 1/1 (~3.38s), parser 4/4, scoped localized HTTP 1/1, browser fixtures 13/13, targeted strict Clippy, formatting/syntax/diff checks. Controlled removal of the post-body guard caused the history-spanning test to fail; restoring it passed. Same independent Sol reviewer assigned to recheck. Lead's cross-batch actual CSRF browser regression also passed 1/1 (~2.71s).
- B03 gate passed: independent Sol re-review resolved all findings and reran parser 4/4, scoped HTTP 1/1 (~0.23s), actual Chromium 1/1 (~3.43s), browser fixtures 13/13 (~7.36s), strict targeted Clippy, formatting/syntax/diff checks. Lead additionally ran full HTTP 172/172 (~2.49s). The legacy FK flake was not observed in this run and remains tracked, not claimed fixed. Lead accepted B03; nothing deployed.
- B03 checkpoint: `eede1b2`. B04 assigned to GPT Sol (`b04_implement`) after acceptance, scoped to recurring registration/reconciliation and actual recurrence evidence; production repair/deployment remains separately authorized.
- B04 implementation reports 3 scoped recurrence tests plus 11 existing job tests passing, strict infrastructure Clippy, workspace check, formatting and diff checks. Lead draft review requested no claim starvation on deferred bootstrap, independent registry-entry reconciliation, explicit NULL-schedule rejection, scheduled-failure preservation through GC, and per-test isolation/lease assertions; corrections are included. No migration. Fresh GPT Sol `b04_review` assigned. True multi-connection bootstrap has not been executed in the scoped lane and is not claimed proven by these tests.
- B04 review round 1: independent Sol reran 3 new + 11 existing job tests, strict infrastructure Clippy, workspace check, formatting/diff checks; lead full HTTP 172/172 also passed (~2.49s). Changes requested for persisted-state/repeat/second-execution evidence, separately exercised mismatch/invalid-input guards, independent retry backoff under busy claims, and alignment of GC policy with code/tests. Returned to implementer; B05 remains queued. See [B04 review](../reviews/remediation/B04-review.md).
- B04 corrections: expanded persisted-row/repeat/second-execution matrix; separate conflict and invalid-schedule cases; schedule-based GC preservation; bootstrap retry deadline with positive minimum while independent claims continue; deterministic shared-loop busy-work regression using scoped kind claims and read-only diagnostics. Sol reports 4 new + 11 existing job tests, strict infrastructure Clippy, workspace check, formatting and diff checks passed. Same independent Sol reviewer assigned to recheck. Operational docs qualify `finished_at` because failure overwrites it; no unconditional last-success guarantee is inferred from that column.
- B04 gate passed: independent Sol re-review resolved all findings and reran 4 recurrence + 11 legacy job tests, strict infrastructure Clippy, workspace check, formatting and diff checks. Lead final corrected-diff HTTP suite passed 172/172 (~2.50s). Accepted source only; no migration or deployment. True multi-connection race evidence remains B16b; broader worker supervision/fencing remains B07. B05 is the next queued implementation batch.
