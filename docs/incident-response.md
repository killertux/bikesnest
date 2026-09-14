# Incident response

Operator runbook, reviewed against the cited sources on 2026-09-14. It is not
evidence that alerts, staffing, a protected incident-record system, or legal
approval are already in place. This remediation does not authorize a production
change, regulatory filing, or notification campaign.

## Readiness and responsibility

The service owner must fill and verify this roster outside the public repository
before relying on the runbook. Do not put personal contact details or credentials
in this file.

| Responsibility | Required owner evidence |
|---|---|
| Incident commander and alternate | Named people, monitored escalation channel, coverage and acknowledgement procedure |
| Controller decision-maker / privacy contact | Confirmed identity, authority, reachable contact and backup |
| Security/operator lead | Approved access, containment authority and evidence-handling procedure |
| Legal/privacy adviser | Applicable jurisdictions, reporting thresholds and deadline calculation |
| External communications | Authorized sender, regulator account/representation, approved delivery channels |
| Incident record | Protected case system, access list, retention rule and recovery test |

Missing roster information is a readiness gap, not a reason to postpone
containment or awareness-time recording. Escalate to the service owner immediately.

## 1. Detect, record and escalate

Open a protected incident case at the first credible signal. Record detection
time, occurrence window if known, and the distinct time the controller becomes
aware that personal data was affected. Keep timezone/UTC offsets and the evidence
supporting those times; later certainty must not silently reset a reporting clock.

Investigative sources include structured application logs, existing classified
audit events, job outcomes and provider reports. Verify which alerts and log
retention are actually configured. Do not assume all security actions emit an
audit row, all provider requests are logged, or every log is free of personal
data. Preserve relevant evidence with access controls and avoid copying tokens,
passwords or complete personal records into diagnostic logs or public issues.

Escalate immediately to the incident commander and controller/privacy lead, with
security and legal in parallel. Record who acknowledged, the next checkpoint and
the deadline owner. If nobody acknowledges, use the verified alternate; do not
wait for a completed investigation to escalate.

## 2. Contain and assess

Classify severity and whether personal data is involved. With appropriate
operational authority, consider isolating affected components, rotating exposed
credentials, revoking sessions/tokens, or restricting compromised content. These
are incident-specific decisions, not instructions to run blanket destructive
commands. Account export is not a session-revocation tool. Preserve evidence and
record the exact targets, actions and recovery plan.

Use [the data inventory](data-processing-inventory.md) and
[provider inventory](provider-transfer-inventory.md) to assess affected people,
data categories, systems, recipients, duration and likely consequences. Include
free text, photos, URLs, logs, queue payloads, exports and provider/backup copies;
removing account attribution does not guarantee those copies are anonymous.

Record known facts, unknowns, confidence, mitigations and the next evidence owner.
Use [backups.md](backups.md) for a separately authorized restore; a restore must
also reconcile later erasures and revoked credentials.

## 3. Reporting clocks and decision record

Legal must confirm scope and applicable exceptions promptly. Use the shorter
working deadline until any extension is supported; an internal approval process
does not stop a statutory clock. The following is a planning baseline, not an
assertion that every incident is reportable or that every listed law applies.

| Regime | Planning baseline | Decision/evidence to retain |
|---|---|---|
| Brazil / LGPD | For a confirmed personal-data incident capable of relevant risk or harm, notify ANPD and affected people within **3 business days** of controller knowledge that personal data was affected, subject to specific-law rules. | Awareness time, risk assessment, business-day calendar, deadline, authorized filer and reasons for any delay/non-notification. |
| Incomplete ANPD report | Submit a justified preliminary report when information is incomplete; track the **20-business-day** complementary-report deadline from communication. | Missing fields, owners, filing receipt, supplement deadline and actual supplement. Preliminary filing is not closure. |
| EU GDPR, if applicable | Authority: without undue delay and where feasible within **72 hours** of awareness, unless unlikely to risk people's rights/freedoms. People: without undue delay where **high risk** is likely. | Separate authority/person decisions, applicable authority, awareness time, delay reasons, and any art. 34 exception with evidence. Do not replace 72 hours with business days. |

Brazil sources: [ANPD incident guidance, questions 2, 4 and 5](https://www.gov.br/anpd/pt-br/canais_atendimento/agente-de-tratamento/comunicado-de-incidente-de-seguranca-cis)
and [Resolution 15/2024, official publication reproduced by TJPE](https://portal.tjpe.jus.br/documents/d/seguranca/resolucao-cd_anpd-n-15-de-24-de-abril-de-2024-resolucao-cd_anpd-n-15-de-24-de-abril-de-2024-dou-imprensa-nacional?download=true).
Apply its article 5 assessment: significant effects on rights/interests together
with a listed criterion, such as authentication, sensitive or protected data,
vulnerable people's data, financial data or large-scale processing. Record the
actual criteria, not just a generic severity label.
Eligible small processing agents have special doubled periods; counsel must
verify eligibility and the current rule, not infer it from a small team. See
[ANPD Resolution 2/2022, as amended](https://www.gov.br/anpd/pt-br/acesso-a-informacao/institucional/atos-normativos/regulamentacoes_anpd/resolucao-cd-anpd-no-2-de-27-de-janeiro-de-2022).

EU source: [GDPR articles 33–34](https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX%3A32016R0679).
Document any phased authority report and follow up without undue further delay.
UK applicability and current UK requirements need their own legal confirmation;
do not treat an EU filing as automatically satisfying a UK obligation.

For each jurisdiction record the applicability and risk decision, facts relied
on, decision-maker, deadline and next reassessment. Preserve a reasoned
non-notification decision too. Provider contractual notices may have different
deadlines: check the actual contract and contact route separately.

## 4. Prepare and send only through authorized channels

Use the regulator's current form and filing instructions. Have the authorized
controller representative confirm representation/access and confidentiality
classification before filing. Do not put a list of affected people in a public
filing or repository. Record the filing receipt and any supplementary submission.
For Brazil, also track the article 9(4) declaration of affected-person
communication: due within three business days after that communication deadline
ends. If direct notification is infeasible, article 9(3) requires prominent
alternative disclosure for at least three months; legal must approve the method.

Use this internal drafting checklist; legal and the communications owner must
approve content, recipients, language, channel and timing:

```text
Case ID; applicable regime; awareness/occurrence dates; draft status
What happened and what remains unknown
Affected data categories and estimated people/records
Likely consequences and protective steps for people
Containment and remediation completed/planned
Verified contact for questions; next update timing
Required form fields, approval, deadline and delivery evidence
```

Write affected-person notices in clear language appropriate to the recipient.
Do not speculate, promise that risk is eliminated, include credentials, or
claim successful delivery from enqueue alone. Check lawful alternatives if
direct notification is not possible. The existing transactional email kinds
are not an implemented incident-campaign tool; an approved sending workflow is
still required. Never repurpose password-reset mail for a breach notice.

## 5. Recover, verify and retain the record

Remediate the cause, verify the fix and any restore/erasure reconciliation,
and record residual risks plus the next checkpoint. Close only after required
notices/supplements and follow-up work are accounted for. Review detection,
response timing, access and test gaps with the owner.

Keep the case and evidence in the approved protected system under its reviewed
retention schedule; see [retention-policy.md](retention-policy.md).
Brazilian incident records, including non-notified incidents, have an article 10
minimum of five years from registration, subject to longer applicable duties.
Do not substitute diagnostic-log expiry for this case-record requirement.
Suggested milestones are opened, contained, assessed, escalated, notified and resolved.
They are operator record labels, not automatic `incident.*` events implemented
by this app. Existing audit access controls do not automatically protect a case
stored elsewhere. Verify diagnostic-log retention and backup rules separately;
neither source configuration nor this document proves an operational purge ran.
