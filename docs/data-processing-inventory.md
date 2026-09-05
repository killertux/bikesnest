# Data-processing inventory + legal basis

> **Living document.** Legal bases below were **decided by product on 2026-09-03**
> (see `docs/legal-review.md` for the decision record and what still needs
> outside counsel). Engineering did not invent them: each is the standard basis
> for that purpose under LGPD art. 7 / GDPR art. 6, and the privacy policy
> (`policies/privacy.*.md`, purposes table) states exactly the same table. **Keep the two in
> sync** — a change here is a policy change (bump `POLICY_VERSION`).
>
> Controller: the Brazilian company named by `POLICY_OPERATOR_*`. Hosting and
> all processors are outside Brazil (EU/US) — see
> `docs/provider-transfer-inventory.md`.

| Data element | Purpose | LGPD art. 7 | GDPR art. 6 | Req/Opt | Stored | Access | Retention | Recipients / transfer |
|---|---|---|---|---|---|---|---|---|
| email | account identity, verification, reset, contact | V contract | (1)(b) contract | required | `users.email`, `authentication_identities.provider_subject` | self; admin (investigation) | until anonymization | email provider (delivery only) |
| password hash | password auth | V contract | (1)(b) contract | required | `authentication_identities.credential_hash` | never readable | deleted on anonymization | — (never transferred) |
| OAuth provider id (`google.sub`) | OAuth auth — **not offered in production (Google OAuth deferred)** | V contract | (1)(b) contract | optional | `authentication_identities.provider_subject` | self | deleted on anonymization | Google (their own records) — add to the policy when shipped |
| display_name | optional profile field the user fills in | V contract | (1)(b) contract | optional | `users.display_name` | self; publicly attributed only with the separate optional preference below | nulled on anonymization | — |
| public-name preference | optional public attribution on new reviews/proposals; disabled by default | proposed I consent, pending legal review | proposed (1)(a) consent, pending legal review | optional; not needed to use the service | `users.public_contribution_name`, latest-change timestamp, per-contribution `public_author` flags | self; public name only while both current preference and original contribution flag allow it | disabling permanently clears old flags; account deletion clears preference and attribution | public audience when opted in |
| session info (hash, timestamps) | session/CSRF | IX legitimate interest (security) | (1)(f) | required | `sessions` | never readable | 30d idle / 90d cap; purged | — |
| IP address / user-agent (in-request) | rate-limit keys, abuse prevention | IX legitimate interest (security) | (1)(f) | transient | limiter keys (ValKey, TTL-bound); not persisted by the app | internal | request/window scoped | — |
| **access logs (date/time + IP)** | statutory *registros de acesso* | **II legal obligation — Marco Civil art. 15** | (1)(f) | automatic | reverse-proxy / LB access log (not the app DB) | ops only, confidential | **6 months**, then deleted | hosting provider |
| reviews | community content | V contract (publishing is the service) | (1)(b) | optional | `review`/`review_revision` | public (anonymous unless separately opted in at creation) | retained, anonymized on deletion | — |
| contributions (locations, proposals, revisions) | dataset | V contract | (1)(b) | optional | `parking_location`/`parking_proposal`/`parking_revision` | public (proposal authors anonymous unless separately opted in at creation) | retained, anonymized | — |
| verification activity | confidence signals | V contract | (1)(b) | optional | `verification` | aggregated | retained anonymized | — |
| parked-here events | personal "I was here" | V contract | (1)(b) | optional | `verification(kind=parked_here)` | never public | 90 days; deleted on account deletion | — |
| favorites | private bookmarks | V contract | (1)(b) | optional | `favorite` | self only | deleted on account deletion | — |
| reports | moderation input (reporter + reported content) | IX legitimate interest (safety of the service) | (1)(f) | optional | `report` | moderators only | retained; reporter anonymized | — |
| photos + metadata | community content; EXIF stripped, original never published | V contract | (1)(b) | optional | `parking_photo`/`review_photo` + object storage | public once approved (uploader never shown) | retained, uploader anonymized; rejected/orphans purged in 24h | object-storage provider (opaque keys) |
| **automated content screening** (planned: LLM/classifier over photos + texts) | detect ToS-violating content before human review | IX legitimate interest | (1)(f) | automatic | flags on the moderation queue; **no personal data sent besides the content itself** — send no account identity to the model provider | moderators | with the moderation record | model provider (processor) — add to `provider-transfer-inventory.md` when wired; disclosed in the policy moderation section |
| browser geolocation | search origin | V contract (running the search) | (1)(b) | optional; browser permission prompt | never persisted | client only | not retained | geocoder/map (coordinates only) |
| audit events | security, accountability, moderation traceability | IX legitimate interest; II where a legal duty applies | (1)(f) | required | `audit_events` | admin only | **5 years** | — |
| privacy requests | rights workflow record | II legal obligation (LGPD art. 18/19; GDPR art. 12) | (1)(c) | optional | `privacy_request` | admin only | **5 years**, `user_id` nulled on deletion | — |
| consent preference evidence | current public-name choice and its latest change | basis and notice review required before release | basis and notice review required before release | optional | account preference/timestamp and per-contribution attribution flags (not a separate consent ledger) | self/admin | account lifetime; cleared on anonymization | — |

### Why these bases

- **Contract (V / 6(1)(b))** for everything the user asks the service to do:
  holding an account, publishing their contributions, keeping their favorites.
  Consent was deliberately *not* used here — it would be a catch-all and
  its withdrawal would break the service.
- **Legitimate interest (IX / 6(1)(f))** for security, rate limiting,
  moderation and audit: necessary to keep a UGC service safe; low privacy
  impact; users can object (policy rights section). Recorded balancing note in
  `docs/legal-review.md`.
- **Legal obligation (II / 6(1)(c))** only where a statute actually requires the
  record: Marco Civil access logs, the rights-request log, consent evidence.
  Note that for GDPR a *Brazilian* statute is not a 6(1)(c) basis, so the access
  logs fall under 6(1)(f) for EEA users.

### Notes

- **No entry sends data to a third party beyond the recipients listed.** No
  marketing, no advertising, no cross-site tracking → no consent banner.
- **Data minimization** is asserted by tests; provider boundaries are in
  `docs/provider-transfer-inventory.md`.
- **Minimum age 18** (decision): no child/adolescent processing (LGPD art. 14,
  GDPR art. 8) — stated in the policy age section and on the sign-up form.

- Public attribution is a new, separately optional purpose, not covered by the
  earlier product decision. The proposed consent basis and source-policy changes
  need legal/product review and a new published policy version before release.
  Editing these source files does not update already-seeded policy versions.
