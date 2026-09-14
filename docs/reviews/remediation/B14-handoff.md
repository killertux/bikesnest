# B14 implementation handoff

## Outcome

B14 adds one shared bilingual renderer for explicit plain-text and escaped HTML
alternatives. New credential messages carry the authoritative token expiry;
legacy queued payloads without that field remain decodable and make no invented
duration claim. SMTP emits `multipart/alternative`, and Resend sends explicit
`text` and `html` fields.

Successful password replacement now commits the credential, session/reset-token
revocation, audit event, and a credential-free warning job together. Confirmed
email change similarly commits the canonical address, revocations, audit, and a
warning to the locked old address. The immutable notification id is serialized
once and supplies a stable queue/provider replay identity.
If post-commit inline dispatch of either warning fails, the committed security
transition still returns success and the durable row retains its queue outcome
(retryable or terminal); the failure is recorded with only account id and
allowlisted message kind.

At delivery, credential mail revalidates its token. Security notices require the
stored recipient digest, successful transition audit, and allowed account state;
password warnings additionally require the address still be canonical, while an
email-change warning is authorized for the old address. Terminal success/failure,
expiry cleanup, and account deletion clear payloads and the new personal-data
metadata. SMTP remains at-least-once; Resend idempotency remains limited by the
provider's documented retention window.

## Files and schema

- Application contract/use cases: `crates/application/src/{auth,email,lib}.rs`
- Atomic SQL and token lookup: `crates/infrastructure/src/auth/{outbox,token_store}.rs`
- Admission/delivery/provider lifecycle: `crates/infrastructure/src/{job,email,privacy}/`
- Renderer/catalog and visual regression: `crates/infrastructure/src/email/templates.rs`,
  `crates/i18n/src/lib.rs`, and `crates/infrastructure/tests/email_renderer_visual_test.rs`
- Forward-only migration: `migrations/0027_security_notice_mail.sql`
- Operational concepts: `AGENTS.md`, `ARCHITECTURE.md`, `docs/deployment.md`

Migration 0027 preserves legacy token jobs, extends the purpose constraint, and
adds nullable recipient-digest/audit-FK columns. The FK is `ON DELETE SET NULL`,
so supported audit retention is not blocked and unsent notices then fail closed.
Quiesce old workers before enabling writers for the new serialized variants;
upgrade/migrate workers first, then enable new writers. Rolling old workers back
over new notice rows is unsupported; pause mail and forward-fix.

## Verification

- `cargo test -p bikesnest-application --test auth_test --locked`: **31 passed**,
  including failing post-commit dispatch for reset, authenticated password
  change, and confirmed email change without false mutation failure/replay.
- Dedicated loopback disposable DB, infrastructure auth: **37 passed**. Includes
  unknown/cross-account/revoked/expired session rejection, atomic notice admission,
  competing reset invalidation, exact old-address delivery, rollback, and a real
  independent-connection row-lock race proving expiry is rechecked after waiting.
- Dedicated loopback disposable DB, job suite: **29 passed**. Includes credential
  lifecycle and unconfounded notice hash/audit/state/canonical-recipient rejection,
  allowed pending password warning, terminal metadata scrub, retry/lease behavior.
- Dedicated loopback disposable DB, privacy suite: **22 passed**. Includes all-state
  token/notice deletion scrubbing and an isolated pre-0027 upgrade fixture proving
  legacy preservation, new purposes, and authorized audit purge nulling the FK.
- `cargo test -p bikesnest-infrastructure --lib email:: --locked`: **11 passed**
  with loopback enabled for the bounded Resend response matrix. SMTP serialization
  asserts `multipart/alternative`, `text/plain`, and `text/html`; Resend asserts
  stable request identity/payload and both alternatives.
- Renderer-specific unit tests: **2 passed**. Local Chromium visual harness:
  **48 assertions passed** across 16 documents/kinds/locales, blocked images,
  narrow viewport, and dark-preference emulation. Final inspected artifacts:
  `/tmp/b14-email-visual-xsGT0G`.
- Sequential HTTP regression on the dedicated audit DB: **180 passed**.
- `cargo check --workspace --all-targets --locked`: passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `cargo fmt --all -- --check`: passed.

## Honest limitations / external gates

- No live SMTP or Resend message was sent. Provider-console delivery/rendering,
  Gmail/Outlook client quirks, and real-client dark-mode auto-inversion remain
  external evidence; the browser check only emulates `prefers-color-scheme`.
- The product has no owner-confirmed monitored help address/route in scope. A real
  support inbox, DNS/provider setup, and ownership/SLA must be approved before a
  help contact is added; the notices do not invent one.
- Resend supports explicit HTML/text and a 24-hour idempotency retention window:
  <https://resend.com/docs/api-reference/emails/send-email> and
  <https://resend.com/docs/dashboard/emails/idempotency-keys>. Lettre's SMTP
  alternative builder is documented at
  <https://docs.rs/lettre/0.11.23/lettre/message/struct.MultiPart.html>.
- The sequential HTTP result does not resolve the separately tracked historical
  parallel-fixture flake.
