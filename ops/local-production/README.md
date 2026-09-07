# Local production deployment

The app runs as a release binary behind Caddy at `127.0.0.1:8081`.
PostgreSQL/PostGIS and Valkey run in a separate Docker Compose project.

From the repository root:

```sh
docker compose --env-file .env -f ops/local-production/compose.yml up -d --wait
cargo build --release --locked -p bikesnest-web
```

Configuration and credentials live in the ignored, mode-0600 `.env` file.
Never run `docker compose config` in shared logs: its output includes passwords.
The app intentionally refuses production startup until real provider settings
are supplied. Google OAuth must remain disabled until a real adapter exists.

The supplied user service is specific to this machine. After installing it into
`~/.config/systemd/user/bikesnest.service`, use:

```sh
systemctl --user daemon-reload
systemctl --user enable --now bikesnest.service
systemctl --user status bikesnest.service
journalctl --user -u bikesnest.service -n 100
curl --fail http://127.0.0.1:8081/healthz
curl --fail http://127.0.0.1:8081/readyz
```

The user's systemd manager and rootless Docker must start at boot (user lingering).
Restart the service after rebuilding the binary. Containers automatically restart
with Docker. Database port is `127.0.0.1:55433`; authenticated Valkey is
`127.0.0.1:56380`. Named volumes survive container recreation.

Once provider and policy settings are complete, use the independent seed
commands, which read `.env` from the working directory:

```sh
target/release/bikesnest-web seed-admin
target/release/bikesnest-web seed-policies
target/release/bikesnest-web seed-mock
```

Mock parking is explicitly requested temporary test content. Do not use
`seed-full-fresh` to clean it up: that command erases other application data.
Remove admin bootstrap credentials from `.env` after seeding.

## Installed configuration (2026-09-07)

- Public site: https://bikesnest.com; admin users: https://bikesnest.com/admin/users.
- Admin bootstrap credentials are in `.local-production/admin-credentials`
  (mode 0600), outside the app environment.
- App database role: `bikesnest_app`, a database owner without cluster superuser,
  role creation, or database creation privileges. `bikesnest_admin` is reserved
  for administration and backups. The original bootstrap role cannot log in.
- Google project: `bikesnest` (`934690449432`). Media bucket:
  `bikesnest-media-934690449432`, region `southamerica-east1`. Public access is
  blocked; the dedicated `bikesnest-storage` service account has object access
  only on that bucket. S3 compatibility uses HMAC and signed URLs at
  `https://storage.googleapis.com`. Deleted media can be recovered for 30 days.
- Maps browser key: `bikesnest-maps-browser`, restricted to Maps JavaScript API
  and `https://bikesnest.com` / `https://bikesnest.com/*`. Server key:
  `bikesnest-maps-server`, restricted to Places and Geocoding APIs and current
  public IPv4/IPv6. Map ID: `dae732539a342f6791386685`.
- Active daily quotas: 960 map loads, 960 geocoding calls, 960 autocomplete
  calls, 96 place-detail calls. Unused quota metrics are set to zero. The owner
  explicitly increased these beyond the original $10/month target. There is
  no enforced monthly spending cap. A conservative **R$10** monthly project
  budget sends early alerts at 50%, 80%, and 100%; the billing account uses BRL.
- Resend sender: `BikesNest <no-reply@email.bikesnest.com>`. Email delivery testing
  was explicitly declined. Google OAuth remains disabled because the app has
  no production implementation.
- Policies: version `2026-09-07.1`, both languages, using the supplied Clemento
  Dev identity. Forty-four temporary Curitiba parking locations were seeded.
- Caddy trusts client headers only from Cloudflare IP ranges and overwrites
  BikesNest's `X-Forwarded-For` with its resolved client address. The app trusts
  exactly one hop and binds only to loopback. The Caddyfile backup is
  `/home/bruno/Projects/caddy/Caddyfile.before-bikesnest-production`.
- Cloudflare injects an analytics beacon permitted by the app's CSP. Disable
  that beacon in Cloudflare if analytics are not intended.

## Automatic operations

`bikesnest-maps-ip.timer` checks the public IPs every five minutes. An IP change
can briefly interrupt server geocoding until the key update propagates. It uses
the machine user's existing gcloud login; reauthenticate if that login is revoked
or expires. API restrictions are preserved during updates.

`bikesnest-backup.timer` runs daily at 03:15 local time with up to 15 minutes of
jitter and catches up after downtime. Dumps go to private São Paulo bucket
`bikesnest-backups-934690449432` and expire after 30 days. The app storage service
account has no access to this bucket. Backup uploads use the machine user's
gcloud credentials. This provides daily recovery points, not continuous PITR.
Keep this directory, secret files, and required administrative credentials in
your own recovery records; database dumps do not contain cluster roles.

```sh
systemctl --user list-timers 'bikesnest-*'
systemctl --user start bikesnest-backup.service
journalctl --user -u bikesnest-backup.service -u bikesnest-maps-ip.service
```

A cloud dump was downloaded and restored into an isolated database on
2026-09-07: 44 parking locations and six policy versions verified. The check
database was removed afterward. For disaster recovery, initialize the roles
with `init-db.sh`, restore with `bikesnest_admin`, and follow `docs/backups.md`
before reopening traffic. Backups share the project's region and account;
they protect against machine loss but are not cross-region disaster recovery.
