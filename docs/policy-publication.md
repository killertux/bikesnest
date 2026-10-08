# Policy publication and acknowledgement controls

This is an operator workflow, not authorization to publish a policy, enable
acknowledgements, send messages, or change production. Source drafts and passing
tests are not legal approval. Record the owner, counsel decision, exact release
and environment in the change record before performing any external action.

## What the application records

- A policy release contains privacy, terms and cookies in both English and
  Brazilian Portuguese. Each document has an immutable identity, version,
  effective time and content. Reusing a version is an identical replay, not an
  editing mechanism. Corrections require a new approved release.
- The currently effective document remains current while a later release is
  scheduled. Exact-version links identify the text shown, including its locale;
  a future document is not silently substituted for current terms.
- When enabled, registration records an acknowledgement of the exact current
  terms with account creation and the verification-mail outbox transaction.
  Existing-account registration recovery does not manufacture historical proof.
- Explicit in-product acknowledgement records the version, exact localized
  document, server timestamp and source. Satisfaction is by shared release
  version, so changing language does not require acknowledging that same release
  again. These records are not blanket consent for personal-data processing.
- A notice presentation records a server-prepared response. It does **not**
  establish that the browser received it, that a person read or understood it,
  that an email was delivered, or that the person acknowledged the terms.
- Current unacknowledged material terms and the nearest upcoming material
  release can be reviewed separately. Future terms cannot be acknowledged before
  they become current. Notices do not impose a service-wide lockout or remove
  access to security, privacy, deletion or logout controls.
- Account export includes this evidence. Account de-identification removes the
  account-linked notice and acknowledgement records; do not assume those records
  survive deletion as a separate legal archive. Any different retention duty
  requires an approved design, not an informal export to another system.

## Approval and external evidence gate

Before publication or activation, record:

1. Counsel-approved bilingual text, materiality decision, applicability,
   notification method and timing, and any acknowledgement requirement. Verify
   that both languages identify the same legal release.
2. The actual controller/contact details and monitored contact arrangements.
   Resolve the provider, transfer, retention and age-related evidence gates in
   [the legal review](legal-review.md) and its linked inventories.
3. The exact version, effective timestamp with timezone, source revision and
   rendered six-document review. No unresolved placeholders or development
   identities may enter the approved release.
4. The affected audience, channel coverage and evidence needed for the promised
   notice. In-product availability does not reach users who do not visit. This
   feature sends **no material-policy-change email**. Do not claim it fulfils an
   email promise or an obligation to notify every affected person.
5. A fleet-consistent activation plan, approved migration window and rollback
   decision. Instances with different feature settings can treat signup
   differently; a rolling configuration change alone is not proof of consistent
   acknowledgement capture.

No receipt, acknowledgement or legal decision may be backfilled from account
creation, verification, login, old terms links, or the absence of an objection.

## Migration and data preflight

Migration `0028_terms_acknowledgement.sql` adds evidence tables and policy
immutability/effective-date constraints. Forward migration
`0029_policy_version_id_immutable.sql` also prevents changing a document's ID
during its first supersession. Release both migrations; 0028 alone does not
provide the complete identity safeguard. Inspect the exact migrations
being released and test both fresh installation and upgrade on an approved
disposable copy before the production window.

This read-only query identifies one class of upgrade blocker:

```sql
SELECT kind, locale, effective_at, count(*)
FROM policy_version
GROUP BY kind, locale, effective_at
HAVING count(*) > 1;
```

Also inspect release completeness, overlapping or ambiguous schedules, and
agreement of version/effective time across the six documents. An empty result
from that one query does not certify release coherence or deployment readiness.
Do not delete, relabel, overwrite or invent acceptance for existing records to
make a migration pass. Escalate any conflict with its exact rows and preserve the
published history; resolution needs a separately reviewed decision.

Verify the backup/restore and deletion-reconciliation procedure before the
window. Application rollback is not permission to restore old personal data or
drop the new proof tables.

Test the proposed rollback binary against the migrated schema on the approved
disposable target. Schema/startup compatibility alone is insufficient: a
pre-B15b binary does not implement the acknowledgement gate, regardless of the
environment flag. After activation, do not serve registration traffic from such
a binary. Use a reviewed acknowledgement-aware rollback target, or pause/drain
registration traffic until a fleet-consistent acknowledgement-aware state is
restored. Record any interval in which registrations escaped that boundary;
never backfill fictitious proof for it.

## Controlled release sequence

1. Deploy the reviewed code and forward-only migration with
   `POLICY_ACKNOWLEDGEMENT_ENABLED=false` across the fleet. Confirm the flag is
   consistently off and existing behavior remains available. This does not
   publish the source drafts or record new acknowledgement evidence.
2. Prepare the approved six documents and `POLICY_OPERATOR_*`,
   `POLICY_CONTACT_EMAIL`, `POLICY_VERSION` and `POLICY_EFFECTIVE_AT` inputs.
   `POLICY_TERMS_MATERIAL_NOTICE=false` is the default. Set it to true only for
   the owner-approved material release; it is seed metadata, not the runtime
   acknowledgement switch.
3. After explicit publication approval, use the existing `seed-policies`
   command in the approved environment. Its release transaction installs all
   six documents together. An identical replay is safe; changed content or
   metadata under the same version is a conflict. Resolve a rejected partial or
   out-of-order release rather than retrying with arbitrary new versions.
4. Verify the exact stored release, history and current/future pages in both
   languages. A future release must not hide currently effective terms. Seeding
   a future document makes it available through exact/history views; do not
   treat the effective timestamp as a confidentiality embargo.
5. Activate `POLICY_ACKNOWLEDGEMENT_ENABLED=true` only after explicit approval
   and a coordinated, fleet-consistent transition. Validate current coherent
   terms before allowing enabled registration traffic. Schedule activation and
   the approved notice channels early enough for the agreed advance-notice
   period; do not wait until the new release is already effective.
6. Verify enabled signup and existing-account recovery, current and upcoming
   notices, locale changes, stale-form recovery, privacy access and the exact
   persisted evidence. Missing/incoherent policy data or a storage outage must
   not silently create an account with invented acknowledgement evidence.

Operator-created accounts and any alternate account-creation integration need
an explicit evidence policy. Do not infer terms acceptance from an OAuth
callback. Follow the released configuration's compatibility checks; do not
bypass them to activate an unsupported combination.

## Failure, rollback and handoff

- Before a seed transaction commits, failure must leave the prior six-document
  release intact. After a successful publication, changing files or reverting
  the application does not retract the stored release.
- Published document content and identity are immutable. Do not use direct SQL
  updates/deletes as rollback. Review any replacement release and its notice
  obligations separately.
- If acknowledgement processing must be disabled, coordinate the whole fleet
  and explicitly record the interval during which signup proof is not captured.
  Decide whether traffic must be paused rather than silently accepting that
  gap. Disabling the feature does not withdraw a published policy or erase an
  existing acknowledgement.
- Rolling back to an unaware binary is not equivalent to a controlled feature
  disable. Apply the rollback-binary and registration-traffic preflight above;
  an enabled environment variable cannot make old code enforce a new gate.
- Preserve evidence with its actual semantics. A prepared notice is not a
  delivery receipt; an idempotent retry is not a second acknowledgement.
- Attach the source revision, approved inputs, migration/fresh-upgrade results,
  rendered-browser checks, fleet verification and remaining external evidence
  to the release record. Do not mark deployment complete based on local tests.

The opt-in rendered regression runs only against a separately verified
disposable test target with local Chromium and fake providers:

```text
env -u DATABASE_URL TEST_DATABASE_URL=<approved-disposable-test-url> \
  cargo test -p bikesnest-web --test policy_browser_test --locked -- --ignored
```

Use the safeguards in [TESTING.md](../TESTING.md). Never substitute the
production connection string or source production environment files for this
test command.
