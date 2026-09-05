# Architecture

How BikesNest is put together. Written for both humans and coding agents — the
"why" is here; the "where" index is in [`AGENTS.md`](AGENTS.md).

## Overview

BikesNest is a server-rendered Rust web application: axum serves HTML rendered
from Askama templates, with htmx swapping fragments and Alpine plus a selected
Google Maps or MapLibre adapter handling client-side behavior. All business
logic lives in a framework-free
core (`domain` + `application`); PostgreSQL/PostGIS is the source of truth.
Everything external (geocoding, email, object storage, OAuth, rate limiting) is
hidden behind a port so it can be replaced by a different implementation
without touching the domain.

```
browser ──> reverse proxy / TLS ──> axum (web) ──> application services ──> ports
                                          │                                  │
                                          v                                  v
                                    Askama templates                infrastructure impls
                                                                   (sqlx, S3, SMTP, ValKey,
                                                                    Google/Mapbox, image processor)
                                                                             │
                                                                             v
                                                                  PostgreSQL/PostGIS + object storage
```

## Principles

1. **Clean architecture.** Dependencies point inward, toward the core:
   `domain ← application ← {infrastructure, web}`.
2. **Framework-free core.** `domain` has no axum/sqlx/askama imports; business
   rules are pure and unit-testable.
3. **Ports & adapters.** The application layer defines `trait`s ("ports") for
   everything it needs from the outside world; `infrastructure` implements
   them. Replacing a provider is a wiring change, not a domain change.
4. **Runtime-checked SQL.** SQL is written with `sqlx::query`/`query_as` + `bind`
   (no compile-time macros), so the workspace builds with no database and no
   offline cache.
5. **Vertical slices.** The code is organized by feature (search, community,
   moderation, photos, privacy) across layers, not by a single horizontal
   "repository" or "model" module.

## The crates

| Crate | Responsibility | May depend on |
|---|---|---|
| `bikesnest-domain` (`crates/domain`) | pure business concepts: value objects, enums, rules (hours, freshness, confidence, cost, security), the typed proposal payload | nothing framework-level (chrono, thiserror, serde_json for the proposal payload) |
| `bikesnest-application` (`crates/application`) | use cases (services) + ports (`trait`s). Orchestrates domain objects; no I/O of its own | `domain` |
| `bikesnest-infrastructure` (`crates/infrastructure`) | SQLx repositories, config loading, providers (S3, SMTP, Google/Mapbox, ValKey, image, timezone), seeders, job worker | `domain`, `application` |
| `bikesnest-i18n` (`crates/i18n`) | the en + pt-BR string catalog (`Locale`, `Translator`); the axum request extractor sits behind the `axum` feature so infrastructure can render emails without it | nothing framework-level (axum only with the feature) |
| `bikesnest-web` (`crates/web`) | axum router, handlers, middleware, view models, Askama templates; re-exports the i18n catalog. **The binary** (`bikesnest-web`) | `domain`, `application`, `infrastructure`, `i18n` |
| `bikesnest-test-support` (`crates/test-support`) | shared `#[db_test]` harness, pool fixture, domain-rich builders, fast test doubles | `domain`, `application`, `infrastructure`, `test-macros` |
| `bikesnest-test-macros` (`crates/test-macros`) | the `#[db_test]` proc macro | (proc-macro deps) |

Crate boundaries are enforced by what each crate is allowed to import; there is
no build-time cycle (Cargo would reject it anyway). `application` never
references `infrastructure` or `web` types — it only knows the `trait`s it
declares.

## The layers

### Domain (`crates/domain`)

Value objects and rules, no I/O. Notable concepts:

- **`ParkingLocation`** — the aggregate: name, address, description,
  `ParkingType`, `Cost`, `GeoPoint` (lat/lon), IANA timezone, `OpeningHours`,
  security features, `ModerationState`, `Rating`, last-verified timestamp, and
  an optimistic-concurrency `version`.
- **`Cost`** — `Free` | `Paid { price: Option<Money> }` | `Unknown`. `Money` =
  cents + `CurrencyCode` + `PricingUnit`.
- **`ParkingType`** — `Rack | ParkingFacility | Indoor | Secured | Locker | Other`.
- **`SecurityState`** — `Yes | No | Unknown` (unknown is explicitly *not* "no").
  Each location carries a set of security *features* (locking point, indoor,
  CCTV, staffed, guard, controlled access, lighting, restricted access), each
  with one tri-state value.
- **`OpeningHours`** — wall-clock ranges per day-of-week stored in the
  location's IANA timezone (never converted to UTC), with an "open now" check
  that is DST-correct. `status_at` answers `Open | Closed | Unknown`; the
  boolean form of the same rule lives in SQL (see "open now", below).
- **`ModerationState`** (parking) — `Active | PendingReview | Flagged | Invalid | Removed`.
- **`FreshnessCategory`** — `Never | Fresh | RecentlyVerified | Aging | Stale | VeryStale`,
  derived from the last-verified timestamp against configurable thresholds.
- **`Confidence`** — `Reported | Verified | RecentlyVerified | Stale | Conflicting`;
  a pure resolution rule combining existence/attribute verification and review
  agreement (a conflict is never silently averaged).
- **`VerificationKind`** — `Existence | Attribute | ParkedHere` (parked-here is
  a private, short-lived usage signal).
- **`Proposal`** — `ProposalKind` (`MoveLocation | ChangeExistence`) +
  `ProposalStatus` (`Pending | Approved | Rejected | Superseded`); sensitive
  changes require a proposal rather than a direct edit.
- **`StarRating`** (1–5) and **`ReviewBody`** for reviews.
- **Accounts/auth** — `AccountState` (`PendingEmailVerification | Active |
  Suspended | Deleted`), `Role` (`User | Moderator | Admin`), `Password` (with
  policy).
- **`PhotoModerationState`** — `PendingReview | Approved | Rejected | Hidden`;
  upload constants (10 MiB, 20 MP, JPEG q85, 400 px thumbnail, jpeg/png/webp).
- **`ReportState`** — `Open | UnderReview | Resolved | Dismissed`, plus the
  report-reason code list and per-target validity.

### Application (`crates/application`)

Use cases + ports. Each feature is a service that receives its dependencies as
ports and returns domain results; it never touches HTTP, SQL, or filesystem
directly. Examples: `SearchParking`, `ContributionService`, `AuthService`,
`ModerationService`, `PhotoService`, `RetentionJob`, the job `Worker`.

**Ports** (the full list of `trait`s the application declares):

| Port | Purpose |
|---|---|
| `Geocoder` | address/place → coordinates |
| `ParkingSearchReader` / `ParkingDetailsReader` | proximity search (`search`) + map-viewport browse (`in_bounds`, clustered past the marker cap) + detail reads |
| `ParkingPhotoReader` / `ReviewPhotosReader` | photo gallery reads |
| `SitemapReader` | the ids `/sitemap.xml` lists (every ACTIVE location) |
| `ParkingContributionRepository` | add/edit/propose, optimistic apply, revisions |
| `ReviewRepository` | reviews + aggregate recompute |
| `VerificationRepository` | existence/attribute/parked-here signals |
| `FavoriteRepository` | favorites |
| `ContributionHistoryReader` | per-user contribution history |
| `AccountRepository` / `SessionStore` / `TokenStore` | accounts, sessions, tokens |
| `PasswordHasher` / `TokenGenerator` / `Clock` | crypto + time seams |
| `OAuthProvider` | federated login (Google; currently a fake) |
| `EmailProvider` | send verification/reset mail (`fake`/`smtp`/`resend`) |
| `RateLimiter` | sliding-window abuse limits |
| `AuditLog` / `AuditLogReader` | write/read the audit trail |
| `ObjectStorage` | put/delete + direct S3 presigned media URLs |
| `ImageProcessor` | decode → EXIF-strip → re-encode → thumbnail |
| `PhotoRepository` | photo lifecycle + moderation queue |
| `ReportRepository` / `ModerationRepository` | reports + moderation actions |
| `ExportRepository` / `PrivacyRequestRepository` / `AnonymizationRepository` / `RetentionRepository` / `PolicyReader` | privacy & retention |
| `TimezoneResolver` | coordinate → IANA timezone |
| `DatabaseProbe` | readiness DB check |
| `JobHandler` | background job execution |

### Infrastructure (`crates/infrastructure`)

The adapters: `Sqlx*` repositories for every persistence port, `Config::from_env`
(reads `.env`), `GoogleGeocoder`/`MapboxGeocoder`/`FakeGeocoder`, `S3ObjectStorage`,
`LocalImageProcessor`, `FakeOAuthProvider`, email impls
(`fake`/`smtp`/`resend`), `ValKeyRateLimiter`/`InMemoryRateLimiter`,
`OfflineTimezoneResolver`, `SystemClock`, `OsRngTokenGenerator`,
`Argon2PasswordHasher`, `SqlxJobRepository` + `Worker`, the `devdata`/seeders
(`seed-mock`, `seed-admin`, `seed-policies`, `seed-full-fresh`), and `Db`/`probe`.

Providers are selected from environment variables in `config.rs` and wired into
the router in `crates/web/src/wiring.rs` — the one module that names them.

### Web (`crates/web`)

`main.rs` loads env, connects the DB, runs migrations, optionally starts the
job worker, then serves the router. Subcommands dispatch before `serve`:
`seed-mock`, `seed-admin`, `seed-policies`, `seed-full-fresh`, `retention`.

The router is split three ways:

- **`wiring.rs`** — the composition root. It builds every provider from the
  parsed `Config`, assembles the services, fills `AppState`, mounts the route
  table and wraps it in the middleware stack (request tracing, security headers
  + strict nonce-free CSP, session/CSRF, auth extraction, the styled-error
  upgrade, compression). `RouterDeps` is the seam tests inject fakes through.
  This is the only module that may name a concrete adapter.
- **`state.rs`** — `AppState`: the services, the read-side ports and the
  configuration-derived values (map, security policy, base URL, asset
  manifest), cloned per request. It holds no connection pool.
- **`routes/`** — one module per slice, each owning its handlers, its
  form/query structs, its form → domain mapping and its error → response
  mapping: `public` (home, about, robots/sitemap, language, health), `search`,
  `details`, `auth` (accounts), `community` (add/edit/propose),
  `reviews` (review/verify/parked-here/favorite + the account activity lists),
  `photo` (upload and photo queue), `moderation` (reports and queues),
  `admin` (users, audit, privacy requests), `privacy` (export/delete),
  `legal` (policy pages), plus `common` (shared render/fragment helpers) and
  `errors` (the styled 404/500 family). `routes/mod.rs` holds the URL → handler
  table. Two tests guard the shape: no file over 1200 lines, and nothing under
  `routes/` may name a repository, a pool or an adapter.

`lib.rs` holds the Askama view-model structs; `i18n.rs` holds the en + pt-BR
catalogs; `security.rs` the headers/CSP; `observability.rs` the JSON structured
logging; `markdown.rs` the sanitizing renderer for the legal pages.

The parking detail page includes a frontend-only collaboration prototype in
`listing_collaboration.html` and `listing-prototype.js`/`.css`. Its proposals,
votes, moderation and version history are page-local mock state; see
[`docs/listing-collaboration-prototype.md`](docs/listing-collaboration-prototype.md).

## Tech stack

- **Language:** Rust (edition 2024), Cargo workspace, toolchain pinned via
  `rust:1.95` in Docker.
- **HTTP:** axum 0.8 + tower/tower-http.
- **Templates:** Askama 0.14 (compiled at build time, embedded in the binary).
- **Frontend:** htmx 4 (`hx-boost` + `hx-alpine-compat`), Alpine.js **CSP build**,
  and a provider-neutral map adapter. The MapLibre runtime is vendored; the
  Google profile loads Google's Maps JavaScript API from its required origin.
- **CSS:** Tailwind CSS 4.3, design tokens from `design-system/colors_and_type.css`.
- **Data:** PostgreSQL 17 + PostGIS, SQLx 0.8 (runtime-checked queries,
  forward-only migrations applied on startup).
- **Caching/limits:** ValKey (Redis-compatible) for the shared rate limiter.
- **Media:** S3-compatible object storage (aws-sdk-s3) with presigned GET URLs.
  The server-side endpoint and the public signing endpoint are configurable
  separately for container networks whose internal DNS names are not visible
  to browsers.
- **Crypto:** argon2id (passwords), HMAC signing, SHA-256-hashed sessions at rest.
- **Email:** lettre (SMTP) or the Resend API.
- **Maps/geocoding:** `LOCATION_PROVIDER` selects Google Maps Platform,
  Mapbox geocoding with Mapbox GL JS, or the deterministic development
  fake with OpenFreeMap. The same profile controls direct geocoding,
  autocomplete, suggestion resolution, and map rendering.

## Data model

Versioned, forward-only migrations in `migrations/`:

| Migration | Covers |
|---|---|
| `0001_init.sql` | base `users`, schema |
| `0002_parking.sql` | `parking_location` + PostGIS geography, `opening_hours`, `parking_security` |
| `0003_photos.sql` | initial photo storage |
| `0004_security_codes.sql` | security-attribute code list |
| `0005_accounts.sql` | `authentication_identities`, `sessions`, tokens, `user_roles`, account lifecycle |
| `0006_audit_events.sql` | the audit trail |
| `0007_contributions.sql` | `parking_revision`, `parking_proposal`, optimistic `version` |
| `0008_community.sql` | `review`, `review_revision`, `verification`, `favorite` |
| `0009_photos.sql` | photo moderation pipeline (`parking_photo` moderation fields) |
| `0010_moderation.sql` | `report` (polymorphic target), moderation CHECK widening |
| `0011_review_photos.sql` | `review_photo` (review photo attachments) |
| `0012_privacy.sql` | privacy requests, exports, anonymization |
| `0013_policies.sql` + `0014_policy_locale.sql` | versioned legal pages |
| `0015_background_jobs.sql` | `background_job` queue |
| `0016_report_dedupe.sql` | one open report per (target, reporter) |
| `0017_indexes.sql` | FK/read-path indexes, narrowing CHECKs |
| `0018_user_locale.sql` | per-account locale |
| `0019_photo_key_and_audit_integrity.sql` | non-empty `storage_key`, append-only audit |
| `0020_open_now_fn.sql` | `bikesnest_is_open_at()` + confirmed-attribute index |

Key modeling notes:

- **Timestamps are UTC**; opening hours are wall-clock ranges in the location's
  timezone; "open now" is computed in that timezone.
- **"Open now" is one implementation with two shapes.** The rule (same-day
  ranges, all-day rows, and ranges that run past midnight counting on both
  days) lives in SQL as `bikesnest_is_open_at(location, timezone, instant)`
  (migration `0020`), which the search query calls twice: once as the
  `open_now` filter and once as each row's flag. The domain keeps
  `OpeningHours::status_at` because the details page needs a *tri-state* — it
  must say "hours unknown" rather than "closed", which a card's boolean cannot
  express. `bikesnest_is_open_at` is exactly `status_at(...) == Open`, and a
  table-driven `#[db_test]` (`parking_test.rs`) holds the two together across
  same-day boundaries, overnight ranges, all-day rows, a DST transition and
  locations with no hours at all.
- **`parking_revision`** is an immutable field-level history (JSONB after-state
  snapshots); **`parking_proposal`** holds sensitive changes pending moderation.
- **Optimistic concurrency** via `version` on `parking_location`; conflicting
  edits are rejected and the client re-fetches.
- **`review`** is one-per-user-per-location with an aggregate rating recomputed
  in-transaction; `review_revision` preserves history.
- **Photos** are held `PENDING_REVIEW` until a moderator approves; only the
  processed derivatives are stored (the original is discarded); EXIF is
  stripped at processing time.
- **`background_job`** stores durable one-shot + recurring jobs; an in-process
  worker claims with `FOR UPDATE SKIP LOCKED`, retries with exponential
  backoff, and dead-letters after `JOBS_MAX_ATTEMPTS`.

## Request lifecycle (happy path)

1. TLS terminates at a reverse proxy; the proxy forwards to `BIND_ADDR`.
2. axum middleware runs: request tracing (method/path/status/latency — headers
   never logged), security headers + CSP, session/cookie + CSRF check, locale
   resolution (`Accept-Language`, fallback pt-BR, `lang` cookie override).
3. The handler extracts inputs, calls an application service (e.g. `SearchParking`).
4. The service applies domain rules and calls its ports; infrastructure impls
   run the SQL (e.g. `ST_DWithin` proximity) or call providers.
5. The handler builds a view model and renders an Askama template (full page)
   or an htmx fragment; media URLs are presigned by `ObjectStorage`.

## Cross-cutting concerns

- **Repository test isolation:** `Db` can explicitly wrap a test-owned SQLx
  transaction. `Db::acquire()` leases that connection, and nested SQLx
  transactions become savepoints. Production still uses the ordinary pool.
  Test-support awaits outer rollback (including after panic) and invalidates
  surviving handles. This is not a substitute for multi-connection race tests;
  see `TESTING.md` for the incremental adapter migration.

- **Security:** strict CSP (nonce-free, Alpine CSP build), security headers,
  CSRF synchronizer token, HttpOnly/Secure/SameSite=Lax sessions hashed at
  rest, argon2id passwords, deny-by-default authorization, server-side
  self-resolve guard on reports. See `crates/web/src/security.rs`.
- **Observability:** `APP_ENV=production` → JSON structured logs; PII-free.
- **Rate limiting:** sliding-window via ValKey (Lua atomic, fail-open by
  default), shared across auth/photo/contribution/moderation.
- **i18n:** all user-facing strings in the catalog; the domain exposes codes
  (e.g. security feature codes), the web layer maps them to localized labels.
- **Background jobs:** Postgres queue + in-process worker (`JOBS_ENABLED`).
- **SEO:** `robots.txt`, `sitemap.xml`, canonical/meta/OG, `hreflang`,
  `noindex` support.
