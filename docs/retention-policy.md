# Retention policy

> Periods marked **decided 2026-09-03** were set by product (see
> `docs/legal-review.md`); the technical defaults are encoded in
> `bikesnest_domain::RetentionPolicy` (env-configurable). The privacy
> policy (`policies/privacy.*.md`) states the same table — keep in sync.

| Record | Period | Mechanism | Status |
|---|---|---|---|
| password-reset token | 1 hour | `expires_at` + retention purge | technical default |
| email-verification token | 24 hours | `expires_at` + retention purge | technical default |
| transactional-mail recipient/link payload | until terminal delivery outcome, account deletion, or first successful retention sweep after token expiry | immediate terminal/deletion redaction; daily retention sweep for expired pending/retrying rows | token usability ends at its database `expires_at`; payload erasure can be delayed while jobs/retention are disabled or unavailable |
| session | 30 days idle / 90-day absolute cap | cookie Max-Age + `expires_at` + purge | technical default |
| "I parked here" | 90 days | `expires_at` + purge (and on deletion) | technical default |
| temporary privacy exports | 24 hours | `expires_at` + purge | technical default |
| temporary upload objects / rejected photos | 24 hours | orphan media sweep — **lists the S3 bucket** under `uploads/` and deletes aged, unreferenced keys | technical default |
| unusable `PENDING_REVIEW` photo rows | 1 hour | reconciliation step: a pending row whose object does not exist is deleted | technical default |
| **inactive accounts** | **not anonymized automatically** (`INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS=0`) | config-gated step stays off | **decided 2026-09-03** — no reminder e-mail exists; the policy promises advance notice before any inactivity deletion. Revisit if a reminder flow ships |
| **deleted (anonymized) account shells** | **30 days** (`DELETED_ACCOUNT_PURGE_AFTER_DAYS=30`) | `retention` command hard-deletes `users` rows with `account_state='DELETED' AND deleted_at < now()-30d` | **decided 2026-09-03** — the shell holds no personal data (email is `deleted+<id>@bikesnest.invalid`), the purge is housekeeping |
| **reviews / contributions / photos** | retained as the community dataset, **anonymized** on account deletion | anonymize-in-place | **decided 2026-09-03** — anonymized rows are no longer personal data. **Review bodies are kept verbatim**, only unattributed — see below |
| **reports / moderation records** | retained for service safety; reporter anonymized on deletion | anonymize-in-place | **decided 2026-09-03** |
| **access logs (date/time + IP)** | **6 months** | reverse-proxy / LB log retention (ops — `docs/deployment.md`) | **legal obligation** — Marco Civil da Internet art. 15 |
| **audit events** | **5 years** | `purge_audit_events_before(ts)` (no automatic step yet — see below). The table is otherwise append-only: a trigger refuses UPDATE and DELETE | **decided 2026-09-03**; aligned with the 5-year limitation period for consumer claims (CDC art. 27) |
| **privacy requests** | **5 years**, `user_id` nulled on deletion | manual/ops purge | **decided 2026-09-03** |
| diagnostic logs | ~30 days | log driver | ops default |

## Review bodies on account deletion

A deletion request **anonymizes** a review; it does not delete the text. The
body stays exactly as published and only stops being attributed (`author_id`
becomes NULL, and every other attribution column with it).

That is a deliberate decision, not an oversight. A review is community content:
the rating it contributes and the sentence it says about a parking spot are what
the service exists to publish, and other people's decisions rest on them.
Erasing bodies on request would let a contributor retract the substance of the
dataset long after other contributors built on it. The body is also not, by
itself, identifying — the identifying part is the link to an account, and that
link is what erasure removes.

The practical consequence: a user who wrote something identifying *inside* the
text of a review cannot have it removed by deleting their account. That is what
the manual **rectification** request kind is for (`MANUAL_REQUEST_KINDS`,
`docs/legal-review.md`), and an operator edits or hides the specific review.
Say so if a user asks.

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
`ON DELETE SET NULL` foreign key does by itself whenever an anonymized account
shell is hard-purged.

`audit_events.metadata` is **not** scrubbed, and `audit_events.target_id` stays
TEXT. Both are deliberate; migration 0019 and `AUDIT_METADATA_KEYS`
(`crates/infrastructure/src/auth/audit.rs`) carry the reasoning, and a test
fails if a metadata key appears that has not been classified.

## Implementation surface

- The seven technical-default purges plus the 30-day shell purge are driven by
  `cargo run -p bikesnest-web -- retention` (schedule it daily; see
  `docs/deployment.md`). The same steps run as the recurring `retention`
  background job.
- Mail delivery locks the account row, validates the exact account, purpose,
  recipient, token hash and database expiry, and holds that lock through the
  bounded provider call. If deletion commits first, even a worker's preclaimed
  in-memory payload is rejected. If the provider accepts first, deletion waits;
  that already-accepted external copy cannot be recalled. A timeout or lost
  database connection can leave the external acceptance outcome ambiguous, so
  this is not a distributed exactly-once guarantee.
- Terminal mail jobs retain only lifecycle metadata and a bounded error code;
  recipient/link JSON is cleared. Account deletion clears it in every queue
  state. Expired active rows are cleared by the next successful retention run,
  normally within the daily schedule, but no wall-clock erasure bound applies
  during an outage or when workers are disabled. Database backups are not
  rewritten: copies age out under the separately approved backup policy.
- The **orphan media sweep** lists the object store, a page at a time, under the
  `uploads/` prefix: it gates on age first, then probes the database in batches
  for keys a photo row still references, and deletes what is aged and
  unreferenced. A store it cannot list is an **error**, not a zero — the sweep
  previously walked a local `MEDIA_ROOT` directory that stopped existing when
  media moved to S3, swallowed the `read_dir` failure and reported success, so
  media retention was a silent no-op.
- `INACTIVE_ACCOUNT_ANONYMIZE_AFTER_DAYS` **must stay 0** unless the policy text
  is changed first (it promises notice).
- **Not yet automated:** the 5-year purge of `audit_events` and
  `privacy_request` rows. Nothing reaches that age before 2031; add a
  `retention` step (config-gated, `AUDIT_RETENTION_DAYS`) before then, or run a
  yearly manual purge. For `audit_events` that purge **must** go through
  `SELECT purge_audit_events_before(now() - interval '5 years');` — a bare
  `DELETE` is refused by the append-only trigger.
- **Not in the app:** the 6-month access-log retention lives at the proxy /
  hosting layer. Verify it is configured before launch.
