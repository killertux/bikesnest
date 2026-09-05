# BikesNest

A community-maintained map of bicycle parking. BikesNest helps cyclists find a
safe, suitable place to leave their bike near a destination — and lets the
community keep that information accurate.

## What it does

Cyclists search an address or place, see nearby bicycle parking on a map, and
compare the things that matter when choosing where to park:

- distance from the destination
- cost (free / paid / unknown)
- parking type (rack, shelter, station, …)
- security features (locking point, CCTV, lighting, access, …)
- opening hours (with a live "open now")
- photos
- community ratings, verification confidence, and how fresh the information is

Everyone can browse. Signed-in users can contribute: add a new spot, edit
details, propose a change that needs review, write a review, verify "this still
exists", mark "I parked here", upload photos, and report problems. Moderators
review photos, proposals and reports through a dashboard; admins manage roles,
users and the audit trail. Every user can export their data and delete their
account (anonymization).

BikesNest is a server-rendered web app that works on desktop and mobile
browsers, in **English** and **Brazilian Portuguese**.

## How it works, in one paragraph

A Rust web server (axum) renders server-side pages (Askama templates) and
serves small interactive fragments via [htmx](https://htmx.org), with
[Alpine.js](https://alpinejs.dev) for client-side behavior and
a selectable Google Maps, Mapbox GL, or MapLibre renderer. Business rules live in a clean,
framework-free core; PostgreSQL + PostGIS does the geospatial search and stores
the data. Everything external — geocoding, email, object storage, OAuth, rate
limiting — sits behind a small interface (a "port") so it can be swapped
without touching the rest of the app. See [`ARCHITECTURE.md`](ARCHITECTURE.md).

## Quick start — Docker Compose

Prerequisites: **Docker** (the compose stack also needs the Rust and Node
images it pulls; no host Rust/Node install is required).

```bash
cp .env.example .env            # sensible defaults; adjust if ports are taken
docker compose up -d            # postgres+postgis, app, tailwind watcher, mailpit, valkey, minio
docker compose logs -f app      # watch it compile & start (first start takes a few minutes)
```

Compose injects the complete `.env` into the app container, so provider,
policy, job, retention, map, and tuning settings apply there too. The
`COMPOSE_*` variables in `.env.example` are reserved for addresses that differ
inside Docker (`db`, `minio`, `valkey`, and `mailpit`).

Open **http://localhost:8080**. The stack starts:

| Service | Role | Where |
|---|---|---|
| `db` | PostgreSQL 17 + PostGIS | `localhost:5432` |
| `app` | the BikesNest server (auto-recompiles) | `localhost:8080` |
| `css` | Tailwind CSS watcher | — |
| `mailpit` | catches every outgoing email | `http://localhost:8025` |
| `valkey` | rate-limit store (Redis-compatible) | `localhost:6380` |
| `minio` | S3-compatible object storage for photos | `http://localhost:9001` |

Once the app is up, seed development data (optional, dev only — production
starts empty):

```bash
docker compose exec app cargo run -q -p bikesnest-web -- seed-mock        # sample Curitiba locations + photos
docker compose exec app cargo run -q -p bikesnest-web -- seed-admin       # create an admin (set ADMIN_EMAIL/ADMIN_PASSWORD in .env first)
docker compose exec app cargo run -q -p bikesnest-web -- seed-full-fresh  # erase data, then seed mock data, admin, and policies
```

Other useful commands:

```bash
docker compose up -d db         # database only — run the app on the host instead
docker compose exec app cargo run -q -p bikesnest-web -- seed-policies    # version the legal pages (privacy/terms/cookies)
docker compose down -v          # wipe the database and volumes, start clean
```

To run only the database in Docker and the app on your host:

```bash
cp .env.example .env
docker compose up -d db
npm install
npm run build:assets            # vendor htmx/alpine/maplibre (committed; only when they change)
npm run build:css               # build Tailwind (committed; only when styles change)
cargo run                       # serve on BIND_ADDR (default :8080)
```

> `cargo build` needs **no database** — SQL is checked at runtime, not compile
> time — so building and serving are self-contained.

Check it's alive:

```bash
curl localhost:8080/healthz     # liveness  → "ok"
curl localhost:8080/readyz      # readiness → {"status":"ready","database":"up"}
```

## Running in production

Build a self-contained release image (no database needed at build time):

```bash
docker build -t bikesnest .
```

Run it with real configuration as environment variables (or a secret manager):

```bash
docker run --rm -p 8080:8080 \
  -e DATABASE_URL=postgres://user:pass@db:5432/bikesnest \
  -e BASE_URL=https://bikesnest.com \
  -e APP_ENV=production \
  -e EMAIL_PROVIDER=resend -e RESEND_API_KEY=... -e RESEND_FROM=... \
  -e S3_ENDPOINT= -e S3_REGION=us-east-1 -e S3_BUCKET=bikesnest \
  -e S3_ACCESS_KEY_ID=... -e S3_SECRET_ACCESS_KEY=... \
  -e VALKEY_URL=valkey://valkey:6379 \
  -e LOCATION_PROVIDER=mapbox \
  -e MAPBOX_GEOCODING_ACCESS_TOKEN=... \
  -e MAPBOX_MAP_ACCESS_TOKEN=... \
  bikesnest
```

Things to know for a production run:

- **Migrations run automatically on startup** (`serve` is the default command),
  before the server accepts traffic. They are forward-only; rollback = redeploy
  the previous image (or restore a backup if the schema is incompatible).
- **TLS** terminates at a reverse proxy / load balancer; the app speaks plain
  HTTP on `BIND_ADDR`. Set `TLS_ON=true` so it emits `Strict-Transport-Security`.
- **Health checks:** wire `GET /healthz` (liveness) and `GET /readyz`
  (readiness — fails until the DB is reachable and migrations are applied) to
  your load balancer.
- **Media** lives in an S3-compatible bucket (`S3_*`), served via presigned
  URLs — the app is not a media proxy.
- **Email** must be `smtp` or `resend` (not `fake`).
- **Rate limiting** should use ValKey (`VALKEY_URL` or `VALKEY_CLUSTER_URLS`)
  so limits aggregate across instances.
- **One-time setup:** run `seed-admin` once to create the admin account, and
  `seed-policies` once per release that changes the legal text.

The full runbook (environment, TLS, migrations, health checks, rolling deploy +
rollback, background jobs, legal pages) is in
[`docs/deployment.md`](docs/deployment.md). Backups & restore:
[`docs/backups.md`](docs/backups.md). Data retention:
[`docs/retention-policy.md`](docs/retention-policy.md). Security incidents:
[`docs/incident-response.md`](docs/incident-response.md).

## Configuration

Every knob is documented inline in [`.env.example`](.env.example) (the single
source of truth). The ones you'll most often touch:

| Variable | Purpose | Default |
|---|---|---|
| `DATABASE_URL` | PostgreSQL DSN (required) | `postgres://bikesnest:bikesnest@localhost:5432/bikesnest` |
| `BIND_ADDR` | HTTP bind address | `0.0.0.0:8080` |
| `BASE_URL` | public origin, used to build links + canonical URLs | `http://localhost:8080` |
| `APP_ENV` | `development` (human logs) or `production` (JSON logs) | `development` |
| `RUST_LOG` | tracing log filter | `info` |
| `EMAIL_PROVIDER` | `fake` \| `smtp` \| `resend` | `fake` |
| `SMTP_HOST/PORT/USERNAME/PASSWORD/TLS` | SMTP backend | — |
| `RESEND_API_KEY` / `RESEND_FROM` | Resend backend | — |
| `S3_ENDPOINT/REGION/BUCKET/ACCESS_KEY_ID/SECRET_ACCESS_KEY` | object storage (empty endpoint = AWS) | MinIO defaults |
| `VALKEY_URL` / `VALKEY_CLUSTER_URLS` | shared rate limiter (unset = in-memory) | unset |
| `RATE_LIMIT_FAIL_OPEN` | allow (true) or 429 (false) if ValKey is down | `true` |
| `LOCATION_PROVIDER` | `google` \| `mapbox` \| `fake`; selects geocoding, autocomplete, and maps | `fake` |
| `MAPBOX_GEOCODING_ACCESS_TOKEN` / `MAPBOX_MAP_ACCESS_TOKEN` | Mapbox server and browser tokens | — |
| `MAPBOX_STYLE_URL` | Mapbox map style | Mapbox Streets v12 |
| `GOOGLE_MAPS_SERVER_API_KEY` | Google Geocoding and Places server key | — |
| `GOOGLE_MAPS_BROWSER_API_KEY` / `GOOGLE_MAP_ID` | Google Maps browser key and map ID | — |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | `seed-admin` bootstrap | — |
| `JOBS_ENABLED` / `JOBS_*` | background job worker (Postgres-backed) | enabled |
| `POLICY_OPERATOR_*` / `POLICY_CONTACT_EMAIL` / `POLICY_VERSION` / `POLICY_EFFECTIVE_AT` | legal pages seeding | — |
| `DELETED_ACCOUNT_PURGE_AFTER_DAYS` | retention: purge deleted shells | `30` |
| `INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS` | retention: auto-anonymize (keep `0`) | `0` |
| `TLS_ON` | emit HSTS behind a TLS terminator | off |
| `CSP_TILE_HOSTS` / `CSP_GEOCODE_HOSTS` | origins allowed by the CSP for map tiles / geocoding | — |
| `REC_*`, `FRESHNESS_*`, `PHOTO_*`, `MOD_*`, `RETENTION_*` | tuning constants | documented values |

## Tests

```bash
cargo test                     # domain + application tests (no DB needed)
docker compose up -d db        # required for DB-backed tests
cargo test --workspace         # everything, incl. #[db_test] integration/HTTP tests
```

How tests are structured and how to write one: [`TESTING.md`](TESTING.md).
CI (`.github/workflows/ci.yml`) reports formatting, Clippy, workspace tests,
generated frontend assets, and Docker image validation as independent checks
on every PR and push to `main`.

## Repository layout

```text
crates/domain            pure business rules (no framework/database deps)
crates/application       use cases + ports (interfaces to infrastructure)
crates/infrastructure    sqlx persistence, config, providers, seeders, job worker
crates/web               axum routes/handlers, view models, i18n, templates (the binary)
crates/test-support      #[db_test] harness, pool fixture, test builders
crates/test-macros       #[db_test] proc macro
migrations/              SQLx migrations (forward-only, applied on startup)
templates/               Askama layouts / pages / components / partials
web/static/              compiled CSS, vendored JS, page JS, images
policies/                privacy/terms/cookies markdown (pt-BR + en)
media/                   local object-storage root (dev only)
design-system/           design system (tokens, components, imagery — source of truth for UI)
docs/                    runbook + operational/legal docs
```

Dependency direction: **`domain ← application ← {infrastructure, web}`** — see
[`ARCHITECTURE.md`](ARCHITECTURE.md).

## Further reading

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — how the code is organized, the domain model, and the provider ports.
- [`TESTING.md`](TESTING.md) — running and writing tests.
- [`AGENTS.md`](AGENTS.md) — orientation for AI coding agents (index + glossary).
- `design-system/` — the design system (tokens, components, imagery).
- [`docs/deployment.md`](docs/deployment.md), [`docs/backups.md`](docs/backups.md), [`docs/incident-response.md`](docs/incident-response.md), [`docs/retention-policy.md`](docs/retention-policy.md) — operations.
- [`docs/legal-review.md`](docs/legal-review.md), [`docs/data-processing-inventory.md`](docs/data-processing-inventory.md), [`docs/provider-transfer-inventory.md`](docs/provider-transfer-inventory.md) — privacy & legal.

## License

Proprietary / unlicensed (`UNLICENSED` in `Cargo.toml`).
