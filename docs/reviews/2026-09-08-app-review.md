# BikesNest — full product and engineering review

**Audit date:** 8 September 2026

**Source:** `main`, commit `7aa243c`

**Scope:** cyclist experience, mobile/desktop usability, architecture, Rust practices, security, CSRF/htmx, fetching, tests, background jobs, transactional email, privacy/cookie policies and terms.

Read the executive assessment and delivery plan first. Detailed sections: [CSRF](#3-csrf-and-htmx-why-users-see-the-session-error), [search/htmx](#4-search-htmx-discipline-and-overfetching), [production evidence](#5-production-state-trust-and-retention), [architecture/tests/workers](#7-architecture-rust-tests-and-worker-reliability), [cyclist UX](#8-cyclist-ux-usability-and-accessibility), [security/policies/email](#9-security-policies-terms-and-transactional-email).

## 1. Executive assessment

**Keep the architecture and server-rendered approach. Fix correctness, trust and operational guarantees before investing in another visual redesign.** The app has a good underlying structure, but passing CI currently overstates confidence in complete user journeys. There are concrete authentication, search and maintenance defects, not just presentation concerns.

The reported “Sua sessão expirou ou este formulário está desatualizado” complaint has a reproducible CSRF-token lifecycle explanation. It is not evidence that CSRF should be disabled or weakened. The frontend can send an obsolete header even when the submitted form contains the correct token.

**Are we safe?** This review cannot certify that. Useful defenses exist, but the suspension bypass and broken maintenance scheduling are material findings. There is no evidence in this audit of an actual compromise. A code audit, passing tests and a clean npm advisory scan are not a penetration test or a legal compliance opinion.

### Fix order

P1 means address promptly before expansion; P2 means next engineering/product iteration; P3 means planned refinement. These are remediation priorities, not CVSS scores or legal conclusions.

| Priority | Finding | Evidence / consequence |
|---|---|---|
| P1 | Email verification can reactivate a suspended account | Source-confirmed transition to `ACTIVE` without an eligible-state predicate; defeats moderation enforcement. |
| P1 | CSRF metadata is stale after htmx navigation | Live browser: current form/cookie match, outgoing header does not; header wins on server. |
| P1 | Anonymous CSRF rotates between tabs and form visits | Live browser: opening registration invalidates the first tab's login token. |
| P1 | Retention and job GC do not recur | Production aggregate check: both rows have NULL schedules and are already `succeeded`. |
| P1 | Synthetic feedback is displayed as community feedback | Production: 44/44 listings seed-marked; 142/142 active reviews attributed to documented seed identities. |
| P1 | Multiple search filters fail; sort sends competing requests | Live HTTP 400 for repeated type/security parameters; browser sort emits two partial GETs. |
| P1 | Mail queue retains identities and raw token links beyond account deletion | Source-confirmed; broken GC compounds retention. No individual mail payload was inspected. |
| P1 | DB test harness can fall back to the application database | `DATABASE_URL` fallback plus misleading DB-free `cargo test` documentation can target the wrong database; not exercised here. |
| P2 | Failed map/GPS interactions lack usable recovery | Browser-injected map asset failure leaves blank map; location denial produces no message. |
| P2 | Password reset consumes its token before password validation | Source-confirmed: correctable input error can invalidate the recovery link. |
| P2 | Worker leases, cancellation and atomicity need strengthening | Batch claims are processed serially; several multi-step use cases can partially complete. |
| P2 | Policies contradict browser storage, analytics and moderation behavior | localStorage and live Cloudflare analytics exist; publication pipelines differ from some policy copy. |
| P2 | Private HTML caching and provider-specific CSP need hardening | `Vary` is not `no-store`; Google allowances are applied site-wide. |
| P2 | Profile hierarchy still delays cyclist decisions | At 390px width, map begins about 1,891px down; security about 1,090px down. |
| P2 | Test isolation is incomplete and browser coverage omits real form transitions | Transaction scope works where injected; legacy pool fixtures still commit. |
| P3 | Transactional email needs a reusable accessible visual template | Plain-text-only today; retain plain text while adding localized HTML. |

### What is working well

- A framework-free domain/application core, ports/adapters, runtime-bound SQL and forward-only migrations are a sensible fit. There is no demonstrated need for microservices, a SPA rewrite or a new worker platform.
- Published and proposed parking facts are separated. Current/history/approvals navigation and textual before/after diffs are worth keeping.
- Search remains usable as a list, not only as a map. Autocomplete debounce/cancellation, cached geocoding, bounded reads and explicit “search this area” already avoid unnecessary provider calls.
- Argon2id, hashed random session/token storage, role checks, image re-encoding, explicit unknown security states and sanitized rendering provide useful defense in depth.
- The rollback-aware database abstraction and new browser lifecycle tests are valuable foundations. Fix coverage gaps rather than discarding them.

## 2. Method, evidence and limitations

Three independent review tracks covered UX, architecture/tests/workers, and security/policies/email. The lead reviewer traced and reproduced CSRF/htmx/search issues and checked selected aggregate production state. Findings were cross-checked before synthesis.

### Negotiation analysis

**Query type:** cross-domain synthesis; **negotiation:** enabled. Rust-router, domain-web, concurrency and Rust anti-pattern/style skills guided the separation of domain constraints, architectural choices and implementation mechanics. The synthesis prioritizes observable failures and invariants over stylistic preferences such as replacing every clone or shortening every function.

| Source | Confidence | Disclosed gaps |
|---|---|---|
| Lead: real browser + local source + aggregate read-only SQL | High for observed CSRF/search mismatches and queue/provenance state | No production form submissions, accounts, attacks or database modifications. |
| Cyclist UX reviewer: Chromium at 390×844 and 1440×844 | High for measured layout and injected failure behavior; medium for design preferences | No field visits, moderated usability study, Safari or complete screen-reader audit. |
| Architecture reviewer: source, Cargo metadata, lint/unit checks | High for inspected dependencies, test harness and scheduling defect | No full DB suite, load profile, deployment failover or race campaign. |
| Security/policy reviewer: source and primary external guidance | High for explicit code paths; medium/unknown for operational/legal applicability | Contracts, provider consoles, edge ACLs, backup recovery and legal sign-off not available. |

**Overall confidence:** high in the specific reproduced/source-confirmed findings, not in the absence of other defects. “Full review” here means broad coverage of the requested areas, not an exhaustive line-by-line proof or compliance certification.

### Verification performed

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | Passed. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed; about 18.5s in this warm workspace. |
| Explicit domain/application tests | 172 passed; invalid DB URL forced to prevent accidental DB use. |
| `npm run test:browser` | 13 passed; about 7.38s, local fixture server and stubbed SDKs. |
| `npm audit --json` | Zero known advisories reported across 113 dependency-tree entries; no dependency changes. |
| RustSec advisory scan | Not performed: `cargo-audit` unavailable; not installed during this audit. |
| Public Chromium CSRF checks | Token mismatches observed; all unsafe form requests intercepted locally and not sent. |
| Public search checks | Multi-value filters returned 400; sort produced duplicate partial requests. |
| Public UX/failure checks | Profiles/maps/tabs rendered normally; local blocked asset and denied GPS exposed missing feedback. |
| Production aggregate SQL | Explicit `BEGIN READ ONLY`; inspected only queue scheduling and seed/review counts. |

No application behavior, policy seed, migrations, production content or service configuration was changed. No release, restart, PR or full DB test suite was performed. This report is the requested repository artifact; diagnostic scripts/screenshots were kept under `/tmp`.

## 3. CSRF and htmx: why users see the session error

### HX-01 — P1: a body navigation leaves the token in the head stale

**Evidence:** `templates/layouts/base.html:7`, `web/static/js/auth.js:5`, `web/static/js/navigation.js:79`, `crates/web/src/auth.rs:387`, `crates/web/src/routes/auth.rs:225`. The exact installed htmx implementation at `node_modules/htmx.org/dist/htmx.js:1049` removes the response head, retaining its title, then builds the body fragment. There is no app hook updating the CSRF meta tag.

The dependency's actual source matters: do not assume a head-management attribute described by newer online documentation is implemented in this pinned build. The event hook itself, `htmx:config:request` and `detail.ctx.request.headers`, is correct for this version. See the [official htmx events guide](https://four.htmx.org/docs/htmx-events-guide).

| Browser step | Original head token | New form token | Cookie token | Outcome |
|---|---|---|---|---|
| Hard-load `/login` | A | A | A | Aligned. |
| Click `/password-reset` using htmx | A | B | B | Body and cookie update; head does not. |
| Submit password-reset form | Header sends A | Body sends B | B | Server chooses header A and rejects it. |

The browser diagnostic recorded only equality booleans, not token values:

```text
hard-login:       metaMatchesForm=true, formMatchesCookie=true
boost-to-reset:   metaMatchesForm=false, formMatchesCookie=true
intercepted POST: headerMatchesCookie=false, bodyMatchesCookie=true
```

**Important qualification:** this demonstrates the actual outgoing mismatch; the POST was fulfilled locally with 204 to avoid sending email or mutating production. The resulting server rejection follows directly from the inspected middleware's header-first validation, rather than an unsafe live submission.

Other affected transitions, inferred from the same code, include login/register/reset error rerenders, authentication creating a new session after an anonymous entry page, and logout/login transitions. A hard-load of a public page with an empty meta token can temporarily mask the defect because the header is omitted and the hidden form token is used. This explains why entry path matters and why users call it intermittent.

The global hook also overwrites an explicitly configured `hx-headers` token, such as the photo upload's freshly rendered token (`parking_details.html:142`): htmx handles `hx-headers` before firing the global configuration event (`htmx.js:500–514`). Multipart forms are therefore not automatically protected from stale metadata.

**Recommended repair:** use one explicit token transport contract. Prefer the initiating form's current hidden token when setting a mutation header; keep any shared token metadata in the swapped body or update it through a deliberately tested response lifecycle. Do not blindly read a stale head meta tag or overwrite an explicit current token. Ensure non-form actions have a current token source too. Consider hard navigation across authentication boundaries as an immediate scoped containment, not a replacement for correct form lifecycle handling.

Do not make the server accept an invalid header merely because another token happens to validate. Do not disable CSRF or retry mutations automatically after refreshing a token: preserve user input and require a deliberate resubmission.

### HX-02 — P1: anonymous token rotation breaks other tabs even without htmx

**Evidence:** `crates/web/src/routes/auth.rs:89`, `:225`, `:273`, other anonymous form render branches; `routes/common.rs:119`; `auth.rs:232`, `:265`.

Each anonymous page/re-render generates a new token and replaces the shared one-hour HttpOnly cookie. In a real browser, hard-loading login in tab 1 then registration in tab 2 left tab 1's form/meta aligned with each other but no longer matching the cookie. Native submission would fail too. Fixing the htmx header alone will not solve this case.

**Recommended repair:** reuse a well-validated, stable anonymous session token for the anonymous browsing lifetime, with an intentional expiry/rotation policy and transition to the authenticated session token. Prefer an established signed/session-bound mechanism over a casually reusable unsigned value. Test concurrent first visits, multiple tabs, expiry and authentication transitions. OWASP explicitly notes the usability tradeoff of per-request tokens, including stale back-button forms; retain strong token validation while addressing lifecycle. [OWASP CSRF guidance](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html).

### HX-03 — P2: genuine expiry and stale-form failures are indistinguishable

`auth_middleware` validates before the handler's authentication gate, and unresolved/expired sessions fall back to anonymous auth (`auth.rs:417`, `:440`). A legitimately expired authenticated form can receive the same CSRF error as a frontend token bug. Logs currently show path/status/latency, not the safe diagnostic categories needed to distinguish them.

Keep the public message non-sensitive, but provide an actionable login/reload path and retain safe user-entered work. Record categorized failures such as missing token, stale session, token mismatch, and request shape, without raw tokens, cookie values, passwords, email addresses, search coordinates or full URLs. Measure CSRF rejection rates by route and navigation shape, not user identity. Do not introduce plaintext persistent storage for sensitive drafts/passwords.

### Required regression matrix

- Hard-load and boosted navigation among login, register, verify, reset and reset-confirm pages.
- Validation error followed by a corrected resubmission without refresh.
- Anonymous → authenticated → logout → login, including a public first page.
- Two tabs, back/forward, expired anonymous cookie and expired session.
- Normal URL-encoded forms, explicit fragment actions and multipart uploads.
- An explicit current per-form header must not be replaced by stale global metadata.
- Missing, incorrect and attacker-provided tokens must still fail; auth transitions must not accept a previous session's token.

Use the real auth middleware with isolated server state for these tests. The current browser fixture omits token-bearing forms, and the HTTP helper fetches a fresh anonymous token before each action (`tests/browser/navigation.test.cjs:32`, `crates/web/tests/http_test.rs:812`, `:878`), so neither exercises the failure sequence.

## 4. Search, htmx discipline and overfetching

### HX-04 — P1: checkbox encoding does not match server query types

`templates/pages/search.html:155` and `:178` render repeated `type` and `security` checkboxes. `crates/web/src/routes/search.rs:34`/`:36` deserialize each into a single `String`. There is no serialization hook joining repeated values before the request.

Observed public requests using explicit coordinates (no geocoding required):

```text
/search?lat=-25.4284&lon=-49.2733&type=rack                    -> 200
/search?lat=-25.4284&lon=-49.2733&type=rack&type=indoor        -> 400
/search?lat=-25.4284&lon=-49.2733&security=cctv&security=lighting -> 400
```

Using the actual UI, selecting Rack and Parking Facility sent `type=rack&type=parking_facility`; the result list became “Não conseguimos ler essa requisição. Tente novamente.” This is not invalid cyclist input; it is a client/server contract bug.

**Repair:** support repeated values on the server with an explicit multi-value query extractor/normalization contract, maintaining any existing comma-separated URL compatibility intentionally. Do not assume changing `String` to `Vec<String>` automatically makes the existing `serde_urlencoded` extractor handle repetitions. Test native forms as well as htmx so the fix is not JS-only. Preserve active filters through sort, pagination, history and clearing.

### HX-05 — P1: sorting triggers two searches with different state

`search.html:143` attaches `change from:select` without restricting it to the filter form. It also observes the separate sort selector, whose own handler submits the sort form (`app.js:431`). Changing sort to Distance produced:

```text
GET /search?lat=-25.4284&lon=-49.2733&sort=distance
GET /search?lat=-25.4284&lon=-49.2733&cost=&radius=1000
```

Both requests were `HX-Request-Type: partial` and target the same result area, but use independent source queues. The second lacks the selected sort; arrival order can overwrite the intended results, URL and out-of-band state. This is demonstrated overfetching and a correctness race, not a speculative performance concern.

**Repair:** scope changes to the correct form, preferably use its own bubbled change event; share synchronization for all searches targeting the same results (`replace`/latest-intent semantics, tested against the installed version). Add a short filter debounce if it improves actual mobile interaction. Assert one search per sort action and deliberately reorder slow responses to ensure the latest user intent wins. htmx's per-element default queue is not global result synchronization (`htmx.js:678`).

### HX-06 — P2: whole-profile work is done for every tab

`crates/web/src/routes/details.rs:122–204` fetches/signs the full gallery, assembles reviews and community signals, loads up to 50 proposals, loads 50 revision snapshots and counts pending photos **before** selecting the requested tab. `community.rs:1024` makes multiple sequential reads; review photos are usefully batched, but gallery/review signing is repeated even for history/approval views.

This is source-confirmed unnecessary work; a query trace/load benchmark was not run, so no invented p95 savings are claimed. The work is bounded in several places, which is good. Up to 50 reviews, proposals and snapshots still need not be loaded for every view.

**Repair:** split compact shared summary/pending-field metadata from tab-specific content. Load history only for history, detailed proposals only for approvals, and gallery/reviews for current. Paginate older versions/reviews and keep explicit totals separate from bounded page contents. Avoid eagerly computing a full fragment when a normal request will immediately redirect. Introduce targeted profile-pane swaps only after token/focus/history behavior is reliable; ordinary full-page GET links remain a valid htmx design.

An additional trust problem: `.ok()` / `.unwrap_or_default()` in the detail handler turn failed community/proposal reads into empty data. “No pending changes” is not equivalent to “couldn't load pending changes.” Preserve readable published facts during a partial outage, but label unavailable collaboration data and log the categorized error.

### HX-07 — P2: body-only navigation also leaves document metadata behind

The same lifecycle does not refresh `<html lang>`, canonical or description metadata. Live browser: switching PT → EN displayed English text while `document.documentElement.lang` remained `pt-BR`. This can give assistive technology the wrong pronunciation. Hard-loaded server HTML is correct; this is not evidence that every crawler gets incorrect metadata.

Synchronize page language and necessary metadata explicitly, or use full navigation where that is safer. Do not blindly reinject all scripts in every response head. Add assertions for language, title, heading focus and page-change announcements across real transitions.

### What htmx is doing correctly

- The inherited boost syntax and renamed htmx 4 event shape are deliberate and correct for the vendored version.
- `crates/web/src/htmx.rs:63` distinguishes explicit fragments from boosted/full/history requests. Fragment endpoints provide redirects for native forms and full navigations; expired fragment auth uses `HX-Redirect` rather than inserting a login page into a tiny target.
- Search responds with real HTML plus out-of-band result counts/query state/map data, without fetching an entirely separate duplicate JSON results API.
- A fragment result refresh keeps the existing map. Whole-page sync resets page-local Alpine state, addressing the previous menu/map bugs.
- The map asset manifest orders dependencies and caches successful asset loads. Do not replace this with global eager SDK loading on every non-map page.
- Autocomplete waits 800ms, aborts old suggestion requests and ignores responses for a changed input (`app.js:206`). Map panning offers an explicit area-search action rather than fetching on every movement.

The conclusion is **keep htmx, but define and test lifecycle contracts**, not “htmx is unsuitable.” The current architecture has good fragment discipline but incomplete document/token/state synchronization. The official [htmx 4 documentation](https://four.htmx.org/docs) also distinguishes explicit inheritance and its changed error-swap defaults; fixes must be verified against the pinned local source.

## 5. Production state: trust and retention

### PROD-01 — P1: daily maintenance ran as one-shot jobs

Source: `crates/infrastructure/src/job/registry.rs:82`, `worker.rs:95`, `repo.rs:62`. The registry defines a daily schedule but bootstrap passes only payload/time/attempts/idempotency key; the insert omits `schedule`. Idempotent bootstrap cannot repair an already-finished row by merely restarting the app.

Read-only production result:

| Kind | State | Schedule missing | Rows | Finished at (UTC) |
|---|---|---:|---:|---|
| `retention` | `succeeded` | true | 1 | 2026-09-07 13:58:59.308529 |
| `jobs.gc` | `succeeded` | true | 1 | 2026-09-07 13:58:59.311179 |

This establishes the in-app recurring mechanism is not delivering daily recurrence. External cron/timers were not inventoried; do not infer there is absolutely no other cleanup process. Expired token validation can still reject a token without purging its row: distinguish validity from data retention.

Repair schedule persistence and add a restart-safe reconciliation for existing unscheduled/terminal recurring rows. Reconcile only exact built-in keys with explicit safeguards; do not delete all job history. Test the real registry → bootstrap → claim → successful finish → future scheduled execution path, not a fixture that manually fills the missing schedule. Monitor last successful retention/GC execution and lateness separately from HTTP readiness.

### PROD-02 — P1: separate demonstration provenance from real cyclist trust

Aggregate production checks returned:

```text
parking listings: 44 total; 44 with seed_key; parking/31 is seed-marked
ACTIVE reviews:   142 total; 142 authored by documented seed-domain identities
```

The seeder explicitly synthesizes authors, review phrases, rating counts, verification ages and bundled photographs (`crates/infrastructure/src/devdata.rs:81`, `:137`, `:175`; `crates/infrastructure/src/parking/seed.rs:100`, `:180`). Public pages present review/freshness signals without an evident demo distinction in the reviewed journeys.

A seed marker alone does not prove every current fact is false or that later genuine corrections should be discarded. It does establish imported/demo origin, and the active-review aggregate is strong evidence that currently displayed feedback is synthetic rather than collected from independent cyclists.

**Action:** determine whether this deployment is intended as a demo. If so, label it conspicuously and make clear reviews/photos/verification ages are examples. If it is intended for real trip planning, inventory provenance, retain real contributions, and approve a backed-up reconciliation that removes synthetic trust signals from production presentation. Independently verify exact pin/access/locking facts. Do not run the mock seeder or blanket-delete seeded rows as an audit “cleanup.” No data was changed in this review.

## 6. Recommended delivery plan

### First: correctness and safety

1. Fix state-aware email confirmation/suspension, including repository-level atomic predicates and concurrent transition tests.
2. Fix CSRF source/lifecycle and stable anonymous sessions; cover real browser auth transitions and multi-tab behavior.
3. Repair recurring schedules and reconcile existing built-in rows under a separately authorized operational change. Add lateness alerts.
4. Fix multi-value search and shared latest-intent synchronization. Add real rendered-filter/browser tests.
5. Decide demo vs production provenance; do not silently present synthetic review signals as independent verification.
6. Scrub/cancel account-related mail payloads and correct token-reset ordering/atomicity.

### Next: resilient cyclist journeys

1. Give map/GPS/network failures actionable, localized feedback and retry without lost form work.
2. Synchronize language/document state; enforce private/no-store cache policy for sensitive pages and prevent edge caching of personalized fragments.
3. Reorder the mobile profile around security/access/cost/location; keep the current/history/pending concept with shorter labels and contextual eligibility actions.
4. Make reads tab-specific, reduce search chrome and paginate where measurements support it. Measure query counts, bytes and p95 before setting optimization targets.
5. Finish transaction-backed adapter migration and move isolated race tests to disposable databases. Keep source guards, but do not count them as behavioral accessibility/security coverage.

### Then: operations, policies and email polish

1. Fix queue lease/concurrency and outbox reliability before assigning it more important work.
2. Complete provider/transfer/retention evidence and correct policies to actual behavior in both languages; have qualified counsel resolve applicability and enforceability.
3. Add one branded HTML email shell with plain-text alternative, correct expiry, accessible CTA and monitored support; add security-event notifications and provider MIME/rendering tests.
4. Add RustSec scanning, a disposable full-stack browser lane, cancellation/race tests, an operational backup-restore drill and a focused authenticated security assessment.

### Release acceptance gates

- No CSRF mismatch in the transition matrix; invalid tokens remain rejected.
- Suspended/deleted accounts cannot become active through verification or reset paths.
- Selecting two or more filters succeeds in both native and htmx forms; one sort action sends one effective search; delayed old responses cannot win.
- Built-in jobs demonstrate a future recurrence after successful completion and after restart; monitoring detects missed runs.
- Deleted-account queue copies and delayed messages follow the documented retention/security rules.
- Published vs proposed facts and real vs synthetic feedback remain distinguishable.
- Mobile task journeys succeed with slow/failed maps, denied location and back/forward navigation.

The following detailed review sections retain source references, recommendations and specific limitations. Their local IDs identify findings within each discipline; the fix-order table above provides the cross-discipline priority.


## 7. Architecture, Rust, tests and worker reliability

Review date: 2026-09-08. Source: working tree at merged `main` (deployment commit `7aa243c`). Read-only application review; no application changes, database tests, policy execution or production mutations performed by this reviewer.

### Negotiation analysis

Query type: synthesis / best-practice review. Negotiation: enabled. Sources: repository architecture and test documentation, code inspection, Rust/router + web-domain + concurrency + anti-pattern + coding-guidelines skills, local compiler/linter/unit-test results, official Cargo/Tokio documentation. Confidence: HIGH for concrete implementation findings; MEDIUM for operational impact without workload/queue measurements. Gaps: no independent multi-connection DB tests, load tests, production queue inspection, restore drill, or full coverage/mutation campaign. Recommendations prioritize correctness and measured capacity over style churn.

### Summary

The modular monolith is a good fit. Keep Rust + axum + Askama + htmx, the inward dependency direction, ports for providers, and PostgreSQL as the consistency boundary. The material risks are not an unsuitable stack or pervasive unidiomatic Rust: they are a broken recurring-job bootstrap, incomplete worker supervision/lease handling, and a test suite whose isolation promises exceed its actual isolation.

### Findings

#### ENG-01 — HIGH: recurring retention and queue garbage collection are bootstrapped as one-shot jobs

Confidence: HIGH (direct data flow and independently confirmed live state). The coordinating reviewer ran a read-only production aggregate: retention and jobs.gc each have one succeeded row with schedule IS NULL, finished 2026-09-07 13:58:59 UTC, and no pending recurrence. No production writes were performed.

- `crates/infrastructure/src/job/registry.rs:82` declares daily retention and `:89` daily jobs-GC with `schedule` values.
- `crates/infrastructure/src/job/worker.rs:95` bootstraps through `enqueue`, passing payload, time, retry budget and key, **not the schedule**.
- `crates/infrastructure/src/job/repo.rs:62` inserts no schedule column; the database default is NULL (`migrations/0015_background_jobs.sql:17`).
- `crates/infrastructure/src/job/worker.rs:141` calculates next-run from the persisted schedule. NULL means no next run (`job/schedule.rs:26`), so a successful built-in job becomes terminal.
- Stable idempotency keys conflict on subsequent boots, and `ON CONFLICT DO NOTHING` does not repair schedule/state (`job/repo.rs:64`). Thus a restart alone does not restore daily execution while the row exists.

Impact: daily privacy retention and job-history cleanup are not guaranteed to happen. Email payloads contain address and credential-bearing links, so the GC failure is privacy-relevant as well as operational. This is the highest-priority engineering defect found.

Recommendation: persist the schedule through an explicit recurring-job upsert that safely repairs existing known built-in rows; ensure failed recurring jobs have an explicit retry/recovery policy; add queue-age/last-success alerts. Repairing production state is a separate authorized remediation, not part of this audit.

Missing test: exercise the real registry → bootstrap → claim → execute → reschedule path, then restart/bootstrap and assert exactly one future pending recurring row. `crates/infrastructure/tests/job_test.rs:105` currently manually UPDATEs the schedule at `:115`; it tests finish-success correctly but bypasses the defective production path.

#### ENG-02 — HIGH operational risk: DB test harness can silently target the application database

Confidence: HIGH; no claim tests actually touched production in this review.

`crates/test-support/src/lib.rs:63` accepts `TEST_DATABASE_URL`, falls back to `DATABASE_URL`, then a development default. `:76` immediately connects and applies migrations. There is no explicit test-only target requirement. Separately `Cargo.toml:1` has no `default-members`, despite `TESTING.md:8` claiming plain `cargo test` is domain/application only.

Read-only `cargo metadata --no-deps --format-version 1` confirmed **all seven crates** are default members. This matches the [Cargo workspace rules](https://doc.rust-lang.org/cargo/reference/workspaces.html): a virtual workspace without default-members selects every member. A developer exporting production DATABASE_URL and trusting the documented quick command could run writes, migrations and cleanup against the wrong database.

Recommendation: require an explicit TEST_DATABASE_URL and a positively validated disposable test environment; remove fallback to DATABASE_URL. Use a test-only role and a dedicated DB/container with no production access. Make documented quick command explicit (`cargo test -p bikesnest-domain -p bikesnest-application`) or intentionally configure default-members after considering cargo build/run ergonomics. Add a harness guard regression.

#### ENG-03 — MEDIUM: automatic rollback does not isolate all existing tests

Confidence: HIGH.

The new transaction-backed `Db` is sound for sequential repository work: `crates/infrastructure/src/db.rs:26` leases one connection; `crates/test-support/src/lib.rs:110` injects the outer transaction; `:305` catches panic and awaits rollback. But old tests use pooled readers/writers and `commit_fixture()` (`:167`) that genuinely commits fixture data before opening a different rollback transaction. The harness cannot undo writes on other pooled connections.

Examples: `crates/web/tests/http_test.rs:165`/`:178`/`:196` use cleanup → committed fixture → cleanup, and its router helper at `:17` uses an independent pool. At review time only seven infrastructure/web test files contained `tx.db()`; 63 source lines in infrastructure still called `pool()` (including legitimate production-only migration paths, so this is a migration inventory, not 63 defects). Test docs do disclose legacy pooled behavior, but their opening claim of all tests being isolated with zero cleanup is too broad.

Impact: panic/crash can leave fixtures, fixed tags can collide across runs, unrelated tests can observe committed rows, and global admin-set locks serialize test processes. `test-support/src/lib.rs:258` intentionally leaks a connection holding an admin-set lock for the entire process. That workaround prevents some interference but makes additional parallelism harder.

Recommendation: incrementally migrate remaining sequential adapters to `Db::acquire()` and inject scoped Db into real routers. Eliminate fixture cleanup on migrated tests, not by simply removing deletes from legacy committed tests. Give genuine race/global-state tests a separate disposable DB with independent connections. Do not run a database worker against the DB used by committed fixture tests.

#### ENG-04 — MEDIUM, HIGH under long jobs/multiple workers: a claimed batch is processed serially without leases for waiting jobs being refreshed

Confidence: HIGH for code; real occurrence requires queue duration/replica evidence.

`job/worker.rs:69` claims the whole configured batch (default four in `config.rs:378`) and `:74` runs jobs sequentially. `:134` starts heartbeat only when each job begins execution. All rows get the same initial lease deadline in `job/repo.rs:142`.

If the first job runs past the TTL, later jobs remain running with expired leases, so another worker can reclaim and execute them. The original worker subsequently executes its stale in-memory entries too. Outcome writes are owner-fenced, which protects job state, but cannot undo duplicate email sends or other external side effects. Even without a second worker, a long maintenance job blocks interactive email delivery for the whole process.

Recommendation: claim only available processing capacity; start execution/heartbeat immediately for every claimed row, or use a one-at-a-time claimer until bounded concurrency is implemented. Reserve capacity or separate kind lanes for transactional email and maintenance. Verify lease ownership before side effects where feasible and pass provider idempotency keys where supported. Do not simply lower the TTL or increase batch size.

Missing test: two real workers, one deliberately long handler, a small lease TTL, assert every waiting job is executed once under valid ownership.

#### ENG-05 — MEDIUM: worker panics and lost outcome writes can be invisible to application health

Confidence: HIGH.

- Handler execution is awaited without panic containment or deadline (`job/worker.rs:135`). A panic exits the single worker task.
- Its JoinHandle is examined only after HTTP server shutdown (`web/src/main.rs:363`). Normal `/readyz` probes only the database (`web/src/routes/public.rs:25`), so a dead worker can leave web health green while emails accumulate.
- `let _ =` discards finish/retry/fail errors (`job/worker.rs:143`, `:157`, `:169`, `:175`) and then logs succeeded/retry/dead-lettered as if persisted.
- Heartbeats stop silently on a DB error (`job/worker.rs:192`) while the handler continues. Repository heartbeat returns success even if no row was updated (`job/repo.rs:174`).

Recommendation: supervise worker lifetime during server operation, publish worker-alive and oldest-due-job metrics, log/persist outcome failures accurately, use bounded per-kind execution deadlines, detect lost ownership, and make heartbeat lifetime cancellation-safe. Do not make every optional dependency a hard web-readiness failure: expose separate operational alerts for queued work.

#### ENG-06 — MEDIUM: account/token creation and email queue insertion are not atomic

Confidence: HIGH; explicitly acknowledged by existing source.

`application/src/auth.rs:539` creates account, `:552` issues token, and `:564` enqueues the email through separate ports/transactions. Comments at `:557` document the crash window. If queue insertion fails after account creation, retrying registration finds an existing address and returns generic success at `:534` without repairing the missing email. Resend verification is the recovery route, but the registration error does not itself provide atomic recovery.

Recommendation: introduce a narrowly scoped transactional registration/outbox port so account/token/outbox record commit together; retain the generic response to avoid enumeration. Keep the provider call in the worker. Test queue-insert failure and crash/retry behavior, not only a mock returning EmailError. This is a reliability improvement, not a reason to send email synchronously again.

#### ENG-07 — MEDIUM capacity hardening: CPU admission control is incomplete/cancellation-sensitive

Confidence: HIGH for implementation; operational severity depends on concurrent load.

Password hashing correctly uses `spawn_blocking` (`infrastructure/src/auth/password.rs:31`, `:47`), but has no shared concurrency budget. Expensive operations can accumulate under distributed requests despite per-IP rate limiting. Image processing does use a semaphore, but its permit is owned by the awaiting async future (`photo/processor.rs:131`), not the spawned closure (`:139`). If that future is cancelled after blocking work starts, the permit is released while decoding continues.

[Tokio explicitly documents](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html) that started blocking tasks cannot be aborted, and CPU-bound work should use bounded concurrency. Recommendation: use a bounded shared hashing budget; move the image permit into the blocking closure so cancellation cannot undercount work. Test cancellation and concurrent work bounds. Do not offload login password verification to a durable worker: users require an immediate answer.

#### ENG-08 — LOW/MEDIUM: separate-worker deployment is not an independent mode

Confidence: HIGH.

`web/src/main.rs:313` suggests JOBS_ENABLED=false for web-only instances or running jobs elsewhere, but `web/src/wiring.rs:115` selects InlineEmailQueue when jobs are disabled. Turning off worker execution also disables durable email enqueueing on that instance. The binary has no dedicated worker-only entry point in the reviewed dispatch. This is not an immediate single-process production bug, but makes the documented scale-out idea misleading.

Recommendation: if splitting web and worker, separate queue-delivery mode from worker-execution mode and provide an explicit worker command. Continue with one process while workload is small; fix existing scheduling/supervision before adding deployment complexity.

### Architecture and Rust strengths

- Core manifests have no axum/sqlx/askama or reverse dependencies; composition is isolated behind application ports. A modular monolith avoids distributed consistency work that this product does not need.
- Real domain value types, tri-state unknown values, explicit errors and typed proposal payloads encode useful rules. Prefer extending those invariants to cosmetic refactors.
- Runtime SQL is parameter-bound; read/write repositories provide transaction boundaries. Proposal publishing uses explicit row locking and a shared atomic publisher (`moderation/actions.rs:752`, `community/contribution.rs:362`).
- `Db::connect_with` sets statement and idle-transaction timeouts and pool recycling (`infrastructure/src/db.rs:99`); migration connections explicitly isolate relaxed timeouts (`:134`).
- CPU-heavy password/image work is already off the async executor. Image resource bounding exists and needs a lifecycle correction, not a rewrite.
- Workspace unsafe is forbidden; edition 2024 is used. Strict Clippy passes. No broad clone/Arc rewrite is indicated: most shared references are normal service ownership.
- Modern Rust/style preferences should not override the repository's safety/inward-dependency rules. Large modules deserve focused extraction only when behavior/ownership is improved, not merely to pass line-count guards.

### Test assessment and speed plan

#### Good tests worth keeping

- Pure use-case suites cover auth anti-enumeration, queue failure, approval semantics, moderation, photo processing decisions, DST/overnight hours and domain boundary values.
- `parking_approval_test.rs:101` tests six distinct eligible votes publish one version; eligibility/stale/self-approval cases are meaningful. Scoped DB setup is correct for these sequential scenarios.
- Transaction-scope regression tests explicitly verify commits/savepoints, rollback after panic, and surviving-handle invalidation.
- Real browser navigation regressions cover delayed map dependencies, page history, mobile menus and fragment morphs instead of relying only on source-string assertions.
- CI already separates formatting, lint, tests, generated asset drift, browser navigation and production image checks.

#### Highest-value missing tests

1. Actual recurring-worker bootstrap/reschedule (ENG-01), not manually constructed scheduled rows.
2. Lease expiry and worker panic/heartbeat/outcome-write failure with independent connections (ENG-04/05).
3. Real browser+server CSRF/session lifecycle: cold anonymous auth pages, login/logout, history restore, multi-tab, expired auth, non-auth map→form navigation, repeated fragment posts. Current navigation fixtures don't execute auth middleware. Coordinating reviewer additionally confirmed `tests/browser/navigation.test.cjs:32` fixture has no CSRF meta/real forms and `web/tests/http_test.rs:812` / `:878` post helpers refresh tokens per action, masking stale transitions. Include multiple checkbox filter submission (live HTTP400) and exactly one request with preserved sort selection (live duplicate partial GETs) in behavior regressions.
4. Concurrent sixth vote versus moderator decision and competing proposals with separate DB connections. Scoped single-connection tests cannot prove lock ordering/race safety even if tasks are spawned.
5. Outbox crash/retry and admission-control cancellation.
6. Mutation/property tests for domain codecs, filter/cursor combinations, SQL/domain opening-hour parity; keep fixed regressions too.

#### Make tests faster safely

- Immediate: expose truthful DB-free command, run it as the fast lane. This review's 172 domain/application tests all report <0.01s execution per binary once compiled.
- Migrate legacy pooled writes into transaction-scoped tests to remove cleanup queries and leaked process-wide locks. This improves determinism and enables safe parallelism.
- Measure compile time separately from test time. `http_test.rs` is 7,979 lines with 166 test annotations; split by behavior for maintainability, but note each integration binary has its own runtime/pool/migration initialization. Avoid scattering every tiny test into a new process.
- Consider an integration-test root with modules (shared binary) plus isolated race binaries, after measuring. Current documentation's one runtime/pool is per process, not globally across Cargo's entire workspace run.
- Do not blindly introduce nextest-style per-test process isolation over today's committed/global fixtures. It changes pool counts, lock behavior and collision patterns; fix DB isolation first and benchmark after.
- Retain fake password/email/storage at provider boundaries for routine HTTP tests. Reserve live-provider/smoke tests for explicit opt-in runs with nonproduction credentials.
- Source guards are useful cheap rules but not proofs of accessibility, runtime CSRF freshness, or architecture. Pair them with rendered/browser/use-case behavior tests; do not chase raw test count or coverage percentage.

### Should workers do more?

Yes selectively, **after fixing correctness and observability**:

| Work | Recommendation | Reason |
|---|---|---|
| Transactional email | Keep durable; add atomic outbox and priority/capacity | User request should not depend on ESP latency |
| Retention / queue GC | Fix recurrence immediately; alert on last-success age | Already promised background behavior |
| Personal-data exports | Good next candidate once measured large | `application/privacy.rs:569` assembles full export synchronously today; use queued/ready states and authenticated polling |
| Large media transformations | Consider queue after upload acknowledgement if latency demands | Requires durable quarantine, processing state, cleanup and EXIF/privacy controls; not a free move |
| Search, geocoding resolution, login validation | Keep interactive with timeouts/caching/resource budgets | User needs the answer to continue |
| Proposal publishing | Keep atomic on approval transaction | Workers must not weaken version/approval invariants |

### Executed checks and limitations

- `cargo fmt --all -- --check`: PASS.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: PASS, 18.54 seconds (warm environment).
- `cargo test -p bikesnest-domain -p bikesnest-application --locked`: PASS, 172 tests plus empty doc-test suites. Both DATABASE_URL and TEST_DATABASE_URL explicitly pointed at an unusable localhost:1 audit target.
- `cargo metadata --no-deps --format-version 1`: confirmed seven default workspace members.
- Coordinating reviewer: browser suite 13 passed in 7.38 seconds, and read-only live recurring-job aggregate confirmed ENG-01; these were independent checks, not executed by this sub-reviewer.
- No DB integration tests, benchmark, production queue query, dependency advisory scan or load test was executed by this reviewer. Compiler success does not establish runtime correctness, data isolation or security.

### Suggested engineering order

1. Fix/repair recurring jobs and put retention/queue age under alerting; validate with the full bootstrap regression.
2. Remove unsafe test-DB fallback and correct the advertised test command.
3. Resolve user-visible CSRF lifecycle problems with browser+middleware regressions (other audit section).
4. Harden worker supervision, leases and delivery outbox; retain current monolith.
5. Finish scoped test migration and add dedicated race tests; benchmark test runtime before changing runners.
6. Introduce queued exports/worker separation only when the functional and operational foundation is dependable.

## 8. Cyclist UX, usability and accessibility

Reviewed 2026-09-08 against the main checkout and public bikesnest.com. Read-only Chromium sessions at 390×844 and 1440×844, source inspection, screenshots, simulated local-browser network failure and denied geolocation. No account creation, review/vote/photo submission or production content writes. Findings distinguish observed behavior from design recommendations; this is not a complete WCAG conformance audit or field validation of parking locations.

### Overall verdict

The product has a solid cyclist-oriented foundation: browse without an account, explicit unknown security states, freshness and opening hours, address autocomplete, useful external navigation, a non-map results list, and proposals that visibly distinguish published from proposed facts. The new version/approval navigation is a real improvement. However, the current profile still organizes information around the site's data model rather than a cyclist's immediate decisions: can I secure my bike here, is it accessible/open, what does it cost, and where exactly is it? Technical failures also look like a button that did nothing or an empty box.

### Prioritized findings

#### UX-01 — Map failure is invisible and provides no recovery action (P2, confirmed)

`web/static/js/navigation.js:47` swallows asset failures and waits for a later navigation or online event. `templates/pages/parking_details.html:275` leaves a fixed-height map box. In a public browser with only the details-map asset locally blocked, the map measured 286 CSS pixels tall, had zero children and empty text; there were no alert elements. A connected but unreliable mobile connection need not trigger a new online event, so users can remain stuck and discover refresh themselves.

Recommendation: explicit localized loading, failed and retry states, with an always-available address/external map link. Retry on the same page without resetting the contribution form. Log a categorized client-side failure without coordinates/tokens. Validate blocked SDK, provider module, style and tiles separately; a successful adapter load does not imply successfully rendered tiles.

Evidence screenshot: `/tmp/ux-map-asset-failure.png`. This intentionally injected network failure is not evidence of a current global map outage.

#### UX-02 — Denying location makes search appear unresponsive (P2, confirmed)

`web/static/js/app.js:432` returns silently when geolocation is unavailable; line 441 only stops the spinner on failure. Home's separate implementation has a denied state, so the same action has inconsistent feedback across pages. In a public search browser with geolocation returning permission-denied, clicking “Usar minha localização” left all main-region text unchanged.

Recommendation: share one location interaction with permission-denied/unavailable/timeout messages, an actionable address-entry fallback, an explicit finite timeout, retry, and an announced loading state. Avoid telling users to enable location when a typed destination is sufficient.

#### UX-03 — Mobile profile buries the map and security facts (P2, confirmed layout; prioritization is product judgment)

At 390×844, `/parking/31` was 3,037px tall; security started at y=1,090 and the map at y=1,891. At 1440×844, the map was correctly adjacent to the photograph at y=373, but security still started around y=1,050. `templates/pages/parking_details.html:92` uses main-column content followed by an aside, so mobile places the entire map and verification section after reviews. The gallery precedes the essential cost/security information.

Recommendation: keep the prominent directions link, then a compact decision summary (cost; open/access; frame-locking/security facts; last verified date), one restrained photo, and nearby map access. Place the map before reviews on mobile or expose a visible “Ver no mapa” anchor/button near the address. Use a sticky low-height directions/save bar only if it does not obscure content or keyboard focus. Do not add another large always-visible card for every fact.

Screenshots: `/tmp/ux-390-_parking_31.png`, `/tmp/ux-1440-_parking_31.png`.

#### UX-04 — Confidence language is difficult to reconcile (P2, confirmed copy ambiguity, not a proven data bug)

The same public listing showed “Verificado há pouco,” “Verificado há 36 dias,” “Recomendado porque → Confirmado por ciclistas,” sidebar “Confiança → Reportado,” and “Ciclistas que estacionaram aqui: 0.” These represent different concepts and can legitimately differ, but users are not taught the distinction. “Reportado” can read as a complaint/report rather than a merely reported location. `crates/domain/src/freshness.rs:20` intentionally labels 30–89 days “recently verified”; `templates/pages/parking_details.html:176`, `:297`, `:315` display overlapping signals.

Recommendation: prioritize the explicit last verification date and a concise explanation of provenance. Distinguish “cadastro recebido / ainda não confirmado” from “problema reportado.” Explain verification vs parking-experience counts. Move recommendation rationale into optional details instead of repeating green reassurance. Treat freshness as age of information, not a guarantee that parking is safe or available now.

#### UX-05 — Approval presentation is clear but lacks an anonymous next step (P2, confirmed)

Current/history/pending routes work and a live proposal displayed two field-level red/green comparisons with explicit “Publicado agora” and “Proposto” labels; color is not the only cue. Inline pending markers correctly exposed cost/security changes. However, anonymous viewers see voting counts and rules but no contextual login/register action or reason why voting controls are absent. `templates/partials/listing_collaboration.html:37` only renders controls when `proposal.can_vote` and has no alternate explanation.

Recommendation: show a compact action appropriate to eligibility: sign in, verify email, already voted, your own proposal, or stale proposal. Include “0 de 6 aprovações” rather than making users connect a distant rules paragraph to the count; keep rejection totals separate. Retain the actual safety/integrity rules on the server. Preserve the proposal anchor and user task through authentication.

At 390px the three navigation links wrap into two rows (`templates/pages/parking_details.html:80`), placing Pending on the second line. Short labels such as “Atual / Histórico / Pendentes (1)” would preserve a stable compact row. These are navigation links with `aria-current`, which is appropriate for separate server-rendered pages; do not add ARIA tabs unless implementing the corresponding tab keyboard behavior.

Screenshot: `/tmp/ux-390-_parking_31_tab_approvals.png`.

#### UX-06 — History label overpromises what is currently present (P3, confirmed)

Public `/parking/31?tab=history` showed “Versões anteriores” but its only item was “Versão 2 … Versão atual.” This need not imply data loss: full historical snapshots may only exist since the versioning feature was introduced. `templates/partials/parking_versions.html:7` expands every saved field, rather than showing the changes between versions.

Recommendation: call it “Histórico,” explicitly describe missing pre-versioning history when applicable, and show concise change summaries with expandable full snapshots. Include published date and decision provenance where privacy policy permits. Do not invent previous versions from current state.

#### UX-07 — Long search results delay comparison and recovery (P2, confirmed scale; optimization is a recommendation)

Public bbox search `?bbox=-49.285,-25.44,-49.255,-25.42` returned 34 results. At390px, the results container began at y=647 and the page was9,952px tall. Introductory copy, a search form, contribution CTA, map/filter controls and sort precede the first result. At1440px the same page was9,172px tall. The anonymous add-a-spot CTA competes with finding a place to park (`templates/pages/search.html:96`).

Recommendation: shorten the search introduction after initial use; make search/sort/filter a compact toolbar with active filter count. Keep contribution entry accessible but secondary. Prefer a compact initial result group and explicit accessible “show more” or pagination when measured results justify it; maintain numbered map/list correspondence and announced result counts. Test retained filters, selected result, scroll and map state when returning from a profile. Do not replace the useful text list with a map-only experience or assume that every long page needs virtualization.

The displayed distance is explicitly from map center for bbox searches; preserve that distinction and do not relabel straight-line distances as walking time without routing data.

#### UX-08 — Directions are not explicitly bicycle-oriented (P2, confirmed URL)

`crates/web/src/lib.rs:531` creates Google directions with destination coordinates but no `travelmode`. Google chooses relevant modes/preferences when the mode is unspecified. For a bicycle-parking product, the CTA can unexpectedly produce a non-cycling default. The OpenStreetMap link is a map pin rather than route planning; current wording “Abrir no…” is honest.

Recommendation: offer clearly labeled cycling directions (`travelmode=bicycling`) where supported, while preserving generic provider choice/fallback for locations lacking cycle routing. Distinguish the ride to parking from any walk to the final destination. Reference: [Google Maps URL documentation](https://developers.google.com/maps/documentation/urls/get-started).

#### UX-09 — Contribution form presents many optional details at once (P2, source-inspected; not authenticated browser-tested)

`templates/pages/parking_new.html:135` shows currency, price and charging unit even when parking is free/unknown, followed by full hours and eight tri-state security groups (`:174`). Address selection/map positioning and optional advanced coordinates are a good foundation, but this is a substantial roadside task. Photo upload controls on the profile (`templates/pages/parking_details.html:159`) lack explicit labels: file input has no associated label and the alt text field uses the upload-title placeholder instead of explaining what description to provide.

Recommendation: prioritize name, location/pin, parking type and cost; reveal price fields when paid; group optional hours/security under clear disclosure without losing unknown states or no-JS submission. Use visible labels for the image and image description fields. Validate pin/address mismatch and inaccessible advanced-field errors with real keyboard/screen-reader tests. Explain approval expectation before submission and retain the contribution intent through registration/email verification. Authenticated journeys were not executed to avoid production writes.

#### UX-10 — Synthetic review provenance is visible in production (P1, aggregate-confirmed)

Several public names, ratings/counts, descriptions and bundled photos resemble the deterministic mock dataset, which explicitly synthesizes community reviewers, ratings and review text. See `crates/infrastructure/src/devdata.rs:81` (seed identities), `:137` (synthetic review phrases), `:175` (shared bundled photos), `:512` (Bike Point Praça Osório), `:533` (Teatro Guaíra), `:736` (Racks Rua Voluntários da Pátria). Public Teatro Guaíra showed4.5(13), Osório4.0(4), and location31 showed a bundled-style rack photo. The lead reviewer subsequently confirmed aggregate production provenance: all 44 listings carry seed markers and all 142 active reviews are attributed to the documented seed identities. No locations were physically verified; seeded origin does not establish that every current listing fact is false. See PROD-02 for the exact scope.

Recommendation: urgently establish whether this is intentionally a demo or real production dataset. If demo content remains, visibly label it and do not present synthetic ratings/freshness/photos as real cyclist verification. Arrange a deliberate, backed-up provenance reconciliation—not blanket deletion. This is more important for cyclist trust than visual polish.

#### UX-11 — Language switching leaves the document language stale (P2 accessibility, confirmed)

In a1440px public browser, initial document language was `pt-BR`. Clicking the actual header EN link produced English content (h1: “A place to park. More room to explore.”) but `<html lang>` remained `pt-BR`. The whole-body navigation updates content without updating the document-level language attribute. Screen readers can consequently pronounce English text using Portuguese rules.

Recommendation: make language changes a full document navigation, or explicitly synchronize document language alongside any other retained head/document metadata. Add a browser assertion for both directions of language switching and browser history restoration. A translated h1 alone is insufficient.

#### UX-12 — Public analytics are observably active (policy cross-check, confirmed network observation)

The same browser fetched `https://static.cloudflareinsights.com/beacon.min.js/v31edd6df95cf4e85bb4c19e7a9bdbcba1788362987495` and automatically issued an XHR POST to `https://bikesnest.com/cdn-cgi/rum`. This was ordinary page-load behavior, not a deliberate data submission by the audit. It establishes that Cloudflare browser telemetry is active, irrespective of whether source templates contain its injected script. Coordinate with the policy/security assessment to ensure the actual telemetry and provider are accurately disclosed; this observation alone does not establish payload contents, identifiers, cookie behavior or a legal consent requirement.

### Accessibility and resilience assessment

Positive source patterns: skip link; semantic forms; labels on most inputs; autocomplete combobox/listbox keyboard support; result count `aria-live`; explicit unknown/no/yes security labels; linked `aria-invalid` field errors; dialog focus trap/inert support; ordinary links/forms for fallback; pending diffs with textual before/after labels; min-height44px profile navigation/actions.

Not proven: WCAG conformance, contrast under sun/high contrast mode, full keyboard traversal after every htmx swap, screen-reader announcement of page title/heading, mobile Safari keyboard/layout, real GPS behavior, offline journeys, authenticated contribution/moderation accessibility. The hamburger is40×40px and some controls smaller than44px; this alone is not a WCAG2.2AA failure, whose target-size minimum is24px with spacing/other exceptions.44px is a useful stronger touch design target ([W3C minimum](https://www.w3.org/WAI/WCAG22/Understanding/target-size-minimum.html), [W3C enhanced target](https://www.w3.org/WAI/WCAG21/Understanding/target-size.html)).

No horizontal overflow was observed on the audited pages at390px or1440px. No JavaScript pageerror events were observed in those routine page visits. Current-profile maps successfully rendered; this is a smoke test, not proof that prior intermittent navigation issues can no longer happen.

### Proposed profile information hierarchy

1. Name, address, open/access status, explicit last-verified date, Directions action.
2. Compact cost/security summary and one representative photo; prioritize real frame-locking/access conditions over repeated reassurance.
3. Single-row Current / History / Pending(n) navigation, with concise field-level pending markers on published facts.
4. Map/context, detailed hours and optional full security list; reviews afterward.
5. Secondary contribution/verification actions with clear sign-in/eligibility paths and uncertainty explanations.

Do not introduce yet another nested set of tabs. Preserve published/proposed separation and make pending changes noticeable without making every unchanged field into a large card.

### Suggested acceptance journeys

- First-time mobile visitor finds a free/open/frame-lockable location, understands unknown facts, opens cycling directions without needing an account.
- Search → profile → back preserves destination, filters, sort, selected result and reasonable scroll; browser back/forward and htmx transitions both work.
- Denied GPS, blocked SDK, failed tiles, slow request and offline transition show actionable accessible feedback without requiring a full refresh.
- Anonymous user follows a pending field marker, understands before/after and vote eligibility, then resumes the same proposal after sign-in.
- Verified user adds/edits on mobile with keyboard-only fallback; validation errors keep entered work and focus/announce the relevant field.
- Screen reader can identify page changes, autocomplete options, pending markers, diff old/new values, dialogs and form feedback.

Prefer a small number of behavior-driven browser scenarios with real server-rendered templates over snapshotting every class name. Add viewport screenshots as review aids, not as a substitute for task completion assertions or real cyclist field testing.

## 9. Security, policies, terms and transactional email

Reviewed 2026-09-08, main source at 7aa243c. Read-only source audit; no production account mutation, email delivery, upload, seed, or attack traffic. Rust-router, domain-web and m07-concurrency skills used. Negotiation: enabled; synthesis confidence HIGH for explicit code paths, MEDIUM for deployment/legal applicability. CSRF/htmx investigation belongs to main reviewer.

### Executive assessment

The app has valuable defenses: Argon2id offloaded from async workers, random hashed single-use tokens, explicit role checks, parameterized SQL, image sniffing/re-encoding/metadata stripping, bounded image decoding, production configuration validation, and security response headers. These are not enough to claim the service is safe or legally compliant. A concrete suspension-bypass path and identity retention in queued mail deserve priority over visual email improvements. Public-policy wording has drifted from implementation and the transfer inventory remains a checklist, not evidence of compliance.

### Security findings

#### SEC-01 — High: email verification can undo administrative suspension

**Confidence: HIGH, source-confirmed; not exercised against production.**

- `crates/web/src/routes/auth.rs:367`: public resend handler accepts email without requiring a signed-in actor.
- `crates/application/src/auth.rs:630`: `resend_verification` finds any account and issues a token without checking `account_state` or existing verification.
- `crates/application/src/auth.rs:581`: `verify_email` consumes token and calls `confirm_email` without enforcing an eligible account state.
- `crates/infrastructure/src/auth/account_repo.rs:311`: confirmation unconditionally sets `account_state = 'ACTIVE'`.
- `crates/application/src/auth.rs:1134`: suspension changes state and revokes sessions, but not outstanding verification tokens.

**Reproduction in an isolated test:** suspend an account; request a fresh verification email for it (or retain an unused token); consume token; attempt login. Code permits the state to become ACTIVE. User needs their own mailbox, not a privileged session. This defeats moderation enforcement, not arbitrary-account authentication.

**Fix:** state-aware verification transitions in the atomic repository operation; only initial PENDING_EMAIL_VERIFICATION→ACTIVE, and allow email-change confirmation for ACTIVE accounts without changing state. Deny issuance/consumption for suspended/deleted users and invalidate outstanding security tokens on suspension. Test concurrent suspension/confirmation as well as the sequential flow.

#### SEC-02 — High: deleting an account does not erase queued email identity/token payloads

**Confidence: HIGH for retention path; individual production mail payloads/backlog not queried; aggregate GC scheduling was verified separately.**

- `crates/infrastructure/src/email/queue.rs:62` serializes full `EmailMessage`, including recipient and raw bearer-token URL, into `background_job.payload`.
- `crates/infrastructure/src/job/repo.rs:224` marks successful jobs without clearing payload.
- `crates/infrastructure/src/job/repo.rs:290` deletes only terminal rows after cutoff; `crates/infrastructure/src/config.rs:383` defaults job-history retention to seven days.
- `crates/infrastructure/src/privacy/anonymize.rs:65` scrubs account, credentials, tokens, contributions and audits but never touches `background_job`.
- `crates/infrastructure/src/job/email.rs:38` sends from payload without checking account deletion or token expiry.
- `policies/privacy.en.md:71` promises identity is removed immediately after deletion; line 73 describes verification/reset retention as 24h/1h, without queue copies.

The relational token table stores hashes, but the mail queue creates a second plaintext credential store. After account deletion, terminal jobs retain old addresses and pending jobs can still deliver unwanted account messages. Seven days exceeds token validity (not necessarily token usability), and stalled pending jobs have no corresponding terminal GC bound.

**Fix:** classify mail payloads as secrets; minimize/scrub payload immediately on terminal outcomes, cancel/redact account-associated jobs in deletion, avoid relying solely on email-string joins (addresses can change), and check validity before delayed delivery. Prefer an opaque account/token-message reference or encrypted short-lived payload. Define backup retention separately; don't promise instantaneous removal from backups unless implemented. Test account deletion with pending/running/succeeded/failed mail, token expiry before execution, and worker/deletion races.

#### SEC-03 — Medium: password reset burns the token before validating the password

**Confidence: HIGH, explicit order.** `crates/application/src/auth.rs:822` consumes token; line 825 validates password; line 827 updates password; line 831 revokes sessions in a separate call. A weak-password submission makes a previously valid link unusable, even though the page offers the user another attempt. A persistence failure similarly consumes the token without completing recovery. Password update and session revocation can partially succeed.

**Fix:** validate new-password policy before token consumption; use an application transaction boundary for token consumption/password replacement/session invalidation. Invalidate other outstanding reset credentials when credentials change, with explicit desired behavior for parallel reset links. Test weak password→corrected retry with same token, persistence failures, and reset races. Existing single-use SQL `UPDATE ... WHERE used_at IS NULL RETURNING` is good and should stay. [OWASP reset guidance](https://cheatsheetseries.owasp.org/cheatsheets/Forgot_Password_Cheat_Sheet.html) supports single-use expiring tokens, consistent responses and post-reset notifications.

#### SEC-04 — Medium: Google CSP is broadly permissive site-wide

**Confidence: HIGH; hardening gap, not proof of an XSS vulnerability.** `crates/web/src/security.rs:61` allows script `unsafe-inline`, `unsafe-eval` and broad Google wildcard origins whenever Google is selected, including pages without a map. Strong frame/object/form restrictions and escaped templates remain useful. Google's own [CSP guide](https://developers.google.com/maps/documentation/javascript/content-security-policy) supports and recommends nonce-based strict CSP; its example still requires unsafe-eval, so do not promise eval can simply be removed.

**Fix:** evaluate a nonce-based policy, propagate nonces through the dynamic map loader and htmx lifecycle, retain only documented provider exceptions, and roll out in Report-Only with integration checks before enforcing. Keep provider configuration-driven. Do not break maps by deleting allowances blindly.

#### SEC-05 — Medium: CPU concurrency limits are incomplete under cancellation/bursts

**Confidence: HIGH for ownership; impact not load-tested.** `crates/infrastructure/src/photo/processor.rs:131` holds semaphore permit in the caller's async future, while the `spawn_blocking` closure at line 139 owns only bytes/limits. Canceling the future releases its permit while the blocking work can continue. New requests can start extra decode jobs. `crates/infrastructure/src/auth/password.rs:31` and `:47` offload hashing/verification without an overall concurrency budget; per-IP limits do not cap a distributed instantaneous burst.

**Fix:** move the owned image permit into the blocking closure so it lasts until real work ends; add a measured process-wide hashing budget with bounded queue/admission timeout. Retain offloading and per-IP/account limits. Test cancellation and assert actual running-task peak never exceeds budget; benchmark memory and p95 latency rather than guessing from core count.

#### SEC-06 — Medium: vendor error strings can undermine no-PII logging claims

**Confidence: MEDIUM; contingent on provider response contents.** `crates/infrastructure/src/email/resend.rs:77` includes raw provider error body in `EmailError`; `crates/infrastructure/src/job/email.rs:40` incorporates that error; dead-letter logger at `:53` emits it. Recipient-domain-only fields do not sanitize a vendor error that echoes an address/request detail. SMTP similarly propagates provider string errors. No actual leaked production data was inspected.

**Fix:** map provider errors to bounded, allowlisted codes/statuses; do not log arbitrary response bodies, credential-bearing URLs or SMTP conversation text. Add synthetic error responses containing email/token markers and assert these never reach persisted job errors/log events. Keep secure diagnostic correlation IDs.

#### Additional security checks and limits

- Private HTML does not centrally get `Cache-Control: no-store`: `security.rs:127` adds noindex and Vary, neither of which is a private-cache prohibition. Main reviewer should cover actual htmx history/cache behavior and authenticated pages.
- `client_ip.rs` correctly rejects arbitrary X-Real-IP and defaults to the TCP peer. Trusted hop-count protection still requires topology enforcement: direct origin must not be accessible with user-controlled XFF, and all public requests must traverse the configured number of appending proxies. Actual edge ACL/hop config not validated here.
- ValKey limiter intentionally fails open by default (`auth/rate_limit.rs:16`). Choose fail-closed or bounded in-process fallback for credential-sensitive endpoints; do not indiscriminately take down all traffic on cache outages. Alert on degraded enforcement.
- Photo format sniffing, dimensions check before decode, metadata-free JPEG output, and moderator-only publication are strong controls. Confirm presigned URLs/bucket privacy and access revocation separately in provider configuration.
- Auth change/reset currently sends no separate password-changed warning or old-address notification (`EmailKind` has only verify/reset-request/confirm-new-email). Add security notifications independent of the actionable token mail; never include passwords. Consider MFA/passkeys for administrators after core correctness fixes.
- No external penetration test, dependency-advisory scan, DNS SPF/DKIM/DMARC verification, infrastructure ACL test or backup restore drill performed in this subaudit.

### Policy and terms correctness

#### LEG-01 — Medium: explicit browser-storage and moderation contradictions

**Confidence: HIGH.** Keep en and pt-BR aligned when correcting.

| Promise | Reality / evidence | Correction |
|---|---|---|
| No localStorage (`policies/cookies.en.md:13`) | Map preference `bn.search.mapOpen` read/written in `web/static/js/app.js:394` and `:411` | List functional local storage, purpose, lifetime/deletion mechanism; distinguish tracking from preferences. |
| All submitted photos and texts held before publication (`privacy.en.md:59`) | Review insertion `community/review.rs:58` publishes review; terms `terms.en.md:40` correctly say other content can be immediate | Describe separate pipelines: new listing/reviews, existing-fact proposals (six eligible approvals or moderator), and moderator-only photos. |
| No analytics/third-party cookies (`privacy.en.md:114`) | Live browser observed the Cloudflare beacon and `/cdn-cgi/rum` request; `security.rs:54` also documents the injection. Cookie policy qualifies provider storage at line 17 | Disclose the observed analytics/provider processing precisely and inventory the actual payload/storage. A cookieless beacon is not automatically a tracking-cookie violation. |
| Browser location is approximate and never stored (`privacy.en.md:25`) | Browser geolocation precision varies; coordinates are used in search URLs | Prefer “coordinates provided by your browser; not retained in application database” only after checking proxy logs/history/analytics/provider requests. Never claim all systems discard them without evidence. |

[Cloudflare describes Web Analytics as privacy-first and not collecting visitors' personal data](https://developers.cloudflare.com/web-analytics/about/); this does not excuse an inaccurate product inventory, but it means the report must not label every beacon advertising or assert consent is automatically required. Cookie consent depends on actual purpose/storage/jurisdiction, not the mere presence of JavaScript.

#### LEG-02 — High governance risk: transfer assurances exceed documented evidence

**Confidence: HIGH for repository evidence; UNKNOWN for signed contracts held elsewhere.** `privacy.en.md:65` says ANPD clauses are incorporated in contracts or another lawful mechanism; `docs/provider-transfer-inventory.md:24`–`:31` still has TBD providers, unchecked regions/DPAs and no completed evidence. The inventory declares all hosting outside Brazil; deployment geography cannot be inferred from source. Google role is processor/independent controller in inventory, while policy broadly calls all listed providers processors.

**Action:** assign owner to fill actual host/database/storage/email/CDN/maps, legal entity, purposes/data, regions, subprocessors, signed DPA date/location and lawful transfer mechanism; retain contract evidence outside public repo as appropriate. Have counsel review whether a claimed contract-necessity exception applies; do not use a generic fallback mechanically. ANPD's [international-transfer guidance](https://www.gov.br/anpd/pt-br/assuntos/assuntos-internacionais/transferencia-internacional-de-dados) describes standard clauses and the original 12-month incorporation requirement; as of this audit that historical grace period is not a future launch allowance. This is an evidence gap, not proof that no DPA exists.

#### LEG-03 — Medium: “18+ avoids children obligations entirely” is not a safe legal assumption

`docs/legal-review.md:19` states age18+ avoids LGPD art14/GDPR art8 entirely. The signup language does contain an 18+ declaration (`i18n/src/lib.rs:1002`), so absence of a date-of-birth field is not by itself a bug. However, a public cycling map can be used without registration. Current [ANPD ECA Digital guidance](https://www.gov.br/anpd/pt-br/assuntos/eca-digital) and [Law15.211/2025](https://www.planalto.gov.br/ccivil_03/_ato2023-2026/2025/lei/l15211.htm) cover products directed at or likely accessed by minors.

**Action:** obtain a documented applicability/risk assessment for this service; do not certify an exemption from a footer declaration. If relevant, apply proportional privacy/safety measures; do not default to collecting identity documents. Counsel also needs to settle GDPR/UK scope, representative/DPO requirements, controller identity, licence/indemnity/liability provisions, and current notice-and-action duties. A perpetual UGC licence cannot override statutory privacy rights. Confidence MEDIUM for applicability; legal assessment required.

#### LEG-04 — Medium: policy versioning lacks acceptance/notification evidence

`templates/pages/register.html:41` shows legal links and implicit agreement text. `routes/auth.rs:105` / `application/auth.rs:510` do not record the effective terms/policy version acknowledged during signup. Policy-version storage alone is not an acceptance audit. `privacy.en.md:118` and `terms.en.md` promise advance notice of material changes; no corresponding policy-notification EmailKind exists.

**Action:** record terms version and acknowledgement timestamp (separate from consent for personal-data processing), add a versioned in-product notice/acknowledgement workflow where appropriate, and retain proof of notification. Exact legal necessity and wording require counsel. Don't repurpose the currently unused consent-record table as if all processing needed consent.

#### LEG-05 — Medium: “anonymised content” and operational promises need precision

`privacy.en.md:5` acknowledges user-authored content can identify someone; later lines71/74 assert residual records/contributions are anonymised. `privacy/anonymize.rs:194` removes author linkage but keeps review bodies, and report/privacy-request narratives likewise remain. Removing a foreign key does not guarantee free text/photos cease being personal data.

**Action:** state that account attribution is removed; maintain a content-redaction/removal rights workflow and clarify retention exceptions, backup expiry and queue copies. Inventory text fields and provider copies. Validate jobs run frequently enough to deliver “within24h” promises including outage recovery. Audit/purge documentation and config knobs are not evidence retention actually ran.

Incident runbook `docs/incident-response.md:69` says “statutory window” without actionable deadlines. [ANPD's current incident notification page](https://www.gov.br/anpd/pt-br/canais_atendimento/agente-de-tratamento/comunicado-de-incidente-de-seguranca-cis) states three business days for required authority/data-subject notices, subject to applicable rules. Add named owner, escalation timer, assessment template, exemption handling and draft notices; for applicable GDPR add its distinct assessment/timing requirements after counsel confirmation. Do not wait for perfect investigation before starting the clock.

#### Terms strengths and targeted counsel questions

Terms explain community-sourced uncertainty, no bicycle-security guarantee, contribution ownership/licence, photo privacy expectations, reports/appeals, and mandatory-consumer-rights exceptions. Keep these. Do not claim blanket immunity from the “as is” wording. Priorities for counsel are enforceability of indefinite irrevocable licence, indemnity and liability caps; controller/DPO facts; geographic scope; minors; lawful international transfers; and current Brazilian platform notice/removal obligations. This audit is engineering/product review, not legal approval.

### Email UX: improve presentation without turning security mail into marketing

**Confidence: HIGH.** `email/templates.rs:17` explicitly supports plain text only. Resend payload `email/resend.rs:48` has only text; SMTP `email/smtp.rs:65` builds a plain body. Current localized copy (`i18n/src/lib.rs:1958`) has correct per-kind subjects and ignore-if-not-you text, but no token-expiry information, clear branded hierarchy or support route.

Recommended shared accessible transactional template:

1. Small BikesNest wordmark, neutral background and narrow single-column content (roughly560px maximum).
2. Short heading specific to action: confirm email / reset password / confirm new address.
3. One sentence explaining why the user received it.
4. One prominent green CTA with at least44px touch height, plus a visible copyable fallback URL.
5. Explicit actual expiry: verification24h, password reset1h (prefer use structured expiry metadata to avoid prose drifting from implementation).
6. “Didn't request this?” guidance and a genuine monitored help link; no marketing banners or tracking pixel.
7. Plain-text alternative and well-tested inline CSS; meaningful text remains when images are blocked.

Add `html` to RenderedEmail; Resend sends both text and html; SMTP uses multipart/alternative. Reuse one structural template and localized catalog copy for all types, escaping all interpolated content/URLs. Keep recipient locale, not current operator/job-worker locale. Test both languages × all kinds × both providers, valid fallback/action URLs, escaped hostile text, no missing placeholders, MIME structure, expiry accuracy and basic narrow-screen/dark-mode/image-blocked rendering. Replace the existing test that explicitly asserts absence of HTML. Validate real inbox rendering and SPF/DKIM/DMARC with operational access before claiming deliverability.

The durable queue is the right place to send these messages. Prioritize secret-payload retention, transaction/outbox consistency and stale-token cancellation before expanding worker responsibilities. Resend currently retries all non-success HTTP statuses as transient and has only enqueue-time deduplication; classify permanent4xx versus429/5xx and evaluate provider idempotency for at-least-once sends. SMTP cannot guarantee exactly-once delivery; report that honestly.

### Scope limitations and recommended next verification

No changes to production or application source were made. Public web-open tool could not fetch bikesnest.com here; main reviewer owns live browser validation. Findings are code-reviewed, not an exploit certification. Confirm SEC-01/03 with rollback-isolated fixtures and the actual AuthService, SEC-02 with disposable worker/queue integration, SEC-05 with cancellation-focused unit tests, and policy drift with a live browser storage/network inventory. Complete provider-contract, DNS/email authentication, edge configuration, backup restore and incident-readiness reviews before making a safety/compliance claim.

### Addendum — bounded dependency advisory check, 2026-09-08

- Executed `npm audit --json` using npm11.16.0 / Node24.16.0. Initial sandbox DNS lookup failed; retried with authorized network access. Registry audit completed successfully (exit0): **zero known npm advisories**, all severities zero. Report dependency total113, prod76, dev2, optional36 (categories need not sum because optional classifications overlap).
- No `--omit=dev` was used: this checks the dependency tree including development tooling, not just runtime dependencies. No fix/install/update command executed.
- `package-lock.json` SHA256: `81763056bd5dfe9fde3c8d0a10fa537231bdb085dbd6b2106dd09f7060950635`.
- `cargo audit --version` returned “no such command: audit”; cargo-audit is **not installed**, and was not installed for this read-only audit. **RustSec/Cargo.lock advisory status remains unverified**, not clean.
- `Cargo.lock` SHA256: `e23878189524443439ea1e2c561f0f041ea5dd2bc6791790221c4557ab2d99eb`.
- Worktree remained clean at this check. The npm registry sees package/version metadata as part of the normal advisory lookup; no app credentials were sent.

Zero npm advisories means no matches returned by that advisory service for this dependency tree at check time, not proof of vulnerability-free dependencies. Separately verify pinned remote Google SDK behavior and vendor provenance; an npm audit cannot cover all browser-loaded third-party code. Recommend a reviewed CI RustSec advisory step plus scheduled npm checks, with explicit owners/expiry for any advisory exceptions.

#### Retention evidence update from main reviewer

Main's read-only production check found `retention` and `jobs.gc` rows in `succeeded` state with **NULL schedule**, finished September7. Consequently SEC-02's default seven-day retention is only a configured intention: payloads may remain **unbounded in practice unless an independent external cleanup mechanism runs**. Do not describe seven days as an observed production guarantee. Fix recurring scheduling and prove repeat execution, then enforce sensitive-payload minimization independently of queue-history GC.


## 10. Handoff

This is an assessment, not an implementation. The only repository change from the audit is this report. Do not interpret the recommendations as permission to delete seeded data, run retention, alter legal policies, send notifications, or deploy changes. Each should be implemented and verified in a scoped follow-up.

Start with the P1 fixes and their regression tests. Keep visual redesign and email styling in a separate focused change so security and operational repairs can be reviewed and released independently.
