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
**S3-compatible bucket** (RustFS in dev), not the image; the bucket is the
configured store (`S3_*` env). Media is served via **direct S3 presigned GET
URLs** (the browser hits the bucket; the app is not a media proxy).

The shipped htmx 4.0.0 network-restores browser history and does not implement
a localStorage history snapshot cache. That is a pinned-library property guarded
by the browser suite, not an HTML configuration attribute; rerun that suite when
upgrading the vendored htmx asset.

HTML responses enforce the existing provider-compatible CSP and simultaneously
emit a fresh-nonce, `strict-dynamic` candidate as
`Content-Security-Policy-Report-Only`. This is deliberately observation-only:
promoting it to enforcement requires live restricted-key Google Maps evidence
and a decision to nonce or disable the Cloudflare-injected analytics beacon,
which a host fallback cannot authorize under `strict-dynamic`. Google Maps is
the only candidate profile retaining `unsafe-eval`, per the official
[Google Maps CSP guide](https://developers.google.com/maps/documentation/javascript/content-security-policy).
Mapbox blob-worker and stricter worker-bundle tradeoffs are documented in its
[security guide](https://docs.mapbox.com/mapbox-gl-js/guides/security-and-testing/).

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
| `PROBE_TIMEOUT_MS` | deadline for the `/readyz` database probe (default `2000`); a slower answer reports not-ready |
| `TRUSTED_PROXY_HOPS` | how many reverse proxies in front of the app may be trusted to have appended to `X-Forwarded-For`; `0` (default) uses the TCP peer address only. See the reverse-proxy guidance below |
| `BASE_URL` | the public origin, e.g. `https://bikesnest.com` — builds links + canonical URLs. **Must be reachable** |
| `MEDIA_ROOT` | directory the **development e-mail outbox** writes to (`EMAIL_PROVIDER=fake` only; default `media`). No longer a media directory: media lives in the S3 bucket, and the retention orphan sweep lists the bucket |
| `S3_ENDPOINT` | **Object storage:** the S3-compatible endpoint. Unset defaults to `http://localhost:9000` (RustFS) in development only; set it empty for the standard AWS endpoint. **Required in production** |
| `S3_PUBLIC_ENDPOINT` | browser-facing endpoint presigned GET URLs are signed for. Set it when `S3_ENDPOINT` is a private container/DNS name the browser cannot reach; defaults to `S3_ENDPOINT` |
| `S3_REGION` / `S3_BUCKET` | region (default `us-east-1`) + bucket name (development default `bikesnest`; **required in production**) |
| `S3_ACCESS_KEY_ID` / `S3_SECRET_ACCESS_KEY` | S3 credentials (development default RustFS `rustfsadmin`; production rejects it and `minioadmin` outright) |
| `TLS_ON` | set `true` to emit HSTS behind a real TLS terminator |
| `VALKEY_URL` | **Rate limiter:** single node, e.g. `valkey://valkey:6379`. Shared across auth/photo/contribution/moderation, survives restarts, aggregates across instances |
| `VALKEY_CLUSTER_URLS` | comma-separated node URLs → **cluster** mode (wins over `VALKEY_URL`) |
| `RATE_LIMIT_FAIL_OPEN` | `true` (default) lets general traffic fail open; credential-sensitive auth always fails closed. `false` fails closed everywhere |
| `JOBS_RUN_WORKER` / `JOBS_DURABLE_ENQUEUE` | independently run a worker and leave auth mail for durable delivery (both default true). Legacy `JOBS_ENABLED` sets both only when the new names are absent |
| `JOBS_POLL_INTERVAL_MS` / `JOBS_BATCH_SIZE` / `JOBS_LEASE_TTL_MS` | queue poll cadence, maximum concurrent attempts, and lease length (defaults 5000 / 4 / 600000) |
| `JOBS_HANDLER_TIMEOUT_MS` / `JOBS_SHUTDOWN_GRACE_MS` | attempt deadline and bounded shutdown drain (defaults 300000 / 30000) |
| `JOBS_MAX_ATTEMPTS` / `JOBS_BACKOFF_BASE_MS` | retry budget (default 5) and exponential-backoff base (default 2000) before dead-letter |
| `JOBS_HISTORY_RETENTION_DAYS` | `jobs.gc` deletes `succeeded`/`failed` rows older than this (default 7) |
| `PASSWORD_HASH_CONCURRENCY` / `PASSWORD_HASH_QUEUE_CAPACITY` / `PASSWORD_HASH_ADMISSION_TIMEOUT_MS` | shared Argon2 hash+verify running budget, finite waiter count, and waiter deadline (defaults 2 / 8 / 2000; queue 0 disables waiting) |
| `PHOTO_PROCESSING_CONCURRENCY` | process-wide simultaneous image decode/encode limit (default `1`). Keep at `1` until a release capacity test proves memory headroom for a higher value |
| `CSP_TILE_HOSTS` / `CSP_GEOCODE_HOSTS` | extra origins allowed by the strict CSP for MapLibre tiles / browser geocoding. Required Mapbox and Google Maps origins are added automatically for their profiles (Google's only on map pages) |
| `CSP_MEDIA_HOSTS` | object-storage origin(s) allowed in the CSP `img-src` that parking photos are served from as direct pre-signed URLs (dev: `http://localhost:9000`; AWS: `https://<bucket>.s3.<region>.amazonaws.com`) |

| `APP_ENV` | `production` → JSON structured logs (machine-parseable, forward to a log aggregator) **and the startup validation described below** |
| `STATIC_ROOT` | directory `/static` is served from; the image sets `/app/web/static`. Unset falls back to `web/static` beside the working directory, then to the compile-time path |
| `LOCATION_PROVIDER` | **Location stack:** `google` \| `mapbox` \| `fake` (default `fake`). One value selects direct geocoding, autocomplete, suggestion resolution, and map rendering |
| `MAPBOX_GEOCODING_ACCESS_TOKEN` | server-side Mapbox geocoding token; required by the Mapbox profile |
| `GEOCODER` / `MAP_STYLE_URL` / `MAPBOX_ACCESS_TOKEN` | **legacy aliases**, read only when `LOCATION_PROVIDER` is unset: `GEOCODER` (`fake` \| `mapbox` \| `google`) selects the geocoder, `MAP_STYLE_URL` selects the map renderer (a `mapbox://` or `api.mapbox.com` style selects Mapbox GL JS), and `MAPBOX_ACCESS_TOKEN` is the fallback for both Mapbox tokens. New deployments set `LOCATION_PROVIDER` and the explicit token names instead (see "Providers" below) |
| `MAPBOX_MAP_ACCESS_TOKEN` / `MAPBOX_STYLE_URL` | URL-restricted browser token and Mapbox GL style for the Mapbox profile |
| `GOOGLE_MAPS_SERVER_API_KEY` | server key for Geocoding API and Places API (New); required by the Google profile |
| `GOOGLE_MAPS_BROWSER_API_KEY` / `GOOGLE_MAP_ID` | HTTP-referrer-restricted Maps JavaScript API key and cloud map ID; required by the Google profile |
| `RATE_GEOCODE_PER_IP` / `RATE_GEOCODE_WINDOW_SECS` | shared per-IP budget for provider-backed direct searches, autocomplete, and place resolution (default 60 per 15 min); over budget the endpoint answers 429 before calling the provider |
| `EMAIL_PROVIDER` | `smtp` or `resend` in production (not `fake`) |
| `SMTP_*` / `RESEND_API_KEY` / `RESEND_FROM` | the chosen email backend |
| `SMTP_TLS` | `true` connects to the relay with STARTTLS (default `false`, which suits the dev Mailpit). Set it for every real relay |
| `ADMIN_EMAIL` / `ADMIN_PASSWORD` | `seed-admin` bootstrap (run once) |
| `POLICY_OPERATOR_NAME` / `POLICY_OPERATOR_CNPJ` / `POLICY_OPERATOR_ADDRESS` / `POLICY_CONTACT_EMAIL` | **Legal pages**: the controller's legal name, CNPJ, registered address and privacy contact e-mail, substituted into `policies/*.md` by `seed-policies`. The seeder refuses to run with any of them unset |
| `POLICY_VERSION` / `POLICY_EFFECTIVE_AT` | version label + effective date of the policy text being seeded; bump the version whenever `policies/*.md` change |
| `POLICY_ACKNOWLEDGEMENT_ENABLED` | fleet-wide gate for versioned terms acknowledgement at signup and the material-change notices (default `false`; must be an explicit boolean). Keep it `false` until the owner and counsel approve the workflow, and set the same value on every serving instance. Cannot be combined with `GOOGLE_OAUTH_ENABLED=true`. See `docs/policy-publication.md` |
| `POLICY_TERMS_MATERIAL_NOTICE` | `seed-policies` marks the seeded terms rows as a material change (default `false`; explicit boolean). It only marks rows; it sends no e-mail |
| `EXPORT_TTL_HOURS` | how long a personal-data export stays downloadable (default `24`) |
| `DELETED_ACCOUNT_PURGE_AFTER_DAYS` | `30` in production (decision, `docs/retention-policy.md`); `INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS` stays `0` |
| `REC_*`, `FRESHNESS_*`, `PHOTO_*`, `MOD_*`, `RETENTION_*` | tuning constants (see `.env.example`) |

Every `CSP_*` entry must be a bare origin — `http://` or `https://`, a host
(optionally `*.`-prefixed) and an optional `:port`, comma-separated. A path, a
`;`, whitespace, a control character or a CSP keyword fails startup with an
error naming the variable, in every environment.

The password default permits two simultaneous Argon2id operations. At the
unchanged 19 MiB memory parameter this is a nominal 38 MiB working-allocation
lower bound before allocator, thread, request, and process overhead. Eight
waiters are deliberately cheap compared with starting eight more hashes and
are bounded by a two-second deadline. These are conservative process defaults,
not a production latency SLO: measure the deployed CPU and memory limit, then
tune all three values together. Queue capacity zero is supported when immediate
overload rejection is preferred.

Image processing uses a separate semaphore with a conservative default of one
decode/encode at a time per process. A 20 MP input decodes to about 60 MB of RGB
pixels and an audit harness observed an approximately 151 MiB absolute
single-process high-water mark on its test runtime. That high-water mark is not
an additive per-request prediction. Before increasing
`PHOTO_PROCESSING_CONCURRENCY`, the release owner must record the deployed
container memory limit, replica/process layout, baseline RSS, and concurrent
20 MP load evidence with adequate headroom. Started blocking work retains its
permit after HTTP cancellation.

**Never** put secrets in the image; the `.dockerignore` excludes `.env*`.

## 2a. Startup validation

The whole environment is parsed once, at startup, into one typed `Config` — no
setting is re-read per request. With `APP_ENV=production` the process then
validates that configuration and **refuses to start** (exit code 1, one line per
problem on stderr) unless all of the following hold:

- `BASE_URL` is set and does not point at `localhost` / `127.0.0.1` — otherwise
  every verification and password-reset e-mail links to the wrong host.
- `S3_ENDPOINT`, `S3_BUCKET`, `S3_ACCESS_KEY_ID` and `S3_SECRET_ACCESS_KEY` are
  all set, and the credentials are not a development default
  (`rustfsadmin`/`minioadmin`).
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

### Reviewing CSP violation reports

Both the enforced `Content-Security-Policy` and the nonce-based
`Content-Security-Policy-Report-Only` candidate send violations to
`POST /csp-report` (`report-uri` for older browsers, `report-to` with the
`Reporting-Endpoints` header for the rest). The endpoint accepts
`application/csp-report` and `application/reports+json`, caps the body at
16 KiB, admits 30 reports per client IP per minute, and stores nothing. Each
accepted violation becomes one `WARN` log line, target `bikesnest::csp`,
message `csp violation reported`, with four redacted fields:

| Field | Content |
|---|---|
| `directive` | the effective directive name only (`script-src-elem`, `img-src`, …) |
| `blocked` | the blocked origin `scheme://host[:port]` (never a path or query), or the browser's keyword (`inline`, `eval`, `data`, `blob`) |
| `document` | the page path, with no query string or fragment |
| `disposition` | `enforce` (the live policy blocked it) or `report` (the candidate only) |

To review, filter the JSON logs on `message = "csp violation reported"` and
group by `disposition`, `directive` and `blocked`. `enforce` rows are breakage
users saw: an origin missing from `CSP_*` or a provider change. `report` rows
are what promoting the report-only candidate would break; it can be promoted
once a full release cycle shows only extension or edge noise there. A sudden
spike of `inline` or unknown origins on one `document` is worth treating as a
possible injection attempt.

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
worker is given the configured shutdown grace to finish active jobs before the
process exits. Killing the process mid-job would leave a `background_job` row in
`state='running'` until its lease expired.

The container image runs the server under [tini] as PID 1
(`ENTRYPOINT ["/usr/bin/tini", "--", "bikesnest-web"]`) so signals are forwarded
and zombies reaped. If you run the binary some other way, make sure it receives
`SIGTERM` directly (`docker run --init`, or `init: true` in compose, gives the
same guarantee). Set the container termination grace above the HTTP drain plus
twice `JOBS_SHUTDOWN_GRACE_MS`: the first worker interval permits natural
completion and the second drains explicitly cancelled handler/heartbeat tasks.

[tini]: https://github.com/krallin/tini

## 4. Migrations

The server runs `sqlx::migrate!` **on startup** (the default subcommand `serve`).
Migrations are **forward-only** (`sqlx` records applied versions). This means:

- **Deploy = run the new image.** The migration runs before the server accepts
  traffic (`readyz` gates until migrations are applied).
- **An older image cannot start against a newer schema.** On startup the
  migrator refuses to run when the database has applied migrations the binary
  does not contain (`sqlx` `VersionMissing`), and the process exits. So once a
  release that adds a migration has started even once, redeploying the
  previous image fails at boot.
- **Rollback = roll forward, or restore.** For a release that added
  migrations, either deploy a fix built on top of it (it carries every applied
  migration), or restore the pre-release database backup and then run the
  previous image — see `docs/backups.md`. Restoring discards every write made
  since the backup. Only a release that added **no** migration can be undone
  by simply redeploying the previous image tag.

Before deploying a release that adds migrations, take a fresh backup and record
the last migration version the previous image knows about.

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

Legacy deployments without `LOCATION_PROVIDER` may still set `MAP_STYLE_URL`.
A `mapbox://` or `api.mapbox.com` style selects Mapbox GL JS and requires
`MAPBOX_MAP_ACCESS_TOKEN` (with `MAPBOX_ACCESS_TOKEN` accepted only as the
legacy fallback). Other style URLs select MapLibre and never expose either
token to that SDK.

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
(RustFS in dev, AWS/S3/R2/B2 in prod; `S3_*` env) and served via **direct S3
presigned GET URLs** — the browser hits the bucket and S3's SigV4 signature
authorizes the read (no app-side proxy, no app signing secret). Selectable by
`S3_ENDPOINT`/`S3_BUCKET`; the compose RustFS is the DEVELOPMENT default only —
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

**Failure mode.** Every ValKey check has a 500 ms total deadline and emits only
an allowlisted reason/policy warning on degradation. Credential-sensitive auth
checks fail closed (the application maps the error to its existing limited
response). With `RATE_LIMIT_FAIL_OPEN=true`, other limited traffic is allowed
so a store outage does not take down unrelated contributions/photos. Set it to
false to fail closed for every limited endpoint.

**Docker compose:** the dev stack runs a single-node ValKey
(`docker-compose.yml`, `valkey` service, wired as `VALKEY_URL`). For cluster
mode use `docker-compose.valkey-cluster.yml` (a cluster-enabled node covering
all slots — portable on Docker Desktop for Mac; see the file header for a
multi-node variant).


## 5c. Background jobs

The app ships a **pure-PostgreSQL job queue** — no broker. A `background_job`
table stores durable one-shot + recurring work; an **in-process worker task**
(started when `JOBS_RUN_WORKER=true`, the default) claims due jobs with
`FOR UPDATE SKIP LOCKED`, runs their handler, and records the outcome. All job
times are UTC.

- **Recurring** jobs never go terminal: on success the worker recomputes
  `run_at` from `schedule` (`{"every_seconds":N}` or a UTC `{"cron":"…"}`) and
  resets the row to `pending`.
- **Retries** use exponential backoff + jitter; after `JOBS_MAX_ATTEMPTS` a
  *one-shot* job is dead-lettered to `failed` (kept with `last_error` for
  inspection). A *recurring* job never goes terminal on a handler failure: when
  an occurrence uses up its attempts (or fails permanently), `last_error` is
  recorded, an error is logged, attempts reset, and the row returns to
  `pending` at its next scheduled run.
- **`jobs.gc`** (itself a recurring job) deletes one-shot `succeeded`/`failed`
  rows older than `JOBS_HISTORY_RETENTION_DAYS` (default 7). Scheduled rows are
  never GC'd. The
  built-ins currently have `{}` payloads; any future sensitive recurring payload
  needs a separate minimization and retention review before registration.
- **At-least-once**: a worker crash leaves the job leasable; it is re-claimed
  after the lease, but only while `attempts < max_attempts`. A job that crashed
  or hung on its final attempt is finalized at the next claim instead (one-shot
  to `failed`, recurring to its next run, both with a `last_error` saying the
  lease expired after the final attempt), so a crash loop cannot re-run a job
  such as an email forever. Handlers must be idempotent.
- On a multi-instance deploy claims are safe because `SKIP LOCKED` assigns
  disjoint rows. A web-only instance sets `JOBS_RUN_WORKER=false` while keeping
  `JOBS_DURABLE_ENQUEUE=true`, and a separate `bikesnest-web worker` process
  must share its database.

The worker authoritatively registers exactly two built-ins at startup:

| Kind | Stable key | Schedule |
|---|---|---|
| `retention` | `recurring:retention` | every 86,400 seconds |
| `jobs.gc` | `recurring:jobs.gc` | every 86,400 seconds |

Registration validates and persists the schedule. On restart it preserves a
healthy pending row's future `run_at` and any active running lease. It repairs
an exact kind/key/payload legacy row whose schedule is NULL. A live legacy row
is revisited after its current lease/attempt finishes, so its active owner is
never overwritten. Any key collision with a different kind, payload or non-NULL
schedule is logged and left untouched. Startup continues processing independent
queue work while retrying reconciliation on later polls.

On boot, a scheduled row left `failed` by an older release is revived: it
returns to `pending` with a fresh attempt budget at its next scheduled run,
keeping `last_error` and `finished_at` as evidence. A legacy failed row with a
NULL schedule is reactivated to run now. A failing retention step no longer
skips the later steps; the run records `result=failure` in its audit event and
is retried. The legacy `cargo run -- retention` subcommand remains a manual
escape hatch.

Before a rollout, use a read-only preflight scoped to the two exact stable keys;
inspect `kind`, `payload`, `state`, `schedule`, `run_at`, `attempts`,
`claimed_by`, `lease_expires_at`, `finished_at` and `last_error`. Stop if either
key belongs to unexpected data. After the separately approved deployment,
verify that each key has exactly one row, the schedule is present, running
ownership was not changed, and a successful execution returns the same row to
`pending` with a future `run_at`. Do not delete job history as a repair.

The previous binary cannot start once this release's migrations are applied
(see section 4), so recovery here means rolling forward. A binary built from this
release can finish and reschedule a row whose schedule is already persisted, but
cannot recreate or safely repair a missing/legacy row. Keep the
persisted schedules intact, use the documented manual retention command only
with explicit operational approval, and roll forward promptly. A scheduled
`failed` state now only occurs for an invalid schedule.

### Job health and alerts

HTTP readiness alone does not establish that background work is healthy:
`/readyz` stays green while a worker is down or a recurring job fails every
run. Admins can open **`/admin/jobs`** (linked from the moderation dashboard),
which shows each recurring job's last success (`finished_at`), next run
(`run_at`), lateness, attempts and `last_error`, plus one-off queue pressure,
and refreshes itself every minute. A recurring row is flagged:

| Status | Meaning |
|---|---|
| Late | `pending` and more than 15 minutes past its `run_at`: nothing is claiming it |
| Stuck | `running` with an expired lease: the claiming worker is gone |
| Failing | on schedule, but the last occurrence failed (`last_error` is set) |
| Stopped | a recurring row in a terminal state (only an invalid schedule does this) |

Alert on the same facts from SQL (read-only; run it from your monitoring
system's Postgres check, every few minutes). Each query returns rows only when
something needs attention:

```sql
-- Recurring jobs that are late, stuck, stopped, or whose last success is
-- older than two schedule intervals (both built-ins run every 86,400 s).
SELECT kind, state, run_at, finished_at AS last_success, attempts, max_attempts,
       last_error, lease_expires_at
FROM background_job
WHERE schedule IS NOT NULL
  AND (
       (state = 'pending' AND run_at < now() - interval '15 minutes')
    OR (state = 'running' AND lease_expires_at < now())
    OR state IN ('failed', 'succeeded')
    OR (finished_at IS NULL AND created_at < now() - interval '2 days')
    OR finished_at < now() - 2 * make_interval(secs => (schedule->>'every_seconds')::double precision)
  );

-- Recurring jobs whose latest occurrence failed (warning, not page-worthy on
-- its own: the next occurrence may succeed).
SELECT kind, run_at, finished_at AS last_success, last_error
FROM background_job
WHERE schedule IS NOT NULL AND last_error IS NOT NULL;

-- One-off queue pressure: jobs waiting more than 15 minutes past due, and
-- jobs dead-lettered in the last 24 hours (for example undeliverable mail).
SELECT
  (SELECT count(*) FROM background_job
    WHERE state = 'pending' AND schedule IS NULL
      AND run_at < now() - interval '15 minutes') AS overdue_pending,
  (SELECT count(*) FROM background_job
    WHERE state = 'failed' AND schedule IS NULL
      AND finished_at > now() - interval '24 hours') AS failed_last_day;
```

The `every_seconds` comparison applies to interval schedules; a `cron` row
(none ship today) yields `NULL` there and is covered by the lateness clause.
Also alert on the worker's recurring-bootstrap error log lines.

## 5d. Transactional email goes through the queue

Registration, verification resend, password-reset request and e-mail-change
request each commit the account/token transition, audit where applicable, and
account-linked `email.send` row in one database transaction. Admission failure
rolls the complete transition back. A registration retry for a pending account
reuses valid queued work (including an active lease), or creates a fresh token
and job when the former credential/outbox is absent or expired; it never
overwrites the existing password, display name or locale. Once admitted, the
worker uses the configured retry budget
(`JOBS_MAX_ATTEMPTS`), exponential backoff and dead-lettering, keeping provider
latency off the request path.

- **Language.** The message carries the recipient's locale (`users.locale`, set
  at registration from the page's language and updated by the header language
  toggle for signed-in users). Subject and body are rendered from the message
  catalog *at send time* — pt-BR and en, never a hard-coded English string.
- **Alternatives and security notices.** SMTP sends `multipart/alternative`;
  Resend receives explicit `text` and `html` bodies. Newly admitted credential
  messages carry their exact stored expiry; legacy payloads without it make no
  invented duration claim. Successful password replacement durably admits a
  credential-free warning to the current canonical address, while confirmed
  address change warns the old address. Their transition audit reference and
  recipient digest are send-boundary evidence, not durable history.
- **Admission deduplication, at-least-once delivery.** Each job is enqueued under
  `email:{kind}:{sha256(identity)}`, where identity is the credential link or
  immutable security-notice id, so repeated admission for the same transition
  collapses onto the existing row. A genuine re-send issues a new token and a
  new job. This does not guarantee one provider delivery: lease expiry or an
  ambiguous provider response can result in a duplicate send.
- **Provider replay semantics.** Resend receives the same bounded outbox key in
  `Idempotency-Key`. Resend documents a
  [24-hour retention window](https://resend.com/docs/dashboard/emails/idempotency-keys),
  so this lowers duplicate risk only within that provider window. Its
  [error reference](https://www.resend.com/docs/api-reference/errors) defines
  `invalid_idempotent_request` as permanent while
  `concurrent_idempotent_requests` is retried later. The
  message is rendered at send time, so catalog/from changes during retries can
  conflict with the original provider payload. SMTP has no portable
  idempotency key and remains explicitly at-least-once.
- **Dead letters.** An exhausted job logs only the allowlisted message kind and
  stores a bounded error classification. Its recipient/link payload is cleared
  immediately; recipient digests and audit references are cleared with it.
  Non-personal lifecycle metadata remains until `jobs.gc` removes the row.
  Alert on that log line: delivery exhausted its retry budget. A provider may
  already have accepted an attempt whose outcome was ambiguous.
- **Deletion boundary.** Delivery and anonymization serialize on the account
  row. Deletion-first cancels even preclaimed mail; provider-acceptance-first is
  already outside the application's recall boundary. Provider timeout or a
  lost database connection remains an ambiguous at-least-once outcome.
- **Explicit inline mode.** `JOBS_DURABLE_ENQUEUE=false` makes the auth request
  exact-claim its committed outbox row. Legacy `JOBS_ENABLED=false` selects
  this mode as well as disabling the local worker when neither new knob is set.
  Success is terminal/redacted; transient failure persists backoff and returns an
  unavailable response, and a request retry cannot bypass that backoff.
  Permanent rejection dead-letters immediately. The trade-off is that provider
  latency is on the request. Prefer leaving the worker on; if you run
  web-only instances, make sure a dedicated `bikesnest-web worker` deployment
  shares the queue. Startup validation needs no new rule
  here: inline delivery uses the same account/token validation, lease ownership
  and lock-through-provider boundary as the worker.
  A credential-free security transition is already complete once its notice
  row commits: a failed post-commit inline attempt leaves its durable queue
  outcome (retryable or terminal) recorded and does not falsely report the
  password/address mutation as failed or invite a replay with the spent token
  or old credential.

Migration 0026 adds nullable mail lifecycle columns. During upgrade it redacts
all legacy `email.send` payloads because those rows cannot be safely linked to
an account/token. Pending/running legacy rows are cancelled as failed; existing
succeeded/failed history keeps its terminal state. Unrelated jobs are untouched.
The migration is forward-only. Rolling back application code after it runs is
not supported for mail delivery: old code cannot interpret redacted legacy rows
or maintain the new lifecycle contract. This migration overrides the generic
rolling sequence below: first stop and drain every old worker and old inline
mail-producing web instance, then start only the new version and let it migrate.
Otherwise an old worker could retain a raw preclaimed snapshot after the
database row is scrubbed. On failure, pause mail and forward-fix. Restoring a
backup can resurrect erased data and lose intervening writes; it is only a
separately approved disaster-recovery action and requires erasure
reconciliation. Backup expiry, not this migration, removes historical copies.

Migration 0027 extends that lifecycle with security-notice purposes, a
recipient digest, and a nullable audit-event foreign key (`ON DELETE SET
NULL`). Existing token jobs remain unchanged and readable; deleting retained
audit history does not block retention, but makes an unsent notice fail closed.
Stop and drain old workers before enabling writers that emit the new payload
variants: an old worker cannot decode them and may dead-letter them. Apply 0027,
upgrade every worker, then enable the new writers. Terminal outcomes and account
deletion clear both new metadata fields. Rollback to an old worker while new
notice rows exist is unsupported; pause mail and forward-fix instead.

## 6. Rolling deploy + rollback

1. Build the new image (tagged with the commit SHA).
2. Push to the registry; deploy the new image to one instance.
3. Wait for `readyz` to go green on that instance (migrations applied).
4. Drain the old instance; promote the new one.
5. On failure: stop the rollout. If the release added **no** migration,
   redeploy the **previous** image tag. If it added migrations, the previous
   image will refuse to start (section 4): roll forward with a fix, or restore
   the pre-release backup and then run the previous image (see
   `docs/backups.md`).

## 6a. Legal pages (privacy / terms / cookies)

The versioned legal pages are stored in `policy_version` and seeded as one
coherent six-document release from
`policies/{privacy,terms,cookies}.{pt-BR,en}.md`:

Every release, material or otherwise, requires the owner/counsel publication
approval recorded by the runbook before these commands are run.

1. Set `POLICY_OPERATOR_NAME`, `POLICY_OPERATOR_CNPJ`, `POLICY_OPERATOR_ADDRESS`
   and `POLICY_CONTACT_EMAIL` (the privacy inbox must be monitored — rights
   requests and takedown notices arrive there).
2. Set `POLICY_VERSION` (e.g. `2026-09-05.1`) and `POLICY_EFFECTIVE_AT`.
3. Follow the preflight, fleet-coherence, activation, and rollback procedure in
   [`policy-publication.md`](policy-publication.md). The seeder rejects partial
   or ambiguous releases and exact replay is idempotent; published rows remain
   immutable and reachable from version history.
4. The acknowledgement feature is off by default. Its in-product presentation
   evidence is not proof of delivery or reading, and it does not send the
   advance e-mail described by the policy draft. Do not enable or publish a
   material release until the additional activation gates in the runbook are
   complete.

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
