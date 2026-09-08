# Parking approval flow

All user-submitted changes to existing parking facts create pending proposals.
The public page continues to show the published version until approval.

| Change | Approval path | Published effect |
| --- | --- | --- |
| Name, address, description, type, cost, hours, security | Six distinct eligible community approvals or a moderator | One atomic edit and saved version |
| Coordinates and timezone | Same proposal approval path | One atomic move and saved version |
| Existence/removal | Same proposal approval path | Listing state and saved version; removed listings leave public results |
| Parking/review photos | Existing moderator-only photo workflow | Only approved media becomes public |
| Reviews and verification | Community signals, not listing edits | Ratings, confidence and verification timestamps; no replacement of listing facts |

## Resolution safeguards

- Proposers cannot approve their own submissions, including as moderators.
- Community voters must be active and email-verified. Repeated votes replace
  the same voter's position; they never add another vote. Current eligibility
  is rechecked when tallying. Rejections are counted separately, as described
  in the existing community policy.
- Moderator approval and threshold publication share one transaction and lock
  order: location, proposal, then competing proposals. Publication checks the
  proposal's base version, updates all affected fields, records one immutable
  revision, and supersedes competing pending proposals.
- Rejected, resolved and stale proposals cannot subsequently publish.
- Malformed legacy payloads require manual review; incomplete detail payloads
  cannot be approved as partial edits.

## Profile

Current version, Version history and Pending approvals are real server-rendered
navigation links, including without JavaScript. Current values carry links to
relevant pending diffs, including proposed security features absent from older
listings. Historical views render saved public-field snapshots, not today's
values. Pending photos expose a count but not media or uploader identity.

## Validation

Regression coverage lives in `parking_approval_test.rs`,
`parking_profile_test.rs`, the two edit HTTP tests, and the domain/application
tests. It checks publication thresholds, duplicate/switched/ineligible votes,
self-approval, rejection, stale versions, supersession, all three proposal
kinds, malformed edits, form submission without publication, bilingual diffs,
saved history, escaping, and private-field exclusion.

The new and migrated HTTP/database cases use `tx.db()` and `db.acquire()` with
automatic outer rollback. They do not commit or delete fixture data. The shared
connection tests do not establish true multi-connection race coverage.

Desktop (1365px) and mobile (390px) fixture previews check all tabs, pending
links, translations and horizontal overflow. These checks do not constitute a
production deployment or an exhaustive rerun of legacy photo/moderation tests.

Deployment requires the normal application restart, which applies the new
forward-only proposal-kind migration automatically.
