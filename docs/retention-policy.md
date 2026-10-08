# Retention policy

> This records application defaults and proposed operational/legal periods.
> Application defaults are encoded in `bikesnest_domain::RetentionPolicy` and
> are environment-configurable. External log/provider/backup schedules require
> operator evidence, and proposed legal periods require counsel review. Keep
> approved public promises in `policies/privacy.*.md` in sync.

| Record | Period | Mechanism | Status |
|---|---|---|---|
| password-reset token | 1 hour | `expires_at` + retention purge | technical default |
| email-verification token | 24 hours | `expires_at` + retention purge | technical default |
| transactional-mail recipient/message data, credential links and notice recipient hashes | until terminal delivery outcome, account deletion, or first successful retention sweep after credential-token expiry | immediate terminal/deletion redaction; retention sweep for expired pending/retrying credential rows | credential usability ends at database `expires_at`; payload/metadata erasure can be delayed while jobs/retention are disabled or unavailable |
| session | 30 days idle / 90-day absolute cap | cookie Max-Age + `expires_at` + purge | technical default |
| "I parked here" | 90 days | `expires_at` + purge (and on deletion) | technical default |
| temporary privacy exports | 24 hours | `expires_at` + purge | technical default |
| temporary upload objects / rejected photos | 24 hours | orphan media sweep — **lists the S3 bucket** under `uploads/` and deletes aged, unreferenced keys | technical default |
| unusable `PENDING_REVIEW` photo rows | 1 hour | reconciliation step: a pending row whose object does not exist is deleted | technical default |
| **inactive accounts** | no automatic de-identification (`INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS=0`) | config-gated step stays off | application default; changing it requires notice/workflow review |
| **deleted account shells** | **30 days** (`DELETED_ACCOUNT_PURGE_AFTER_DAYS=30`) | `retention` command hard-deletes `users` rows with `account_state='DELETED' AND deleted_at < now()-30d` | application default; shell identifiers are substituted/cleared, but avoid a blanket “no personal data” claim while retained relations/content may remain |
| **reviews / contributions / photos** | retained as the community dataset with account/public-name attribution removed on deletion | de-identify account relationships in place | proposed product period; free text/photos remain verbatim and may still be personal data — see below |
| **reports / moderation records** | retained for service safety; reporter account link removed on deletion | de-identify relationship in place | proposed product period; content may remain personal data |
| **access logs (date/time + IP)** | proposed **6 months** | reverse-proxy / LB configuration, outside app enforcement | operator must evidence actual fields/schedule; counsel to confirm Marco Civil applicability |
| **audit events** | proposed **5 years** | `purge_audit_events_before(ts)` exists, but no automatic step yet | counsel to confirm period/basis; operations must schedule it before any promise is effective |
| **privacy requests** | proposed **5 years**, `user_id` nulled on deletion | manual/ops purge | counsel to confirm period/basis; no automatic purge yet |
| diagnostic logs | proposed ~30 days | hosting/log-driver configuration | operator evidence pending; not enforced by app |

## Retained content on account deletion

A deletion request removes the account relationship and public attribution
from a review; it does not delete or rewrite the text. The body stays exactly
as published while `author_id` and the other attribution columns are cleared.

The product reason for retaining community facts is dataset continuity, but a
body or image can identify its author or another person on its own. Attribution
removal is therefore not a guarantee of anonymization and does not decide a
person's separate right to erasure, restriction, rectification or content
removal.

The account-deletion workflow does not remove identifying material inside free
text or photos. Route a request concerning the content itself through the
manual rights/content-removal workflow (`MANUAL_REQUEST_KINDS`) for an
individual assessment; do not tell a user that account deletion resolved it.

## Audit-event integrity

`audit_events` is append-only, enforced by the `audit_events_append_only`
trigger (migration 0019). Exactly two mutations are sanctioned, and each
announces itself by setting `app.audit_purge` for its own transaction:

- **the LGPD erasure scrub** (`privacy/anonymize.rs`) — nulls `actor_user_id`
  and rewrites an `target_id` that holds the account's e-mail (a failed login is
  audited with the attempted address, because no user id resolved);
- **the retention purge** — `SELECT purge_audit_events_before(ts)`, a
  `SECURITY DEFINER` function, which is how the 5-year purge below must be run.

Independently of that setting, one narrow UPDATE is always allowed: nulling
`actor_user_id` with every other column unchanged. That is what the
`ON DELETE SET NULL` foreign key does by itself whenever a de-identified account
shell is hard-purged.

`audit_events.metadata` is **not** scrubbed, and `audit_events.target_id` stays
TEXT. Both are deliberate; migration 0019 and `AUDIT_METADATA_KEYS`
(`crates/infrastructure/src/auth/audit.rs`) carry the reasoning, and a test
fails if a metadata key appears that has not been classified.

## Implementation surface

- The technical-default purges plus the 30-day shell purge are driven by
  `cargo run -p bikesnest-web -- retention` (schedule it daily; see
  `docs/deployment.md`). The same steps run as the recurring `retention`
  background job.
- A failing step does not stop the others: every step still runs, the
  `retention.purged` audit event is written with `result = "failure"` and the
  failed step marked `"failed"` under `steps`, and the run reports an error
  naming the failed steps (an object-store outage in the orphan sweep no longer
  skips account purges). The job retries with backoff; if that occurrence
  exhausts its attempts the error is kept in `background_job.last_error` and the
  job moves to its next daily run instead of being dead-lettered, so a brief
  outage delays retention by one cycle rather than stopping it.
- Mail delivery locks the account row, validates the exact account, purpose,
  recipient, token hash and database expiry, and holds that lock through the
  bounded provider call. If deletion commits first, even a worker's preclaimed
  in-memory payload is rejected. If the provider accepts first, deletion waits;
  that already-accepted external copy cannot be recalled. A timeout or lost
  database connection can leave the external acceptance outcome ambiguous, so
  this is not a distributed exactly-once guarantee.
- Terminal mail jobs clear message content, credential-link metadata,
  recipient hash and transition-audit reference; account deletion clears the
  same sensitive queue data in every state. Expired active credential rows are
  cleared by the next successful retention run, but no wall-clock erasure bound
  applies during an outage or while workers/retention are disabled. Database
  backups are not rewritten in place: operator must evidence their lifecycle,
  and a restored backup must reconcile deletions before normal use.
- The **orphan media sweep** lists the object store, a page at a time, under the
  `uploads/` prefix: it gates on age first, then probes the database in batches
  for keys a photo row still references, and deletes what is aged and
  unreferenced. A store it cannot list is an **error**, not a zero — the sweep
  previously walked a local `MEDIA_ROOT` directory that stopped existing when
  media moved to S3, swallowed the `read_dir` failure and reported success, so
  media retention was a silent no-op.
- `INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS` **must stay 0** unless the policy text
  is changed first (it promises notice). When enabled, inactivity is measured
  from `users.last_active_at`, which sign-in and session refresh advance and
  which survives the session purge (the purge folds each deleted session's
  `last_seen_at` into it). Each candidate is re-checked under its row lock, so
  an account that signs in while the job runs is skipped.
- **Not yet automated:** the proposed 5-year purge of `audit_events` and
  `privacy_request` rows. After counsel approves a period, schedule a retention
  step before the earliest retained row reaches it (config-gated,
  `AUDIT_RETENTION_DAYS`) or establish an evidenced manual process. For
  `audit_events` that purge **must** go through
  `SELECT purge_audit_events_before(now() - interval '5 years');` — a bare
  `DELETE` is refused by the append-only trigger.
- **Not in the app:** the proposed 6-month access-log retention lives at the
  proxy/hosting layer. Verify actual fields and enforcement before publishing
  the period.
