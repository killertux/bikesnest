# Legal review record — privacy, terms, cookies

> Hand this file plus `policies/*.md` to counsel. It records product proposals,
> implementation evidence and questions that still need legal review. The
> policy drafts were written by engineering; they are not legal advice and
> have not been approved for publication.

## 1. Product state and review status

| Topic | Current record | Where it shows |
|---|---|---|
| Controller | A Brazilian company (name, CNPJ, address supplied via `POLICY_OPERATOR_*` at seed time) | policy §1, terms intro/§13 |
| Contact / encarregado | Policy templates expose `POLICY_CONTACT_EMAIL`; owner must evidence that the inbox is monitored and counsel must confirm whether a named encarregado/DPO is required | policy §1, §9, §13 |
| Jurisdictions | LGPD is the primary draft; GDPR/UK GDPR applicability is unresolved and must be assessed from actual territorial scope, not visitor location alone | policy intro, §6, §9; terms §12 |
| Hosting / providers | Provider, region, contractual role and transfer path must be recorded from the deployed accounts | policy §4/§6; `docs/provider-transfer-inventory.md` |
| Transfer mechanism | No DPA or transfer mechanism is treated as accepted without owner evidence; counsel must select the applicable mechanism per deployed transfer | policy §6 |
| Minimum age | Accounts are restricted to **18+**, but that rule does not by itself avoid duties for services likely to be accessed by children or adolescents | policy §10, terms §2, sign-up form |
| Legal bases | Engineering's proposed mapping is in `docs/data-processing-inventory.md`; counsel must confirm applicability, including whether each legal-obligation claim is supportable | policy §3 |
| Retention | inactive accounts are not auto-deidentified; deleted shells are configured for purge after 30 days; several other periods and external schedules still require owner/legal evidence | policy §7; `docs/retention-policy.md` |
| Erasure of contributions | account deletion removes the account link and public attribution; retained free text/photos are not necessarily anonymous. A request about identifying content itself is a separate rights/content-removal assessment | policy §7/§9; `docs/retention-policy.md` |
| Cookies / local storage / telemetry | App cookies and `bn.search.mapOpen` are documented; observed Cloudflare telemetry must be revalidated against the deployed edge before counsel decides notice/consent requirements | cookies policy; processing/provider inventories |
| UGC responsibility | user warrants authorship, no faces/plates/private interiors, no obscene/illegal content; indemnity and licence need counsel review; attribution follows the account setting | terms §3 |
| Moderation | new listings/reviews may publish immediately; existing-fact proposals need six eligible approvals or a moderator; photos need moderator approval. No unimplemented automated classifier is represented as current | policy §5, terms §4 |
| Takedown channel | report button + contact e-mail; notice must carry URL + reason + contact | terms §4 |
| Liability | "as is"; explicit no-guarantee of bike safety/theft; limits "to the extent permitted by law" (CDC preserved) | terms §7–§8 |
| Governing law / forum | Brazil; company's seat, with the consumer's domicile preserved (CDC art. 101 I) and mandatory foreign consumer protections acknowledged | terms §12 |

## 2. Legitimate-interest balancing note (LGPD art. 10 / GDPR art. 6(1)(f))

- **Purpose:** keep a public UGC map safe — sessions, rate limits, audit trail,
  moderation of photos/texts, handling reports.
- **Necessity (proposed):** rate limiting and audit support a public upload
  surface. The app does not persist raw IP/user-agent in its database, but
  proxy/provider logs and rate-limiter keys must be considered separately.
- **Impact:** no marketing or profiling is implemented. A display name can be
  public under the account's attribution setting (enabled by default for new
  accounts); e-mail and voting identity are not public. User content can itself
  identify someone.
- **Safeguards:** provider minimization, EXIF stripping, access control,
  bounded retention and attribution removal. Counsel must confirm the balance.

## 3. Points for counsel to confirm or fix

1. **Wording review** of all six files (`policies/*.{pt-BR,en}.md`), especially
   the terms' licence (§3.3), indemnity (§3.4) and liability limits (§8) against
   the CDC, and whether the anonymous-display clause is compatible with moral
   rights (Lei 9.610 art. 24) — it is framed as the author's choice, not a waiver.
2. **Encarregado:** we rely on a contact channel rather than a named DPO. If the
   company is not an *agente de tratamento de pequeno porte* (Res. CD/ANPD
   2/2022), a named encarregado may be required — add the name to policy §1.
3. **International transfers:** identify every deployed provider, its region,
   legal role and onward transfers; obtain and review the actual contract/DPA;
   then select the applicable LGPD/GDPR mechanism. None is currently evidenced.
4. **GDPR/UK GDPR scope and governance:** determine territorial applicability
   under the relevant primary law. If applicable, assess representative, DPO,
   supervisory-authority and transfer duties; no exemption is assumed.
5. **Marco Civil art. 15:** assess whether the company is an application
   provider "com fins econômicos" and whether a proposed six-month proxy-log
   schedule would satisfy the "controlled environment" duty. The actual proxy
   fields and retention enforcement require owner evidence.
6. **Marco Civil art. 19 after the STF decision (June 2025):** confirm the
   notice-and-takedown duties that apply to a small UGC platform and whether the
   terms §4 channel and our moderation SLA are adequate.
7. **Future automated moderation:** if a classifier is proposed, assess its
   necessity, provider transfer, decision effects and notice before it ships;
   it is not part of the current factual policy.
8. **Retention numbers:** confirm the proposed 5 years for audit/rights-request
   records, 30-day shell purge, retained unattributed contributions, backup
   lifecycle and outage caveats. Retained text/photos may remain personal data.
9. **Children and adolescents:** the official text of Law 15.211/2025 covers
   services directed to or likely accessed by children/adolescents, so an 18+
   declaration is not a complete exemption. Assess probable access, the law's
   duties and Decree 12.880/2026/current ANPD guidance. Do not introduce routine
   identity-document collection without a necessity/proportionality assessment.
10. **Legal bases:** confirm each proposed LGPD/GDPR basis in the processing
    inventory, including access logs and rights-request records. A Brazilian
    legal duty is not automatically a GDPR Article 6(1)(c) duty.

Primary sources reviewed on 2026-09-14: [Law 15.211/2025](https://www.planalto.gov.br/ccivil_03/_ato2023-2026/2025/lei/l15211.htm), [Decree 12.880/2026](https://www.planalto.gov.br/ccivil_03/_ato2023-2026/2026/decreto/d12880.htm), the [ANPD ECA Digital overview and preliminary age-assurance guidance](https://www.gov.br/anpd/pt-br/assuntos/eca-digital/eca-digital-english), and the [official GDPR text](https://eur-lex.europa.eu/eli/reg/2016/679/oj).

## 4. Publication evidence gate

- Obtain counsel approval and owner evidence before setting
  `POLICY_OPERATOR_NAME/CNPJ/ADDRESS`, `POLICY_CONTACT_EMAIL`,
  `POLICY_VERSION` and `POLICY_EFFECTIVE_AT` or running `seed-policies`.
- Evidence inbox ownership, monitoring/coverage and the applicable response
  deadlines before publishing the contact promise.
- Record deployed providers/regions/roles and obtain contractual evidence per
  `docs/provider-transfer-inventory.md`; route it to counsel before making
  processor or transfer-mechanism claims.
- Evidence proxy access-log and diagnostic-log fields, access and retention;
  the app configuration does not enforce those external schedules.
- `DELETED_ACCOUNT_PURGE_AFTER_DAYS=30`; keep `INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS=0`.
- Hide/disable the fake Google login in production.
- After approval, use the controlled version/seed/notification workflow; source
  edits alone do not alter the policy already served from the database.
