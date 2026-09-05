# Listing collaboration prototype

Frontend concept for parking detail pages. No proposal, vote, file, public-name
preference, or moderation decision is sent to the server or persisted. Reloading
resets the examples. The real published listing stays unchanged even when a
proposal is accepted in the preview; the resulting mock version is in History.
Existing edit/upload controls are hidden while this prototype is initialized.
Other features (navigation, reviews, verification) retain their real behavior.

## Interactions

- Overview shows published data and a pending-proposal banner.
- Suggest changes opens a native dialog covering title, description, type,
  price, address, coordinates, each day's hours, tri-state security, and other
  details such as capacity or access restrictions. Review the diff before posting.
- Photo files remain local object URLs. Existing photos can be proposed for
  removal without disappearing. Mixed edits become separate detail/photo proposals.
- A viewer can approve, reject, switch or withdraw their single mock vote, or
  suggest an alternative. They cannot vote on their own proposal.
- Six approvals (the requested “more than five,” not five net votes) accept a
  non-photo proposal. Rejections are separate; there is no auto-rejection rule.
  The simulation control adds another person's approval.
- Photos never auto-accept. Moderator simulation exposes immediate decisions.
  This is not an authorization implementation.
- History includes decisions, vote counts, comparisons and mock snapshots with
  photos. No voter identities are displayed.
- Authors default to “Anonymous cyclist.” A per-proposal checkbox demonstrates
  a future public-name preference using an explicitly fictional name.

## Privacy assessment and implementation boundary

The existing data-processing inventory and privacy policies keep contributions
unattributed, display names private, and verification activity aggregated.
The prototype does not change those policies or expose real account data.

Public vote totals and anonymous attribution fit that direction. A public-name
setting for reviews/proposals is a new optional disclosure: default it off,
explain the audience, and record the choice. Do not retroactively reveal old
anonymous contributions. Withdrawal should remove public attribution from live
and historical views; justified restricted audit records need separate rules.
This is a design recommendation, not a determination of legal compliance.

[GDPR articles 4–6, 13, 17 and 25](https://eur-lex.europa.eu/eli/reg/2016/679/oj)
require a lawful basis, transparency, minimization, rights handling and privacy
by design. Linkable pseudonyms remain personal data.
[LGPD articles 6–8, 12 and 18](https://www.planalto.gov.br/ccivil_03/_ato2015-2018/2018/lei/l13709compilado.htm)
similarly require necessity, an appropriate basis and data-subject rights.
If consent is used for public attribution, it must be specific, optional,
informed and withdrawable. Confirm the basis and notices before publishing real
identities. Merely adding a checkbox is not sufficient.

A playful anonymous label should be contribution-specific, not a stable public
user identifier. Do not expose voter lists, emails, internal IDs, IP addresses,
or linked visit histories. Text and photos can themselves contain personal data:
unlinking an author does not automatically anonymize content. Historical views
need redaction/removal handling; sensitive photos must not reappear in History.

Before backend work, define voter eligibility/anti-abuse checks, conflicting or
stale proposals, withdrawal after decisions, moderator reasons, photo screening,
retention/erasure/export and public-name settings. None is implemented by this mock.

## Files

- `templates/partials/listing_collaboration.html`: views and editor.
- `web/static/js/listing-prototype.js`: page-local state and interactions.
- `web/static/css/listing-prototype.css`: scoped layout.
- `crates/i18n/src/lib.rs`: English and Portuguese copy.
