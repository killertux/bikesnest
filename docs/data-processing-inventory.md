# Data-processing inventory + legal basis

> **Living document.** The legal bases below are product/engineering proposals,
> not legal conclusions. Counsel must confirm that each basis and each law is
> applicable. Keep this inventory and `policies/privacy.*.md` in sync before
> publishing a new policy version.
>
> Controller: the Brazilian company named by `POLICY_OPERATOR_*`. Deployed
> providers, regions, roles and transfer mechanisms still require owner
> evidence; see `docs/provider-transfer-inventory.md`.

| Data element | Purpose | LGPD art. 7 | GDPR art. 6 | Req/Opt | Stored | Access | Retention | Recipients / transfer |
|---|---|---|---|---|---|---|---|---|
| email | account identity, verification, reset, contact | V contract | (1)(b) contract | required | `users.email`, `authentication_identities.provider_subject` | self; admin (investigation) | until account de-identification | email provider (transactional delivery) |
| password hash | password auth | V contract | (1)(b) contract | required | `authentication_identities.credential_hash` | never readable | deleted on account de-identification | — (never transferred) |
| OAuth provider id (`google.sub`) | OAuth auth — code/config disabled by default; deployed state unverified | V contract (proposed) | (1)(b) contract (proposed) | optional | `authentication_identities.provider_subject` | self | deleted on account de-identification | Google records if enabled; complete inventory before enabling |
| display_name | optional profile field the user fills in | V contract | (1)(b) contract | optional | `users.display_name` | self; publicly attributed only under the separate setting below | nulled on account de-identification | — |
| public-name setting | optional public attribution on new reviews/proposals; enabled by default for new accounts and changeable by the user | legal basis pending counsel review; do not assume consent | legal basis pending applicability/counsel review | optional; not needed to use the service | `users.public_contribution_name`, latest-change timestamp, per-contribution `public_author` flags | self; public name only while both current setting and original contribution flag allow it | disabling permanently clears old flags; account deletion clears setting and attribution | public audience according to the user's setting |
| session info (hash, timestamps) | session/CSRF | IX legitimate interest (security) | (1)(f) | required | `sessions` | never readable | 30d idle / 90d cap; purged | — |
| last activity time | inactive-account retention (advanced on sign-in and throttled session refresh; survives session purge) | IX legitimate interest (retention enforcement) | (1)(f) | automatic | `users.last_active_at` | internal | kept with the account row; no longer attributable after de-identification; removed when the row is purged | — |
| IP address / user-agent (in-request) | rate-limit keys, abuse prevention | IX legitimate interest (security) | (1)(f) | transient | limiter keys (ValKey, TTL-bound); not persisted in the application DB; proxy/provider logs are separate | internal; infrastructure providers | request/window scoped in limiter; external log retention separately configured | hosting/edge provider |
| **access logs (date/time + IP)** | proposed statutory *registros de acesso* purpose | II legal obligation proposed — Marco Civil art. 15 applicability pending counsel | (1)(f) proposed if GDPR applies | automatic if configured | reverse-proxy / LB access log (not the app DB) | ops/provider as configured | proposed 6 months; actual enforcement unverified | hosting/edge provider |
| reviews | community content; may publish immediately | V contract (publishing is the service) | (1)(b) | optional | `review`/`review_revision` | public (name shown only under the attribution rules) | retained; account link/attribution removed on deletion, body unchanged | — |
| contributions (locations, proposals, revisions) | dataset; new listings may publish immediately, existing-fact proposals need six eligible approvals or moderator action | V contract | (1)(b) | optional | `parking_location`/`parking_proposal`/`parking_revision` | public after the applicable workflow (name shown only under attribution rules) | retained; account link/attribution removed | — |
| verification activity | confidence signals | V contract | (1)(b) | optional | `verification` | aggregated | retained with account link removed | — |
| parked-here events | personal "I was here" | V contract | (1)(b) | optional | `verification(kind=parked_here)` | never public | 90 days; deleted on account deletion | — |
| favorites | private bookmarks | V contract | (1)(b) | optional | `favorite` | self only | deleted on account deletion | — |
| reports | moderation input (reporter + reported content) | IX legitimate interest (safety of the service) | (1)(f) | optional | `report` | moderators only | retained; reporter account link removed, content unchanged | — |
| photos + metadata | community content; EXIF stripped, original never published | V contract | (1)(b) | optional | `parking_photo`/`review_photo` + object storage | public only after moderator approval; uploader name is not displayed, though image content can identify someone | retained with account attribution removed; rejected/orphans targeted for purge in 24h | object-storage provider (opaque keys) |
| browser geolocation / map pin | search origin | V contract (running the search) | (1)(b) | optional; browser permission prompt or user selection | not intentionally persisted as an account record; coordinates can be in search URL/history and request/access/provider logs | browser, app request path and infrastructure providers | browser/log/provider retention applies | hosting/edge logs; selected geocoder/map provider |
| search UI preference | remember whether the results map is open | V contract (proposed) | (1)(b) (proposed) | optional | browser `localStorage` key `bn.search.mapOpen` | that browser profile | no app-set expiry; user/browser clearing or eviction | — |
| edge/browser telemetry | page-view/performance telemetry observed from the configured edge on 2026-09-08 | basis pending deployed-payload review | basis/applicability pending review | automatic if enabled at edge | provider-controlled; exact fields, cookies and retention unverified | operator/provider as configured | unverified | Cloudflare/selected edge provider; legal role unverified |
| audit events | security, accountability, moderation traceability | IX legitimate interest proposed; II only where counsel confirms a duty | (1)(f) proposed if GDPR applies | required | `audit_events` | admin only | proposed 5 years; purge not automated | — |
| privacy requests | rights workflow record | II legal obligation proposed (LGPD arts. 18/19) | (1)(c) proposed only if GDPR applies and the obligation qualifies | optional | `privacy_request` | admin only | proposed 5 years; `user_id` nulled on deletion; purge not automated | — |
| public-attribution setting evidence | current setting and its latest change | basis and notice review required before release | basis and applicability review required before release | optional | account setting/timestamp and per-contribution attribution flags | self/admin | account lifetime; cleared on de-identification | — |

### Why these bases

- **Contract (V / 6(1)(b))** for everything the user asks the service to do:
  holding an account, publishing their contributions, keeping their favorites.
  Consent is not proposed as a catch-all because withdrawal would conflict with
  the requested core service; counsel must confirm necessity and scope.
- **Legitimate interest (IX / 6(1)(f))** for security, rate limiting,
  moderation and audit. The preliminary balance and its caveats are recorded in
  `docs/legal-review.md`; counsel must confirm it.
- **Legal obligation (II / 6(1)(c))** only where a statute actually requires the
  record: Marco Civil access logs, the rights-request log, consent evidence.
  Note that for GDPR a *Brazilian* statute is not a 6(1)(c) basis, so the access
  logs fall under 6(1)(f) for EEA users.

### Notes

- No marketing or advertising integration is implemented. That fact does not,
  by itself, decide whether an edge telemetry feature needs consent or another
  notice; inspect the deployed payload/cookies/configuration first.
- Provider-boundary tests cover specific application payloads; they do not
  establish external provider configuration. See
  `docs/provider-transfer-inventory.md`.
- **Minimum account age 18** is a product rule, not evidence that no minor can
  access the public service. Counsel must assess Law 15.211/2025 probable-access
  duties, Decree 12.880/2026 and current ANPD guidance; there is no default
  identity-document collection.

- Public attribution is a separately optional purpose but is enabled by default
  for new accounts, so this inventory does not characterize it as affirmative
  consent. Its basis/default/notice need legal and product review before release.
  Editing these source files does not update already-seeded policy versions.
