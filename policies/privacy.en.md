This Privacy Policy explains how BikesNest handles personal data under Brazil's General Data Protection Law (Law No. 13,709/2018, "LGPD"). The European Union or United Kingdom GDPR provisions described below apply only when the relevant law applies to the processing; a person's location alone does not decide that question.

BikesNest is a community map of bicycle parking. Anyone can search without an account. An account is only needed to contribute (adding locations, photos, reviews and verifications).

For new accounts, your display name is shown on new reviews and change proposals by default. You can turn this off in account settings before contributing, or at any time afterwards, to contribute anonymously. Existing accounts keep their previous visibility setting. If you have no display name, you remain anonymous; your e-mail address is never used as your public name. This applies only to new reviews and proposals, not earlier anonymous contributions, even if you edit them. Turning the setting off removes your name from existing contributions; turning it on again does not restore those names. You can use the service with name sharing turned off. We store the preference and the date of its latest change. Voting identities are never public; other visitors see vote totals only. Content you write or photograph may itself identify you or someone else, regardless of the author label.

## 1. Who the controller is

**{{OPERATOR_NAME}}**, Brazilian company registration (CNPJ) No. {{OPERATOR_CNPJ}}, with registered office at {{OPERATOR_ADDRESS}} ("we"), is the controller of the personal data processed on BikesNest.

Privacy contact channel and data protection officer (LGPD art. 41): **{{CONTACT_EMAIL}}**.

## 2. What data we process

| Category | Data | Source | Required? |
|---|---|---|---|
| Account | e-mail address; password (stored only as a cryptographic *hash*); display name (optional); account state; creation and e-mail verification dates | you | e-mail and password: yes |
| Contributions | parking locations, change proposals, revisions, reviews, verifications and photos you submit, with timestamps | you | no |
| Photos | the image, re-encoded into resized versions; **EXIF metadata (including GPS location) is removed** before storage and the original file is never published | you | no |
| Private activity | favourites; "I parked here" records | you | no |
| Reports | reported content, reason and moderation outcome | you / moderators | no |
| Technical data | session identifier (*hash*), e-mail verification and password reset tokens, IP address and browser information used for rate limiting, security and access logs | your browser | automatic |
| Audit records | administrative, moderation and privacy actions, with actor and timestamp | system | automatic |
| Privacy requests | requests to exercise your rights and their status | you | no |
| Search and browser location | address/search text; precise or approximate coordinates supplied by your browser or a map pin; search URL parameters; the `bn.search.mapOpen` map-display preference stored locally in your browser | you / your browser | no |

We do not ask you to provide sensitive personal data. Free-text, photos and location contributions can nevertheless reveal sensitive or identifying information about you or someone else. Please do not include it unless it is necessary and lawful. We do not profile you or use your data for advertising.

Browser coordinates are used to run your search. They can appear in the search URL and browser history and travel in requests through our hosting or proxy; those systems' logs and the selected map or geocoding provider may therefore receive them. We do not intentionally add browser-location coordinates to your account record or the community dataset unless you separately submit a map location as a contribution.

## 3. Why we use the data and on what legal basis

| Purpose | Data | Legal basis (LGPD art. 7) | Legal basis (GDPR art. 6) |
|---|---|---|---|
| Create and maintain your account; authenticate; verify e-mail; reset password; send transactional e-mails | account, technical data | V – performance of a contract | 6(1)(b) – performance of a contract |
| Publish and maintain your contributions on the community map; favourites and "I parked here" | contributions, photos, private activity | V – performance of a contract | 6(1)(b) – performance of a contract |
| Keep the service secure: sessions, abuse and fraud prevention, rate limits, audit records | technical data, audit records | IX – legitimate interest | 6(1)(f) – legitimate interest |
| Keep application access logs for the statutory period | access timestamp and IP address | II – legal obligation (art. 15 of the Brazilian Internet Act, Law No. 12,965/2014) | 6(1)(f) – legitimate interest |
| Content moderation and handling of reports | contributions, photos, reports | IX – legitimate interest | 6(1)(f) – legitimate interest |
| Handle rights requests and keep a record of them | privacy requests, account | II – legal obligation | 6(1)(c) – legal obligation |
| Run a nearby search and remember whether the search map is open | search and browser location data | V – performance of a contract | 6(1)(b) – performance of a contract |

Legitimate interest is the proposed basis for the security, moderation and audit purposes described above. You may object to processing based on legitimate interest (section 9).

## 4. Who we share data with

We do not sell personal data or share it with advertising networks. Depending on the deployed configuration and the provider's terms, the following services may act as a processor or in another legally defined role:

- **Hosting and database** – run the application and store all the data described above.
- **File storage** – holds resized photo versions under opaque identifiers; the object key does not contain your e-mail address or account identifier.
- **E-mail delivery** – receives your e-mail address and the transactional account or security message.
- **Geocoding** – receives the address text you type and may receive a random autocomplete-session identifier. Requests come from the BikesNest server, so they include no account identity, cookie or direct browser IP address.
- **Maps** – when the map is displayed, your browser requests map code and data directly from the selected map provider, which receives your IP address and the map area shown. We send it no account data. When Google Maps Platform is selected, its use is also governed by the [Google Maps Platform Terms](https://cloud.google.com/maps-platform/terms) and [Google Privacy Policy](https://policies.google.com/privacy).
- **Edge and browser telemetry** – Cloudflare provides page-view and performance telemetry at the service edge and may receive request metadata such as IP address, URL and browser information. See the [Cookie Policy](/cookies) for the data and controls that apply to the deployed feature.

We may also disclose data where required by law, court order or a competent authority, or to protect our rights and the safety of the service and its users.

The current list of configured providers and their roles is available on request through the contact channel.

## 5. Content moderation

New parking listings and reviews can appear immediately. A proposed change to an existing parking fact becomes the published fact only after six eligible community approvals or a moderator decision; the pending proposal or a summary of it may be visible before then. Photos appear only after moderator approval. Human moderators can refuse, hide or remove content under the Terms of Service.

You can ask for a moderation decision to be reviewed through the contact channel.

## 6. International transfers

Provider locations and legal roles depend on the services selected for deployment. When personal data is transferred internationally, the applicable destination, recipient and transfer mechanism must be assessed for that provider under LGPD art. 33 and, where applicable, Chapter V of the GDPR. Contact us for the current deployed-provider information.

## 7. How long we keep data

| Data | Period |
|---|---|
| Account (e-mail, password *hash*, display name) | until you delete your account; the active account is then de-identified and its residual database shell is scheduled for deletion after 30 days |
| Sessions | 30 days idle, at most 90 days |
| E-mail verification / password reset token | 24 hours / 1 hour |
| Contributions, reviews, verifications, approved photos | remain on the map as part of the community dataset; account and public-name attribution are removed on account deletion, but retained free text or images are not thereby guaranteed to be anonymous |
| Photos rejected in moderation and incomplete uploads | removed within 24 hours |
| Favourites | until you remove them or delete your account |
| "I parked here" | 90 days, or until account deletion |
| Reports and moderation records | kept for the safety of the service; the reporter's account link is removed on account deletion, but report content may still identify someone |
| Access logs (timestamp and IP) | 6 months (art. 15 of the Brazilian Internet Act) |
| Audit records | 5 years |
| Privacy requests and their handling | 5 years, unlinked from the account after deletion |
| Data export file | 24 hours |
| Browser location | not intentionally stored as an account record; it may remain in browser history, request/access logs and provider records under their respective retention periods |

We do not delete accounts for inactivity without notice. If we start doing so, we will warn you by e-mail in advance and update this Policy.

Deletion also scrubs queued transactional-mail recipient and credential-link data in the application database. A message already accepted by an external delivery provider cannot be recalled. Scheduled expiry and deletion can be delayed during an outage or while retention jobs are disabled. Backup copies are not rewritten immediately; they expire under the applicable backup lifecycle, and restored backups must have deletions reconciled before normal use.

## 8. Security

We apply technical and organisational measures proportionate to the risk: encrypted connections (TLS), passwords stored with a strong hashing algorithm (Argon2), role-based access control with audit records, removal of photo metadata, rate limits and protection against abusive automation, backups, and an incident-response procedure. If a security incident is likely to cause you significant risk or harm, we will notify the ANPD and the affected people as required by law.

## 9. Your rights

At any time you may (LGPD art. 18; GDPR arts. 15–22):

- confirm that processing takes place and **access** your data;
- **rectify** incomplete, inaccurate or outdated data;
- request the **anonymisation, blocking or erasure** of unnecessary or excessive data;
- obtain **portability** of your data in a machine-readable format;
- **delete your account** – the account link and display-name attribution are removed from retained contributions; their text or images may still identify someone;
- request removal or restriction of identifying content itself through a separate rights request; deleting the account alone does not rewrite retained community content;
- be informed about who we share your data with;
- **object** to processing based on legitimate interest and request **restriction** of processing;
- request review of a moderation decision;
- withdraw consent where processing is based on it;
- lodge a complaint with the **ANPD** (gov.br/anpd) or, if you are in the European Union, with your national data protection authority.

**How to exercise them:** while signed in, go to *Account → Privacy & data* to export your data, delete your account or file other requests. You can also write to {{CONTACT_EMAIL}} from your registered e-mail address. To protect your data we may ask you to confirm your identity first. We respond within **15 days** (LGPD) or **one month** (GDPR); these periods may be extended where the law allows.

## 10. Minimum age

BikesNest is intended for people aged **18 or older**. We do not knowingly create accounts for anyone under 18; if we become aware of such an account, we will act under the applicable law. The age rule does not by itself exclude protections that apply to a service likely to be accessed by children or adolescents. BikesNest does not use identity-document age verification by default; please do not send identity documents unless we request them through a documented privacy process.

## 11. Cookies

We use first-party cookies for session, form protection and language. The browser also stores the `bn.search.mapOpen` search-map preference in local storage. Cloudflare page-view and performance telemetry at the edge is described in the [Cookie Policy](/cookies).

## 12. Changes

This Policy is versioned. The effective date and version number appear at the top of the page, and previous versions are available at [/privacy/versions](/privacy/versions). If we make material changes, we will notify you by e-mail or through a notice on the service before they take effect.

## 13. Contact

Questions, requests and complaints about privacy: **{{CONTACT_EMAIL}}**.
