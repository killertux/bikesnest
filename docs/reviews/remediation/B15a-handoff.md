# B15a implementation handoff

Status: implementation complete; no policy was seeded, published or versioned.
This is an engineering draft for independent review and counsel/owner evidence.

## Changed surface

- `policies/privacy.{en,pt-BR}.md`
  - describes browser coordinates at their actual precision/source boundary and
    the URL/history/proxy/provider-log caveat instead of saying “never stored”;
  - inventories `bn.search.mapOpen` and Cloudflare edge page-view/performance
    telemetry without claiming facts about an uninspected deployed payload;
  - describes current publication gates: new listings/reviews may be immediate,
    existing-fact changes need six eligible approvals or a moderator to become
    published facts, and photos require moderator approval;
  - distinguishes removal of account/name attribution from anonymity or removal
    of retained free text/photos, and identifies content removal as a separate
    rights request;
  - records transactional-mail scrubbing, provider-accepted-copy, outage and
    backup-restoration limits; and
  - removes the false absolute that sensitive data cannot be collected and the
    assumption that an 18+ account rule resolves all duties to minors.
- `policies/terms.{en,pt-BR}.md`: factual moderation and account-deletion wording
  only. Licence, indemnity, liability and other legal scope were not expanded.
- `docs/legal-review.md`: converts provider, transfer, GDPR/UK, DPO/legal-basis,
  retention and age assertions into explicit counsel/owner evidence gates.
- `docs/data-processing-inventory.md`: aligns real browser/provider/content
  flows, attribution defaults and proposed (not adjudicated) legal bases.
- `docs/provider-transfer-inventory.md`: removes unsupported all-foreign,
  all-processor and signed-DPA/SCC assurances; adds evidence fields for the
  selected hosting/storage/e-mail/map/edge configuration.
- `docs/retention-policy.md`: distinguishes application defaults from external
  schedules/proposed legal periods and records B14 mail queue scrubbing,
  provider acceptance, outages and backup restoration honestly.
- Root separately updated the bilingual cookie drafts and incident-response
  workflow and added the policy renderer smoke. This handoff does not attribute
  those files to this implementation slice.

## Evidence checked

- Source inventory: `web/static/js/app.js` persists `bn.search.mapOpen` without
  an application TTL; `crates/web/src/security.rs` contains the optional
  Cloudflare Insights boundary; infrastructure tests establish the default-on
  new-account public-name setting and the sixth eligible approval transition.
- The policies retain equal structure: privacy has 13 level-two sections and
  35 Markdown table lines in each locale; terms has 13 sections in each locale.
  Both locale pairs contain the same four supported operator placeholders.
- Current official sources reviewed 2026-09-14:
  - [Brazilian Law 15.211/2025](https://www.planalto.gov.br/ccivil_03/_ato2023-2026/2025/lei/l15211.htm)
    applies by intended or probable access, not only an account-age label;
  - [Decree 12.880/2026](https://www.planalto.gov.br/ccivil_03/_ato2023-2026/2026/decreto/d12880.htm);
  - [ANPD ECA Digital overview and preliminary age-assurance guidance](https://www.gov.br/anpd/pt-br/assuntos/eca-digital/eca-digital-english);
  - [official GDPR text](https://eur-lex.europa.eu/eli/reg/2016/679/oj), used to
    leave territorial scope, representative/DPO and transfer duties for legal
    assessment rather than assume an exemption.
- Root's incident-response source review also used the current ANPD FAQ,
  Resolution 2 and a government-hosted reproduction of Resolution 15 because
  the DOU URL returned HTTP 502:
  [Resolution 15 reproduction](https://portal.tjpe.jus.br/documents/d/seguranca/resolucao-cd_anpd-n-15-de-24-de-abril-de-2024-resolucao-cd_anpd-n-15-de-24-de-abril-de-2024-dou-imprensa-nacional?download=true).

## Checks run

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-web --test policy_draft_test --locked
=> 1 passed; 0 failed

cargo fmt --all -- --check
=> pass

git diff --check
=> pass
```

The renderer smoke fills only the known operator placeholders and renders all
six bilingual Markdown drafts through the real policy renderer. It does not
seed the database or certify legal correctness/publication readiness.

## Required external gates / limitations

- Counsel must approve jurisdictional applicability, proposed legal bases,
  retained-content rights handling, age/probable-access duties, response and
  retention periods, controller contact/DPO/representative duties, terms and
  international-transfer mechanisms.
- The owner must supply the legal entity/contact, prove inbox coverage, and
  inventory the actual hosting, backup, proxy/log, object-storage, e-mail,
  geocoder/map/tile and edge accounts. No `.env`, provider console, contract or
  production configuration was inspected.
- Revalidate the Cloudflare feature observed on 2026-09-08 against the deployed
  edge account: payload, cookie/storage behaviour, purpose, retention, legal
  role and any consent requirement remain unproven by source code alone.
- External log, provider and backup erasure schedules are not enforced by the
  application. Retention jobs can be delayed in an outage; already accepted
  e-mail cannot be recalled; restored backups require deletion reconciliation.
- Do not run `seed-policies`, change the effective version or notify users until
  the evidence and counsel gates are complete. No such action was taken here.

## Independent-review correction pass

The requested bounded corrections were applied after the initial handoff:

- privacy §3 now labels legitimate interest as the proposed basis and preserves
  the objection right without claiming that a balancing assessment is complete;
- privacy §9 retains the conditional withdrawal right without asserting that no
  consent-based processing exists while the edge configuration is unresolved;
- the Marco Civil proxy-log entry now labels six months as proposed and requires
  owner evidence for actual fields and enforcement; and
- retention scheduling is tied to the earliest retained row reaching a
  counsel-approved period, with no unsupported “nothing reaches it before 2031”
  assertion. The `ON DELETE SET NULL` paragraph indentation was also corrected.

Post-correction validation repeated the policy renderer (1 passed, 0 failed),
`cargo fmt --all -- --check` and `git diff --check` successfully.
