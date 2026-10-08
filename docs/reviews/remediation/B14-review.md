# B14 independent review

Date: 2026-09-14  
Reviewer: independent GPT Sol (`b06a_independent_review`)  
Baseline: `eaafdfb`  
Decision: **PASS**

I read the B14 handoff, remediation-plan acceptance criteria and relevant audit
material, inspected the complete frozen diff except the root-owned plan ledger,
and independently exercised the renderer, transactional auth, delivery,
privacy/migration, provider, HTTP and strict workspace gates. I changed no
application source, plan, migration, generated asset, provider, release,
deployment or production state.

## Review disposition

No material findings remain.

- The shared renderer covers all five message kinds in both locales, produces
  escaped HTML and useful plain text, uses the exact stored expiry when present,
  and makes no duration claim for legacy credential rows. Its action heading,
  CTA, visible fallback URL and credential-free security notices remain useful
  with blocked images, dark preference and narrow layouts. The actual browser
  matrix measures the CTA at at least 44px and verifies nonzero visible heading
  geometry; it does not claim live Gmail/Outlook certification.
- SMTP constructs a multipart alternative and Resend submits explicit text and
  HTML with a stable per-message idempotency key. Provider response bodies and
  diagnostics remain bounded and do not expose credentials or raw hostile
  recipient data.
- Password reset, authenticated password change and confirmed email change put
  the credential/canonical-address mutation, expected token/session
  invalidation, transition audit and durable notice admission in one database
  transaction. Conditional hashes and locked account/session/token rows prevent
  stale waits from committing. Failure-injection and lock-wait tests demonstrate
  rollback, current-session ownership and expiry revalidation rather than only
  checking happy-path final state.
- Once one of those security transitions has durably committed, failure of the
  inline notice dispatch is recorded as `auth.security_notice_dispatch_failed`
  without falsely returning mutation failure or making a spent token reusable.
  The wording correctly allows either a retryable or terminal queue outcome and
  promises no eventual or exactly-once delivery. Existing credential-message
  admission error behavior is unchanged.
- Delivery revalidates credential tokens at the provider boundary. Security
  notices require durable purpose/account metadata, recipient digest and live
  transition-audit authority; password warnings additionally require the
  recipient still be canonical. Email-change warnings deliberately remain
  authorized by the immutable transition evidence for the locked pre-transition
  address. I initially questioned an A-to-B-to-A delayed warning, then retracted
  the concern after checking the explicit contract: the historical A-to-B event
  remains real and suppressing its warning could conceal it. This distinction is
  intentional, not a missing canonical-address check.
- Provider acceptance is serialized with account deletion/anonymization by the
  account lock. Terminal success/failure, retention, anonymization and deletion
  scrub message payloads, recipient digests and transition references. Audit
  metadata stays within the classified allowlist, and the audit foreign key is
  nullable with `ON DELETE SET NULL`, so supported audit retention fails an
  unsent notice closed rather than retaining sensitive queue authority.
- Migration 0027 is forward-only. Independent privacy tests exercise both its
  fresh schema behavior and an isolated pre-0027 upgrade fixture, including
  legacy-row redaction, rather than rewriting terminal history. The documented
  mixed-version restriction and worker-quiescing rollout are honest.
- No direct runtime password mutation bypass was introduced. Dependency
  direction, approval/public-fact behavior, CSP and the known B16 parallel test
  boundary remain intact. The green sequential HTTP suite is not evidence that
  B16's wider parallel fixture work is complete.

The genuinely monitored help-contact link remains owner evidence and was not
invented in this batch. Live provider delivery, real-client rendering and
production rollout remain explicitly external gates.

## Independent commands and results

```text
git diff --stat eaafdfb
git diff --name-status eaafdfb
git status --short
git diff eaafdfb -- <all non-ledger changed paths>
git diff --check eaafdfb -- . ':(exclude)docs/plans/2026-09-08-audit-remediation.md'
```

Result: the complete tracked and untracked implementation/test/handoff
inventory was inspected; diff validation passed.

Renderer validation, performed independently against the final frozen renderer
slice:

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --lib email::templates --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --test email_renderer_visual_test \
  --locked -- --ignored --nocapture
```

Result: renderer unit tests **2 passed, 0 failed**; offline Chromium matrix
**48 passed, 0 failed**. Final independent artifacts were written to
`/tmp/b14-email-visual-xsGT0G`; representative narrow/dark English and
Portuguese expiry captures were inspected directly.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-application --test auth_test --locked
```

Result: application auth **31 passed, 0 failed**.

All database-backed commands below removed `DATABASE_URL`, used only
`TEST_DATABASE_URL=postgres://bikesnest_test:bikesnest_test@127.0.0.1:55439/bikesnest_test_audit`,
used `CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target`, and ran with
`--locked -- --test-threads=1`:

```text
cargo test -p bikesnest-infrastructure --test auth_test
cargo test -p bikesnest-infrastructure --test job_test
cargo test -p bikesnest-infrastructure --test privacy_test
cargo test -p bikesnest-web --test http_test
```

Result: infrastructure auth **37/37**, job lifecycle **29/29**, privacy and
migration **22/22**, and sequential HTTP **180/180** passed. These include the
owned real-database session-expiry lock race, transaction/audit/outbox rollback,
send-boundary eligibility, permanent/retry/lease behavior, terminal scrubbing,
anonymization/deletion serialization, and fresh/upgrade migration coverage.

```text
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo test -p bikesnest-infrastructure --lib 'email::' --locked
```

Result: provider/rendering/email unit matrix **11 passed, 0 failed** after
allowing only a local loopback listener. The first sandboxed attempt was
**10 passed, 1 failed** solely because binding that listener returned
`Operation not permitted`; the approved loopback rerun passed completely and
made no external provider call.

```text
cargo fmt --all -- --check
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo check --workspace --all-targets --locked
env -u DATABASE_URL CARGO_TARGET_DIR=/home/bruno/Projects/bikenest/target \
  cargo clippy --workspace --all-targets --locked -- -D warnings
git diff --check eaafdfb -- . ':(exclude)docs/plans/2026-09-08-audit-remediation.md'
```

Result: formatting, workspace all-target check, strict workspace Clippy and
final diff validation passed.

B14 therefore satisfies the local renderer and security-notification acceptance
criteria without weakening credential lifecycle, audit, privacy or delivery
boundaries. Production provider calls, publishing, deployment and release asset
generation were not performed.
