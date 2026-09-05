# Provider & international-transfer inventory

> **Decision 2026-09-03:** production hosting and **every** processor sit
> **outside Brazil** (EU and/or US). Every row is therefore an international
> transfer under LGPD art. 33 for Brazilian users, and — for EEA users — a
> Chapter V transfer whenever the provider is outside the EEA. The privacy
> policy's international-transfer section says exactly this.
>
> **Mechanisms we rely on** (state them in each DPA):
> - **LGPD:** the ANPD standard contractual clauses (Resolução CD/ANPD nº
>   19/2024 — *Regulamento de Transferência Internacional de Dados*),
>   incorporated in the provider's DPA. If a provider does not offer them, the
>   fallback is art. 33 IX (transfer necessary to perform the contract with the
>   data subject) — weaker; prefer providers that sign the ANPD clauses.
> - **GDPR:** an adequacy decision (EU-hosted, or a US provider certified under
>   the EU-US Data Privacy Framework) or the EU standard contractual clauses in
>   the provider's DPA.
>
> **Status column** is the pre-launch checklist. Nothing below is done until the
> DPA is accepted in the provider account and the region is written down.

| Provider (chosen) | Purpose | Data transferred | Region | Role | GDPR mechanism | LGPD mechanism | Status |
|---|---|---|---|---|---|---|---|
| Hosting (app + PostgreSQL) — _name TBD_ | run app, store DB | full app + DB (all personal data) | ☐ EU / US — record it | processor | ☐ EU-hosted → none needed; US → DPF or EU SCC | ☐ ANPD SCC in DPA (else art. 33 IX) | ☐ DPA accepted ☐ region recorded ☐ backups same region |
| Object storage (S3-compatible: AWS S3 / Cloudflare R2 / Backblaze B2) — _TBD_ | photo binaries | derivative bytes under opaque keys, with no user metadata | ☐ | processor | ☐ | ☐ | ☐ DPA ☐ region ☐ bucket private, presigned GET only |
| Email (Resend **or** SMTP relay) — _TBD_ | verification / reset mail | email address + token link | ☐ (Resend: US) | processor | ☐ | ☐ | ☐ DPA ☐ region |
| **Mapbox** (`LOCATION_PROVIDER=mapbox`) | address search and autocomplete | query string; server-to-provider request metadata; no BikesNest identity or cookie | US | processor | ☐ Mapbox DPA (EU SCC / DPF) | ☐ ANPD SCC if offered, else art. 33 IX | ☐ DPA accepted ☐ server token API-scoped |
| **Google Maps Platform** (`LOCATION_PROVIDER=google`) | address search, autocomplete, place resolution, and map rendering | typed query + random session token from the server; browser IP + viewed area for map assets; no BikesNest account identity or cookie | ☐ record contracted region/terms | processor / independent controller as contract states | ☐ Google Maps Platform terms + DPA/SCC/DPF reviewed | ☐ ANPD SCC if offered, else art. 33 IX | ☐ terms/DPA accepted ☐ keys API/referer/network restricted ☐ Places/Geocoding/Maps JS enabled |
| OpenFreeMap tiles | render the MapLibre development basemap for the fake profile | requests **from the user's browser**: client IP + viewed area; no account identity | ☐ | processor (receives IP directly) | ☐ | ☐ | development only |
| Automated content screening (LLM/classifier) — _future_ | moderation assist | the photo/text being screened only; **never** account identity | ☐ | processor | ☐ | ☐ | not wired yet; add it here before launch |
| Observability / error tracking — _none planned_ | logs/metrics | logs (headers never logged; PII minimized) | ☐ | processor | ☐ | ☐ | if added: DPA + region |
| Google (OAuth) | login | `sub`, email, `email_verified` | US | independent controller (their side) | n/a | n/a | **deferred — not in production**; update the policy when shipped |

## Data-minimization confirmations

- **Map renderer** receives no authenticated identity — tiles are public.
- **Geocoder** receives the typed query and, for Google autocomplete, a random
  per-selection session token. It receives no BikesNest account identity,
  cookie, or direct browser connection.
- **Object store** receives only derivative bytes under opaque keys (no email or
  provider `sub` in keys).
- **Email provider** receives only the address + the verification/reset link.

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

1. Pick the hosting, storage, email and tile providers; fill the _TBD_ cells.
2. In each provider console, accept the DPA; download/print it and note whether
   it includes **EU SCC / DPF** and the **ANPD standard clauses**. File them.
3. Record each region. Prefer EU regions for hosting/DB/storage: it removes the
   GDPR transfer question for EEA users entirely.
4. If any provider lacks ANPD clauses, note "art. 33 IX" in this table and flag
   it in `docs/legal-review.md` for counsel.
5. For MapLibre, set `CSP_TILE_HOSTS` to the chosen style/tile hosts. Google
   Maps origins are enabled automatically by the selected profile.
