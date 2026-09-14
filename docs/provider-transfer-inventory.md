# Provider & international-transfer inventory

> **Evidence gate:** the production hosting, regions, provider legal roles,
> contracts and onward-transfer paths have not been supplied. Do not describe
> every provider as a processor, every deployment as foreign-hosted, or a DPA
> or transfer clause as accepted until the operator records the evidence here.
>
> For each actual transfer, counsel must select the applicable LGPD art. 33 and,
> if GDPR applies, Chapter V mechanism after reviewing the real contract,
> destination, onward transfers and the parties' roles. ANPD standard clauses,
> adequacy, DPF and EU SCC are possibilities, not established facts here.
>
> **Status column** is an evidence checklist, not an assurance that a DPA is
> available, sufficient, signed or accepted.

| Provider (chosen) | Purpose | Data transferred | Region | Role | GDPR mechanism | LGPD mechanism | Status |
|---|---|---|---|---|---|---|---|
| Hosting (app + PostgreSQL) — _name TBD_ | run app, store DB | full application and database data | ☐ record actual locations and backups | ☐ determine | ☐ assess if applicable | ☐ assess | ☐ provider/terms/DPA/region/backups evidenced |
| Object storage (S3-compatible: AWS S3 / Cloudflare R2 / Backblaze B2) — _TBD_ | photo binaries | derivative bytes under opaque keys; image content may contain personal data | ☐ record actual locations | ☐ determine | ☐ assess if applicable | ☐ assess | ☐ provider/terms/DPA/region evidenced ☐ bucket private, presigned GET only |
| Email (Resend **or** SMTP relay) — _TBD_ | transactional account/security mail | e-mail address plus rendered text/HTML message and provider request metadata | ☐ record actual locations | ☐ determine | ☐ assess if applicable | ☐ assess | ☐ provider/terms/DPA/locations/retention evidenced |
| **Mapbox** (`LOCATION_PROVIDER=mapbox`) | server-side address search/autocomplete and browser map SDK/style/tiles | server requests carry query/coordinates and request metadata; browser map requests carry IP and viewed area; no BikesNest account identity or cookie intentionally added | ☐ record contracted processing locations | ☐ determine from contract and each flow | ☐ assess if applicable | ☐ assess | ☐ terms/DPA/role/locations reviewed ☐ server/browser tokens restricted |
| **Google Maps Platform** (`LOCATION_PROVIDER=google`) | address search, autocomplete, place resolution, and map rendering | typed query/coordinates + random session token from server; browser IP + viewed area for map assets; no BikesNest account identity or cookie intentionally added | ☐ record contracted processing locations | ☐ determine from contract and each flow | ☐ assess if applicable | ☐ assess | ☐ terms/DPA/role/locations reviewed ☐ keys API/referer/network restricted ☐ enabled APIs recorded |
| OpenFreeMap tiles | configured tile path for the code's `fake` location-provider profile | requests **from the user's browser**: client IP + viewed area; no account identity intentionally added | ☐ | ☐ determine from terms | ☐ | ☐ | code profile exists; deployed selection unverified |
| Automated content screening (LLM/classifier) — _future_ | not part of the current product | unknown until designed | ☐ | ☐ | ☐ | ☐ | not wired; complete privacy/design review before adding |
| Cloudflare edge/browser telemetry — deployment evidence pending | proxying and observed page-view/performance telemetry | may include IP, URL, browser/request metadata; exact beacon payload and cookies unverified | ☐ record deployed region/terms | ☐ determine | ☐ | ☐ | observed 2026-09-08; operator must revalidate deployed config, payload, cookies, retention and consent basis before publication |
| Observability / error tracking — _none wired in app_ | logs/metrics | application logs are minimized; hosting/edge log fields remain configuration-dependent | ☐ | ☐ determine if added | ☐ | ☐ | inventory deployed hosting/log stack and retention |
| Google (OAuth) | login | `sub`, e-mail, `email_verified` | ☐ | ☐ | ☐ | ☐ | code/config disables this feature by default; deployed configuration unverified; complete inventory before enabling |

## Data-minimization confirmations

- **Map renderer** receives no authenticated identity — tiles are public.
- **Geocoder** receives the typed query and, for Google autocomplete, a random
  per-selection session token. It receives no BikesNest account identity,
  cookie, or direct browser connection.
- **Object store** receives only derivative bytes under opaque keys (no email or
  provider `sub` in keys).
- **Email provider** receives the address plus the rendered transactional
  account/security message. Credential links are scrubbed from the application
  queue on terminalization/deletion, but a provider-accepted message cannot be
  recalled.

Tests assert that the export payload never contains a credential or token hash;
the map, geocoder, and object-key calls carry no account identity.

## Selectable location provider

`LOCATION_PROVIDER` selects one coherent profile. It defaults to `fake` in
development, and production accepts only `mapbox` or `google`.

- **fake** — deterministic dev geocoder (no external request).
- **mapbox** — `MapboxGeocoder` calls the Mapbox forward-geocoding endpoint.
  The response's provider-ranked features supply up to ten dropdown options.
  Their coordinates are ready as soon as the user selects one. Mapbox GL JS
  renders the configured Mapbox style.
- **google** — `GoogleGeocoder` calls Geocoding API for direct searches, Places
  Autocomplete (New) for dropdown predictions, and Place Details for the chosen
  prediction. One random session token ties autocomplete to selection. The
  browser loads Maps JavaScript API and renders Google results on Google Maps.

Hosted geocoding responses are not stored in BikesNest's in-process cache. The
per-IP rate limit bounds provider traffic. Errors render the localized location
service unavailable state rather than an internal-server-error page.

## Pre-launch procedure (ops)

1. Record the actually selected hosting, storage, email, map/tile and edge
   providers and the exact production product/configuration.
2. Capture provider terms, legal role, processing/storage regions, subprocessors,
   retention, payload fields and contract/DPA evidence. Revalidate the observed
   Cloudflare telemetry against the deployed edge account.
3. Ask counsel to assess LGPD/GDPR applicability and choose any required
   transfer mechanism. Do not treat an EU region as resolving all GDPR scope,
   onward-transfer or controller/processor questions.
4. Update the public policy from the verified inventory, then complete the
   controlled policy-version/notification workflow.
5. For MapLibre, set `CSP_TILE_HOSTS` to the chosen style/tile hosts. Google
   Maps origins are enabled automatically by the selected profile.
