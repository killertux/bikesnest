# B15a independent review

Date: 2026-09-14  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `cd6e879`  
Decision: **PASS**

I read the B15a handoff and remediation-plan acceptance criteria, inspected the
complete frozen policy/runbook/inventory diff except the root-owned plan ledger,
checked the implementation facts referenced by the drafts, independently
rendered all six policy files, and checked the time-sensitive legal statements
against official sources. This is an engineering/factual review, not legal
certification. I made no application, policy, plan, seed, version, publication,
provider, notification, deployment or production change.

## Review disposition

No material findings remain. The first review requested four documentation-only
corrections; the frozen correction pass resolves each one in both meaning and
structure:

- the bilingual privacy drafts now label legitimate interest as the proposed
  basis and retain the objection right without claiming a completed balancing
  assessment;
- the conditional consent-withdrawal right remains, while the categorical claim
  that no consent-based processing exists has been removed from both locales;
- the Marco Civil entry in `docs/legal-review.md` now calls six-month proxy-log
  retention a proposed schedule and requires owner evidence for actual fields
  and enforcement; and
- `docs/retention-policy.md` now ties scheduling to the earliest retained row
  reaching a counsel-approved period, without inferring production row age from
  repository or migration dates. The corrected continuation lines remain
  properly nested under the same list item.

The corrections do not expand the licence, indemnity, liability or
jurisdictional legal scope.

## Accepted portions and remaining gates

- The English and Portuguese policy structures and factual product descriptions
  are parallel. They accurately distinguish public attribution removal from
  anonymity/content erasure; new listing/review publication from the six-vote or
  moderator workflow for existing facts; and moderator-held photos.
- The browser-storage correction matches the only guarded application
  `localStorage` reads/writes for `bn.search.mapOpen`. The Cloudflare language is
  limited to the browser telemetry observed on 2026-09-08 and the provider's
  own description; it does not invent payload, cookie, retention, role or
  consent conclusions.
- Provider and transfer inventories no longer assume every provider is foreign,
  a processor, covered by a signed DPA, or governed by a selected transfer
  mechanism. Hosting, storage, e-mail, map, edge, OAuth and logs all retain
  explicit deployed-account evidence fields.
- Retention wording correctly distinguishes application defaults from external
  schedules and proposed legal periods, and accurately records B14 queue
  scrubbing, provider-acceptance ambiguity, outage delay and backup-restoration
  reconciliation. Purge readiness is now expressed relative to the earliest
  retained row and a counsel-approved period rather than an assumed calendar
  year.
- The incident runbook does not invent a staffed roster, protected case system,
  `incident.*` automation, configured alerts or authority to file, notify or run
  destructive containment. Current ANPD guidance supports its cumulative
  reportability criteria, three-business-day period, justified preliminary
  filing and 20-business-day complement. Amended Resolution 2 supports doubled
  periods for eligible small processing agents, subject to eligibility. The
  official GDPR text supports the separate 72-hour authority and high-risk
  affected-person rules; the runbook correctly does not conflate EU and UK law.
- The minors correction appropriately treats 18+ as a product/account rule, not
  a complete exemption from probable-access duties, and avoids introducing
  routine identity-document collection.

Counsel must still approve applicability, legal bases, rights/content handling,
age duties, response and retention periods, terms and transfer mechanisms. The
owner must supply the legal entity/contact, monitored inbox, deployed provider,
contract, region, proxy/log, backup and edge evidence. No policy may be seeded,
versioned, published or used for a notification campaign until those gates are
satisfied.

## Independent commands and results

```text
git diff --name-status cd6e879
git diff --stat cd6e879
git status --short
git diff cd6e879 -- <all changed policy/runbook/inventory paths>
rg -n 'localStorage|bn\.search\.mapOpen|cloudflareinsights' \
  web/static/js/app.js crates/web/src/security.rs policies docs
rg -n 'incident\.' crates
```

Result: complete tracked and untracked B15a inventory inspected. The application
contains only the guarded `bn.search.mapOpen` get/set pair; no implemented
incident-case event workflow was found.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test policy_draft_test --locked
```

Result: **1 passed, 0 failed**. The real placeholder filler and Markdown
renderer processed all six bilingual drafts without seeding or publishing them.

```text
cargo fmt --all -- --check
git diff --check cd6e879 -- . \
  ':(exclude)docs/plans/2026-09-08-audit-remediation.md'
```

Result: formatting and non-ledger diff validation passed.

Official sources independently checked on 2026-09-14:

- ANPD, “Comunicação de Incidente de Segurança” (current FAQ and procedure):
  <https://www.gov.br/anpd/pt-br/canais_atendimento/agente-de-tratamento/comunicado-de-incidente-de-seguranca-cis>
- ANPD Resolution 2/2022 consolidated with the Resolution 15 amendment:
  <https://www.gov.br/anpd/pt-br/acesso-a-informacao/institucional/atos-normativos/regulamentacoes_anpd/resolucao-cd-anpd-no-2-de-27-de-janeiro-de-2022>
- Regulation (EU) 2016/679, Articles 33–34:
  <https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32016R0679>
- Cloudflare Web Analytics provider documentation:
  <https://developers.cloudflare.com/web-analytics/about/>

The DOU endpoint for Resolution 15 remained unavailable; the handoff records
the government-hosted TJPE reproduction used for its article 5, 9 and 10 text.
That fallback does not eliminate the owner/counsel publication gate.
