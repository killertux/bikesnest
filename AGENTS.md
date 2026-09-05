# AGENTS.md

Orientation for AI coding agents (and anyone new to the codebase). Read
[`ARCHITECTURE.md`](ARCHITECTURE.md) before structural changes and
[`TESTING.md`](TESTING.md) before writing tests.

## What this is

**BikesNest** is a community-maintained bicycle parking finder. A Rust web app
(axum + Askama server-rendered templates + htmx/Alpine) backed by
PostgreSQL/PostGIS. Users search a destination, browse parking spots on a map,
and the community adds, edits, reviews, verifies, photographs and moderates
that data. Bilingual (en + pt-BR). Works on mobile and desktop browsers.

## Golden rules

- **Dependencies point inward:** `domain ← application ← {infrastructure, web}`.
  `domain` must not import axum/sqlx/askama; `application` must not import web
  or infrastructure. Enforce this on every change.
- **SQL is runtime-checked** (`sqlx::query`/`query_as` with `bind`). There are
  **no** compile-time `sqlx::query!` macros — `cargo build` needs no database.
- **Migrations are forward-only** and applied automatically on startup
  (`serve`). Add a new `migrations/NNNN_*.sql` file; never edit an applied one.
- **Every external dependency is behind a port** (a `trait` in
  `crates/application`). Replacing a provider = new impl + wiring change, never
  a domain change.
- **User-facing strings** live only in the `bikesnest-i18n` catalog
  (`crates/i18n/src/lib.rs`, en + pt-BR; re-exported as `crates/web/src/i18n.rs`
  and used by infrastructure to render emails), never hard-coded in
  domain/application/web logic.

## Index — where to find things

| What | Where |
|---|---|
| Cargo workspace definition | `Cargo.toml` (workspace members + shared deps + lints) |
| Pure domain rules & value objects | `crates/domain/src/` (`parking.rs`, `community.rs`, `auth.rs`, `moderation.rs`, `photo.rs`, `privacy.rs`, `hours.rs`, `freshness.rs`, `lib.rs`) |
| Use cases + ports (traits) | `crates/application/src/` (`search.rs`, `community.rs`, `auth.rs`, `moderation.rs`, `photo.rs`, `privacy.rs`, `jobs.rs`, `ports.rs`, …) |
| SQLx persistence & providers | `crates/infrastructure/src/` (`db.rs`, `config.rs`, `storage.rs`, `geocoding.rs`, `devdata.rs`, `probe.rs`, plus `auth/`, `community/`, `email/`, `job/`, `moderation/`, `parking/`, `photo/`, `privacy/`, `timezone/`) |
| HTTP server, routes, handlers, view models | `crates/web/src/` (`main.rs` entry point, `wiring.rs` composition root, `state.rs` `AppState`, `routes/` one module per slice — `mod.rs` route table, `public`, `search`, `details`, `auth`, `community`, `reviews`, `photo`, `moderation`, `admin`, `privacy`, `legal`, `common`, `errors` — `lib.rs` templates/view models, `i18n.rs`, `auth.rs`, `security.rs`, `observability.rs`, `markdown.rs`, `view.rs`) |
| Database schema | `migrations/` (numbered `NNNN_*.sql`, forward-only) |
| Templates | `templates/` (`layouts/`, `pages/`, `components/`, `partials/`) |
| Frontend assets | `web/static/` (`css/`, `js/`, `vendor/`, `img/`) |
| Tailwind entry / build | `web/static/css/input.css`, `package.json` scripts (`build:css`, `watch:css`, `build:assets`) |
| Legal page text | `policies/{privacy,terms,cookies}.{pt-BR,en}.md` |
| Test harness & builders | `crates/test-support/src/`, `crates/test-macros/src/` |
| Integration/HTTP tests | `crates/*/tests/` (e.g. `infrastructure/tests/parking_test.rs`, `web/tests/http_test.rs`) |
| Domain unit tests | inline `#[cfg(test)] mod tests` in `crates/domain/src/*.rs` |
| Environment config | `.env.example` (single source of truth), parsed in `crates/infrastructure/src/config.rs` |
| Operations docs | `docs/` (`deployment.md`, `backups.md`, `incident-response.md`, `retention-policy.md`) |
| Legal/privacy docs | `docs/` (`legal-review.md`, `data-processing-inventory.md`, `provider-transfer-inventory.md`) |
| Visual design | `design-system/` (tokens, kit, imagery) |

## Commands

```bash
cargo build                              # self-contained; no DB needed
cargo run                                # serve (default subcommand), BIND_ADDR (:8080)
cargo run -p bikesnest-web -- seed-mock   # dev: 24 sample Curitiba locations + photos
cargo run -p bikesnest-web -- seed-admin  # create admin (ADMIN_EMAIL/ADMIN_PASSWORD)
cargo run -p bikesnest-web -- seed-policies  # version legal pages (POLICY_* env)
cargo run -p bikesnest-web -- retention   # run the retention purge job

cargo test                               # domain + application (no DB)
docker compose up -d db                  # needed before DB-backed tests
cargo test --workspace                   # everything incl. #[db_test]

npm run build:assets                     # vendor htmx/alpine/maplibre into web/static/vendor
npm run build:css                        # Tailwind → web/static/css/app.css
```

## Conventions & gotchas

- **Lint:** `unsafe_code = "forbid"` (workspace lint). Keep it clean.
- **`#[db_test]`** runs against real Postgres; requires `docker compose up -d db`.
  Transaction-per-test with automatic rollback. See `TESTING.md`.
- **Repository tests:** prefer `let db = tx.db().await` before fixture queries.
  Seed through `db.acquire()`, release the lease, and inject `db.clone()` into
  repositories. Repository transactions use savepoints; the harness awaits
  outer rollback even after a panic. Adapters must use `Db::acquire()`, not
  `Db::pool()`. Account/review/export adapters are migrated; others still need
  migration. True multi-connection race tests need separate database isolation,
  not this single-connection scope. See `TESTING.md` for legacy pooled tests.
- **Subcommands** dispatch in `crates/web/src/main.rs`; default is `serve`.
  Add a new `Some("…")` arm there for a new CLI command.
- **Providers are wired in one place:** `crates/web/src/wiring.rs`
  (`app_router` / `app_router_with`). New ports get wired there, selected from
  env (see `crates/infrastructure/src/config.rs` + `.env.example`). Handlers
  under `crates/web/src/routes/` only ever see the ports held in `AppState` —
  a test in `crates/web/tests/http_test.rs` fails if one of them names a
  repository, a pool or a concrete adapter, and another fails if any file in
  `crates/web/src` passes 1200 lines.
- **Background jobs** (M9) are a Postgres-backed queue (`background_job` table)
  with an in-process worker; handlers live in `crates/infrastructure/src/job/`
  and implement `bikesnest_application::JobHandler`. Set `JOBS_ENABLED=false` for
  web-only instances. A test that claims jobs directly (rather than simulating
  a claim with a plain `UPDATE`) must use `SqlxJobRepository::claim_kinds` with
  a kind unique to that test, not the unscoped `claim` — see "Job-queue test
  isolation" in `TESTING.md`.
- **New fragment endpoints** need the `is_fragment_request` tests (a request
  without the htmx fragment headers gets a 303 to the whole page, not a bare
  partial — see the `p3_fragment_endpoints_*`/`moderation_fragment_endpoints_*`
  tests in `crates/web/tests/http_test.rs` for the pattern to copy).
- **New colour tokens** go in `web/static/css/input.css`'s `@theme` block
  before a template uses them —
  `no_undefined_tailwind_color_tokens_remain_in_templates` (`http_test.rs`)
  fails on any `text|bg|border|...-<name>` utility whose `<name>` isn't a
  defined token, a Tailwind colour keyword, or on that test's own
  non-colour-utility allowlist.
- **Legacy spec/ledger references:** some `docs/`, `.env.example`,
  code comments and templates still carry section numbers from the
  now-removed spec and references to the old milestone plan's bookkeeping as
  historical annotations. There is no such document anymore. **Whenever you edit
  a file that contains one of these, delete it** (rephrase the surrounding text
  if needed so it still reads naturally). This is a slow, file-by-file cleanup —
  do not do a bulk sweep, just clean whatever you happen to touch.

## If you're asked to change behavior

1. Find the relevant use case in `crates/application`.
2. Check the port it uses; implement any new persistence/provider in
   `crates/infrastructure`, wire it in `crates/web/src/wiring.rs`.
3. Add a migration if the schema changes.
4. Add/extend tests per `TESTING.md`.
5. Update `ARCHITECTURE.md`/`AGENTS.md` if you add a module or concept, and
   `.env.example` if you add a config knob.
