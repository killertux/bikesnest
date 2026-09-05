# Deployment

> **What this covers:** getting the `bikesnest-web` server into production. The
> runtime queries are **not** compile-time checked, so the Docker image
> builds with **no database** — `cargo build` is self-contained. Everything else
> (secrets, providers, TLS, migration, health) is env-driven and documented here.

---

## 1. Build the image

```bash
docker build -t bikesnest:$(git rev-parse --short HEAD) .
```

The multi-stage `Dockerfile` (`rust:1.95` builder → `debian:bookworm-slim`) bakes
the release binary and `web/static/`. Templates and migrations are **embedded**
(Askama / `sqlx::migrate!`), so nothing else is copied. Uploaded media lives in an
**S3-compatible bucket** (MinIO in dev), not the image; the bucket is the
configured store (`S3_*` env). Media is served via **direct S3 presigned GET
URLs** (the browser hits the bucket; the app is not a media proxy).

Build is reproducible because `Cargo.lock` is committed and the toolchain is
pinned by the base image tag. No `DATABASE_URL`, no offline cache, no build-time
DB.

## 2. Required environment

All knobs are documented in `.env.example`; production sets them as real secrets
(or a secret manager mounted as env). The table below is what matters most.

| Variable | Notes |
|---|---|
| `DATABASE_URL` | Postgres DSN. Example `postgres://user:pass@db:5432/bikesnest` |
| `DB_MAX_CONNECTIONS` | pool ceiling per instance (default `10`). Size it as the database's `max_connections` divided by the replica count, minus headroom for migrations/psql |
| `DB_STATEMENT_TIMEOUT_MS` | `statement_timeout` on every pooled connection (default `5000`). Migrations are exempt; a long maintenance run through the app — e.g. `retention` on a large database — may need a higher value |
| `DB_IDLE_IN_TX_TIMEOUT_MS` | `idle_in_transaction_session_timeout` (default `10000`), so a transaction abandoned by a crashed client releases its connection |
| `BIND_ADDR` | default `0.0.0.0:8080` |
| `TRUSTED_PROXY_HOPS` | how many reverse proxies in front of the app may be trusted to have appended to `X-Forwarded-For`; `0` (default) uses the TCP peer address only. See the reverse-proxy guidance below |
| `BASE_URL` | the public origin, e.g. `https://bikesnest.com` — builds links + canonical URLs. **Must be reachable** |
| `MEDIA_ROOT` | directory the **development e-mail outbox** writes to (`EMAIL_PROVIDER=fake` only; default `media`). No longer a media directory: media lives in the S3 bucket, and the retention orphan sweep lists the bucket |
| `S3_ENDPOINT` | **Object storage:** the S3-compatible endpoint. Unset defaults to `http://localhost:9000` (MinIO) in development only; set it empty for the standard AWS endpoint. **Required in production** |
| `S3_REGION` / `S3_BUCKET` | region (default `us-east-1`) + bucket name (development default `bikesnest`; **required in production**) |
| `S3_ACCESS_KEY_ID` / `S3_SECRET_ACCESS_KEY` | S3 credentials (development default MinIO `minioadmin`, which production rejects outright) |
| `TLS_ON` | set `true` to emit HSTS behind a real TLS terminator |
| `VALKEY_URL` | **Rate limiter:** single node, e.g. `valkey://valkey:6379`. Shared across auth/photo/contribution/moderation, survives restarts, aggregates across instances |
| `VALKEY_CLUSTER_URLS` | comma-separated node URLs → **cluster** mode (wins over `VALKEY_URL`) |
| `RATE_LIMIT_FAIL_OPEN` | `true` (default) → a ValKey outage **allows** requests (goes fail-open); `false` → **denies** (429s the rate-limited endpoints) |
| `JOBS_ENABLED` | **Background job queue:** `true` (default) spawns an in-process worker that claims, runs, and retries `background_job` rows; `false` for web-only instances — transactional email then sends inline on the request path instead of being queued |
| `JOBS_POLL_INTERVAL_MS` / `JOBS_BATCH_SIZE` / `JOBS_LEASE_TTL_MS` | queue poll cadence, batch size, and lease length (defaults 5000 / 4 / 600000) |
| `JOBS_MAX_ATTEMPTS` / `JOBS_BACKOFF_BASE_MS` | retry budget (default 5) and exponential-backoff base (default 2000) before dead-letter |
| `JOBS_HISTORY_RETENTION_DAYS` | `jobs.gc` deletes `succeeded`/`failed` rows older than this (default 7) |
| `CSP_TILE_HOSTS` / `CSP_GEOCODE_HOSTS` | extra origins allowed by the strict CSP for MapLibre tiles / browser geocoding. Required Mapbox and Google Maps origins are added automatically for their profiles |
| `CSP_MEDIA_HOSTS` | object-storage origin(s) allowed in the CSP `img-src` that parking photos are served from as direct pre-signed URLs (dev: `http://localhost:9000`; AWS: `https://<bucket>.s3.<region>.amazonaws.com`) |
| `APP_ENV` | `production` → JSON structured logs (machine-parseable, forward to a log aggregator) **and the startup validation described below** |
| `STATIC_ROOT` | directory `/static` is served from; the image sets `/app/web/static`. Unset falls back to `web/static` beside the working directory, then to the compile-time path |
| `LOCATION_PROVIDER` | **Location stack:** `google` \| `mapbox` \| `fake` (default `fake`). One value selects direct geocoding, autocomplete, suggestion resolution, and map rendering |
| `MAPBOX_GEOCODING_ACCESS_TOKEN` | server-side Mapbox geocoding token; required by the Mapbox profile |
| `MAPBOX_MAP_ACCESS_TOKEN` / `MAPBOX_STYLE_URL` | URL-restricted browser token and Mapbox GL style for the Mapbox profile |
| `GOOGLE_MAPS_SERVER_API_KEY` | server key for Geocoding API and Places API (New); required by the Google profile |
| `GOOGLE_MAPS_BROWSER_API_KEY` / `GOOGLE_MAP_ID` | HTTP-referrer-restricted Maps JavaScript API key and cloud map ID; required by the Google profile |
| `RATE_GEOCODE_PER_IP` / `RATE_GEOCODE_WINDOW_SECS` | shared per-IP budget for provider-backed direct searches, autocomplete, and place resolution (default 60 per 15 min); over budget the endpoint answers 429 before calling the provider |
| `EMAIL_PROVIDER` | `smtp` or `resend` in production (not `fake`) |
| `SMTP_*` / `RESEND_API_KEY` / `RESEND_FROM` | the chosen email backend |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | `seed-admin` bootstrap (run once) |
| `POLICY_OPERATOR_NAME` / `POLICY_OPERATOR_CNPJ` / `POLICY_OPERATOR_ADDRESS` / `POLICY_CONTACT_EMAIL` | **Legal pages**: the controller's legal name, CNPJ, registered address and privacy contact e-mail, substituted into `policies/*.md` by `seed-policies`. The seeder refuses to run with any of them unset |
| `POLICY_VERSION` / `POLICY_EFFECTIVE_AT` | version label + effective date of the policy text being seeded; bump the version whenever `policies/*.md` change |
| `DELETED_ACCOUNT_PURGE_AFTER_DAYS` | `30` in production (decision, `docs/retention-policy.md`); `INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS` stays `0` |
| `REC_*`, `FRESHNESS_*`, `PHOTO_*`, `MOD_*`, `RETENTION_*` | tuning constants (see `.env.example`) |

**Never** put secrets in the image; the `.dockerignore` excludes `.env*`.

## 2a. Startup validation

The whole environment is parsed once, at startup, into one typed `Config` — no
setting is re-read per request. With `APP_ENV=production` the process then
validates that configuration and **refuses to start** (exit code 1, one line per
problem on stderr) unless all of the following hold:

- `BASE_URL` is set and does not point at `localhost` / `127.0.0.1` — otherwise
  every verification and password-reset e-mail links to the wrong host.
- `S3_ENDPOINT`, `S3_BUCKET`, `S3_ACCESS_KEY_ID` and `S3_SECRET_ACCESS_KEY` are
  all set, and the credentials are not the MinIO development default
  (`minioadmin`).
- `EMAIL_PROVIDER` is `smtp` or `resend`, with its credentials present. The
  `fake` provider discards every message, so production never runs on it.
- `LOCATION_PROVIDER=mapbox` or `google`, with that profile's credentials. The
  fake geocoder fabricates coordinates for unknown queries.
- `VALKEY_URL` or `VALKEY_CLUSTER_URLS` is set. The in-memory limiter is
  per-process, so N replicas would multiply every rate limit by N.
- `TLS_ON=true`, so `Strict-Transport-Security` is emitted.
- `CSP_MEDIA_HOSTS` names the object-storage origin photos are served from;
  without it the CSP blocks every photo.
- `GOOGLE_OAUTH_ENABLED=false` — only the deterministic fake provider exists.

Every failing rule is reported in one run, so a misconfigured deploy is fixed in
one pass rather than one restart per missing variable. Independently of
`APP_ENV`, asking for a provider without its credentials (e.g.
`EMAIL_PROVIDER=resend` with no `RESEND_API_KEY`, or a ValKey URL that cannot be
reached) is a hard startup error — the app never silently downgrades to a fake.

Development runs no validation; it logs a `warn!` naming each fake in use.

## 3. TLS, reverse proxy, health checks

Terminate TLS at a reverse proxy / LB (Caddy, Traefik, nginx, or a cloud LB) and
set `TLS_ON=true` so the app emits `Strict-Transport-Security`. The app itself
listens plaintext on `BIND_ADDR`.

Health/readiness endpoints:

- `GET /healthz` → liveness (process up). Wire to the LB's health check.
- `GET /readyz` → readiness (DB reachable + migrations applied). Wire to the LB's
  readiness gate so `readyz` fails during a migration before rollout completes.

### Client address behind the proxy (`TRUSTED_PROXY_HOPS`)

Every per-address rate limit (login, registration, password reset, photo upload,
reports) is keyed on the client address the app resolves. `X-Forwarded-For` is a
plain request header — anyone can send one, and anyone can send a *different* one
per request — so the app ignores it unless you say how many proxies are in front
of it:

- `TRUSTED_PROXY_HOPS=0` (default): the TCP peer address, and nothing else.
  Correct when the app is directly exposed. **Behind a proxy this keys every
  client on the proxy's address**, so one shared bucket for everyone — set the
  real value.
- `TRUSTED_PROXY_HOPS=N`: each of the N proxies appends the address it saw, so
  the entry N places from the **right** is the address the outermost trusted
  proxy received the request from. One load balancer → `1`; a CDN in front of a
  load balancer → `2`.

Set it to the *exact* number: too high lets clients forge their own address by
prepending entries, too low keys everyone on a proxy. A chain shorter than N
entries, or an entry that is not a bare IP address, falls back to the peer
address. `X-Real-IP` is never read (it is not standardised and carries no hop
count). The app also needs the peer address itself, which it gets from the TCP
connection — no configuration needed.

### Shutdown (SIGTERM) and PID 1

`SIGTERM` (what `docker stop` / Kubernetes send) starts a graceful shutdown: the
HTTP server stops accepting, in-flight requests drain, then the background job
worker is given up to 30 s to finish whatever job it is running before the
process exits. Killing the process mid-job would leave a `background_job` row in
`state='running'` until its lease expired.

The container image runs the server under [tini] as PID 1
(`ENTRYPOINT ["/usr/bin/tini", "--", "bikesnest-web"]`) so signals are forwarded
and zombies reaped. If you run the binary some other way, make sure it receives
`SIGTERM` directly (`docker run --init`, or `init: true` in compose, gives the
same guarantee) and allow at least 35 s of termination grace
(`--stop-timeout` / `terminationGracePeriodSeconds`) so the worker's 30 s budget
is usable.

[tini]: https://github.com/krallin/tini

## 4. Migrations

The server runs `sqlx::migrate!` **on startup** (the default subcommand `serve`).
Migrations are **forward-only** (`sqlx` records applied versions). This means:

- **Deploy = run the new image.** The migration runs before the server accepts
  traffic (`readyz` gates until migrations are applied).
- **Rollback = redeploy the previous image.** Because migrations are
  forward-only, a *rollback* is a restore of the previous release, **not** an
  automated down-migration. New columns introduced by the rolled-back release
  become harmless (ignored) but remain in the schema.

> If a release must be undone and the schema is incompatible, restore the
> pre-release data backup and re-run the old image — see `docs/backups.md`.

Migrations run on a dedicated connection with `statement_timeout` disabled and
closed afterwards, so `DB_STATEMENT_TIMEOUT_MS` never aborts an index build
on a cold database, and the relaxed setting never leaks back into request
handling.

**PostGIS prerequisite.** `0001_init.sql` runs `CREATE EXTENSION IF NOT EXISTS
postgis`, which requires the PostGIS extension to be *installed* on the target
Postgres instance, not merely permitted by SQL — on a self-managed box that
means the `postgresql-*-postgis-*` package (or an image that bundles it, e.g.
`postgis/postgis`); on a managed provider (RDS, Cloud SQL, etc.) it means
adding `postgis` to that provider's extension allowlist *before* the first
deploy. Missing this fails the very first migration, not a later one.

**Building new indexes on a live database.** Every `CREATE INDEX` in a
migration runs inside `sqlx`'s migration transaction, which takes a normal
(non-concurrent) lock for the build's duration — acceptable against an empty
or small table (dev, first deploy), but a normal-priority lock on a large,
already-populated table (e.g. `parking_location`, `report`, `audit_events`)
blocks writers for as long as the build takes. For an index migration against
a table with real production volume, build the equivalent index with `CREATE
INDEX CONCURRENTLY` by hand, out of band, *before* shipping the release that
adds it as a plain (transactional) migration — `CONCURRENTLY` cannot run
inside a transaction block, so it is never something a `migrations/*.sql`
file can do on its own. If a concurrent build fails partway (it can leave an
`INVALID` index behind), `DROP INDEX` the invalid one and retry rather than
letting the later transactional migration collide with it.

## 5. Providers

The map, geocoder, email, OAuth, and object-storage integrations are selected at
wiring time from environment variables. The Google OAuth development fake is
documented below and must be replaced before enabling sign-in in production.

**Location stack.** `LOCATION_PROVIDER` selects all address and map behavior:

- `fake` — deterministic development geocoder plus MapLibre/OpenFreeMap.
- `mapbox` — `MapboxGeocoder` provides direct search and up to ten ranked
  autocomplete results. Mapbox GL JS renders `MAPBOX_STYLE_URL`; use separate
  least-privilege keys for server geocoding and browser rendering.
- `google` — `GoogleGeocoder` uses Geocoding API, Places Autocomplete (New),
  and Place Details. The browser uses Maps JavaScript API with advanced markers.
  Autocomplete predictions and the selected Place Details request share a
  random session token. Google requires a browser key, a server key, and a map
  ID; absence of any one is a startup error.

The hosted geocoder sees the typed address and, for Google autocomplete, the
random session token. It receives no BikesNest account identity, cookie, or
direct connection from the browser. The Google map renderer receives normal
browser request metadata and the viewed map area. See
`docs/provider-transfer-inventory.md` before selecting either hosted profile.

Hosted results are kept out of the generic in-process cache so provider storage
terms are respected. `RATE_GEOCODE_PER_IP` and `RATE_GEOCODE_WINDOW_SECS` bound
direct searches, autocomplete, and selection resolution before an external call.
Coordinates already submitted by the browser skip direct geocoding. Provider
errors render the localized location-service unavailable state.

**Object storage.** Media is stored in an S3-compatible bucket
(MinIO in dev, AWS/S3/R2/B2 in prod; `S3_*` env) and served via **direct S3
presigned GET URLs** — the browser hits the bucket and S3's SigV4 signature
authorizes the read (no app-side proxy, no app signing secret). Selectable by
`S3_ENDPOINT`/`S3_BUCKET`; the compose MinIO is the DEVELOPMENT default only —
production must set every `S3_*` value (see “Required environment” above).

**Email — done in code.** Provider is selected by `EMAIL_PROVIDER`
(`fake` | `smtp` | `resend`, default `fake`; dev uses `smtp` → Mailpit). Asking
for `smtp`/`resend` without its credentials is a startup error, never a silent
fallback to the fake. For
production set `EMAIL_PROVIDER=resend` + `RESEND_API_KEY`/`RESEND_FROM`, or
`smtp` + `SMTP_*`. Only the production relay/ESP credentials remain (ops).
Delivery itself goes through the background job queue described below.

**Google OAuth still uses a development implementation.** It remains disabled
in production independently of Google Maps Platform; the two use separate
configuration and credentials.


## 5b. Rate limiter (ValKey)

The rate limiter is a sliding-window counter shared by auth, photo, contribution
and moderation. Dev uses an in-memory limiter; production should run ValKey
(Redis-compatible), which aggregates limits across instances and survives
restarts. The app picks the backend from env (no code change):

- no `VALKEY_URL`/`VALKEY_CLUSTER_URLS` → in-memory (default, dev/test only — production refuses to start on it).
- `VALKEY_URL` → `ValKeyRateLimiter::single` (one node).
- `VALKEY_CLUSTER_URLS` → `ValKeyRateLimiter::cluster` (a real cluster; the
  `redis-rs` `ClusterClient` auto-discovers nodes and routes each key to its
  owning slot). Cluster wins over `VALKEY_URL`.

**Atomicity:** each `check` runs a Lua script against a ValKey sorted set
(`ZREMRANGEBYSCORE` → `ZCARD` → `ZADD`), so the trim+count+record is atomic and
correct under concurrency, including in cluster mode (the script touches a
single key, so it stays within one hash slot).

**Failure mode — fail open by default.** `Check` on a ValKey outage returns
*allow* and logs a `warn!` (`RATE_LIMIT_FAIL_OPEN=true`), so a ValKey outage
degrades brute-force protection without taking the site down (the application
maps any `RateLimitError` to 429 — fail closed — which would 429 every
rate-limited endpoint during an outage). Set `RATE_LIMIT_FAIL_OPEN=false` to
fail closed instead (stricter, but an outage 429s auth/photo/moderation).

**Docker compose:** the dev stack runs a single-node ValKey
(`docker-compose.yml`, `valkey` service, wired as `VALKEY_URL`). For cluster
mode use `docker-compose.valkey-cluster.yml` (a cluster-enabled node covering
all slots — portable on Docker Desktop for Mac; see the file header for a
multi-node variant).


## 5c. Background jobs

The app ships a **pure-PostgreSQL job queue** — no broker. A `background_job`
table stores durable one-shot + recurring work; an **in-process worker task**
(started when `JOBS_ENABLED=true`, the default) claims due jobs with
`FOR UPDATE SKIP LOCKED`, runs their handler, and records the outcome. All job
times are UTC.

- **Recurring** jobs never go terminal: on success the worker recomputes
  `run_at` from `schedule` (`{"every_seconds":N}` or a UTC `{"cron":"…"}`) and
  resets the row to `pending`.
- **Retries** use exponential backoff + jitter; after `JOBS_MAX_ATTEMPTS` a job
  is dead-lettered to `failed` (kept with `last_error` for inspection).
- **`jobs.gc`** (itself a recurring job) deletes `succeeded`/`failed` rows older
  than `JOBS_HISTORY_RETENTION_DAYS` (default 7).
- **At-least-once**: a worker crash leaves the job leasable; it is re-claimed
  after the lease. Handlers must be idempotent.
- On a multi-instance deploy each instance runs its own worker; claims are safe
  because `SKIP LOCKED` assigns disjoint rows. `JOBS_ENABLED=false` keeps an
  instance web-only (no worker).

The always-on recurring jobs (`retention`, `jobs.gc`) are bootstrapped by the
worker at startup (idempotent via a stable `idempotency_key`), so no manual
seeding is required. The legacy `cargo run -- retention` subcommand still works
as a manual escape hatch.

## 5d. Transactional email goes through the queue

Verification, password-reset and e-mail-change messages are **queued, not sent
inline**. A request writes one `email.send` job (a single INSERT, in the same
database as the account and token rows) and returns; the worker delivers it with
the queue's retry budget (`JOBS_MAX_ATTEMPTS`), exponential backoff and
dead-lettering. A slow or failing relay/ESP therefore cannot hold an HTTP
request open, and cannot fail a registration *after* the account already exists.

- **Language.** The message carries the recipient's locale (`users.locale`, set
  at registration from the page's language and updated by the header language
  toggle for signed-in users). Subject and body are rendered from the message
  catalog *at send time* — pt-BR and en, never a hard-coded English string.
- **No double sends.** Each job is enqueued under `email:{kind}:{sha256(link)}`,
  so a retried or double-submitted request collapses onto the existing row. A
  genuine re-send issues a new token, hence a new link and a new job.
- **Dead letters.** An exhausted job logs at `error!` with the message kind and
  the recipient's *domain* only (never the address, never the link) and stays in
  `background_job` as `failed` with `last_error` until `jobs.gc` removes it.
  Alert on that log line: it means someone is stuck without a verification or
  reset link and needs a re-send.
- **`JOBS_ENABLED=false`.** No worker runs, so nothing would ever claim an
  `email.send` row. The app detects this at wiring time and sends **inline** on
  the request path instead (same provider, same localized rendering) — mail is
  never silently queued into a void. The trade-off returns with it: a slow ESP
  is back on the user's request. Prefer leaving the worker on; if you run
  web-only instances, make sure at least one instance (or a dedicated worker
  deployment) has `JOBS_ENABLED=true`. Startup validation needs no new rule
  here: both wirings deliver, so neither is a misconfiguration.

## 6. Rolling deploy + rollback

1. Build the new image (tagged with the commit SHA).
2. Push to the registry; deploy the new image to one instance.
3. Wait for `readyz` to go green on that instance (migrations applied).
4. Drain the old instance; promote the new one.
5. On failure: stop the rollout, redeploy the **previous** image tag, and if the
   schema is incompatible restore the pre-release backup (see /`docs/backups.md`).

## 6a. Legal pages (privacy / terms / cookies)

The versioned legal pages are stored in `policy_version` and seeded from
`policies/{privacy,terms,cookies}.{pt-BR,en}.md`:

1. Set `POLICY_OPERATOR_NAME`, `POLICY_OPERATOR_CNPJ`, `POLICY_OPERATOR_ADDRESS`
   and `POLICY_CONTACT_EMAIL` (the privacy inbox must be monitored — rights
   requests and takedown notices arrive there).
2. Set `POLICY_VERSION` (e.g. `2026-09-05.1`) and `POLICY_EFFECTIVE_AT`.
3. Run `bikesnest-web seed-policies` once per release that changes the text. It is
   idempotent per `(kind, locale, version)`; a new version supersedes the current
   one and the old text stays reachable at `/{privacy,terms,cookies}/versions`.
4. Material changes must be announced to users (e-mail or in-app notice) before
   `POLICY_EFFECTIVE_AT` — the policies promise that.

Review status of the text itself: `docs/legal-review.md`.

## 7. Logging & retention

- `APP_ENV=production` → JSON structured logs (one line per request via
  `TraceLayer`: method/path/status/latency; **headers never logged**, so no
  cookie/token/PII). PII-free `info`/`warn` events at key boundaries.
- Forward stdout/stderr to a log driver (Docker/CloudWatch/journald/Syslog).
- **Access logs: keep 6 months.** As a Brazilian company operating an internet
  application, art. 15 of the Marco Civil da Internet (Lei 12.965/2014) requires
  the *registros de acesso a aplicações* — date/time of access + the client IP —
  to be kept for **6 months** under confidentiality in a controlled environment.
  The app's own request logs do not include the client IP, so satisfy this at
  the reverse proxy / LB access log (or the hosting provider's equivalent):
  retain those logs for 6 months, access-controlled, then delete. Everything
  else (diagnostic logs) can stay at ~30 days. The privacy policy states the
  6-month period; keep them aligned (`docs/retention-policy.md`).
- Separate **audit events** (the `audit_events` table, `action` codes like
  `photo.*`, `report.*`, `privacy.*`, `retention.*`) from diagnostic logs — audit
  rows are queryable and protected (see `docs/incident-response.md`).
