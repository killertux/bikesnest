# Listing collaboration implementation plan

## Product contract

- The published listing remains authoritative while proposals are pending.
- Verified active accounts may propose changes and vote; one mutable vote per
  account/proposal, no self-voting, and no voting after a decision.
- Six distinct approvals accept non-photo changes (not net votes). Rejections
  are shown separately and never automatically reject a proposal.
- Moderators may approve/reject immediately. All decisions are recorded.
- Photo additions and removals always require moderation. Mixed submissions
  separate listing changes from photo decisions; existing processing, upload
  limits and non-public pending-photo protections remain intact.
- Conflicting/stale proposals cannot overwrite a newer published version.
- Public views expose counts, never voter identities. Anonymous attribution is
  the default. A public-name account setting applies to new reviews/proposals,
  never retrospectively reveals anonymous content, and disabling it suppresses
  attribution on existing contributions. No stable anonymous aliases.

## Work packages

1. Extend existing proposal/revision concepts and application ports to cover
   all supported editable listing fields, typed validation, votes, decisions,
   history and photo removal requests. Preserve inward dependencies.
2. Add forward-only migrations and SQLx adapters. Vote updates, eligibility,
   threshold decisions, optimistic version checks, listing publication and
   revision writes must be transactionally consistent and race-safe.
3. Integrate existing moderator queues/actions and safe photo pipeline. Close
   legacy edit routes that could bypass proposal review for existing listings.
4. Implement server-backed details-page tabs, pending cues, accessible inline
   editor, comparisons, votes, alternative proposals and paginated history.
   Preserve no-JS/full-page fallbacks and HTMX fragment redirect contracts.
5. Replace handwritten prototype CSS with Tailwind utilities using existing
   tokens and build pipeline. Remove mock data and simulation controls.
6. Implement optional public-name settings and privacy-safe read models;
   extend export/deletion handling and policy/inventory documentation. Never
   expose pending/hidden photos through proposal or revision snapshots.
7. Verify domain/application rules, real-DB transactions and concurrency,
   HTTP authentication/authorization/CSRF, privacy, stale proposals, moderation,
   uploads, bilingual copy and responsive keyboard-accessible browser flows.

## Acceptance checks

- Refresh and a second browser session see persisted proposals and totals.
- Five approvals remain pending; the sixth publishes exactly once.
- Duplicate/concurrent votes cannot inflate totals or duplicate revisions.
- A stale proposal is explicitly blocked/superseded, not silently applied.
- Photos cannot auto-publish, including mixed submissions.
- Ordinary accounts cannot invoke moderator decisions or bypass proposal flow.
- History shows real previous versions and decisions without voter identifiers,
  withdrawn public attribution, or photos hidden/removed for safety.
- Account deletion/export includes new personal data; anonymous content is not
  relinked when public attribution is enabled later.
- All UI text is in both catalogs; generated CSS matches the Tailwind build.

## Execution

GPT-5.6 Terra owns the main implementation and accompanying automated tests.
The primary agent independently reviews privacy/security edge cases, integration
and browser behavior. Preserve existing work and unrelated `.DS_Store` changes.
Do not reset/reseed the development database, deploy, or commit without request.
