# B06b correction handoff

This uncommitted correction addresses only the two findings in the independent
`B06b-review.md`. The reviewer record and remediation plan were not edited.

## Corrections

1. Resend 409 response classification now reads a response without a declared
   `Content-Length`, accumulating at most 1 KiB. The only permanent 409 is the
   allowlisted `invalid_idempotent_request`; the documented concurrent name and
   unclassifiable, malformed, hostile, absent-length, or oversized responses are
   conservatively retryable. Provider response text is never returned, logged,
   or persisted. A response-level test uses an ephemeral loopback HTTP stub and
   covers both known names with present and absent lengths plus oversized,
   malformed, and unreadable bodies. No provider is contacted.
2. Added fresh transaction-scoped registration recovery cases for expired,
   missing, succeeded, failed, actively running, and actively running exhausted
   jobs. Each captures the original account credential/display-name/locale,
   token, and job rows, then verifies exact preservation or a distinct fresh
   token/job as appropriate. Added late audit-insert failures for
   `auth.register` and `auth.email_change_requested`, after token/job admission,
   proving the complete account/token/job/audit aggregate rolls back.

## Verification

Database tests used only the disposable loopback audit database, sequentially,
with `DATABASE_URL` unset and the shared debug target directory.

- Resend focused unit tests: 5 passed.
- Infrastructure auth integration suite: 35 passed.
- Infrastructure job integration suite: 17 passed.
- `cargo fmt --all -- --check`: passed.
- Strict Clippy for application, infrastructure, and web, all targets, locked,
  with `-D warnings`: passed.
- `git diff --check 5728a46`: passed.

No migration, commit, deployment, production access, asset build, or external
provider action was performed.
