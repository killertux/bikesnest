# B11 authentication-state follow-up

Baseline: `ce0aa52`

## Correction

`DetailsPage::build` now derives `can_contribute`, `is_authenticated`, and
`is_moderator` directly from the request's authoritative `Auth`. These values
no longer depend on the optional Current-tab community reader. This restores
shared Suggest, Report/modal, proposal-vote, and moderator pending-photo
controls on Approvals and History, and keeps them present during a Current
reader outage.

Favorite state, own-review state, confidence/dispute data, and recommendation
reasons remain dependent on the Current community read; no data-dependent
value was inferred from authentication and no authorization rule changed.

## Actual-router evidence

`approvals_derive_eligibility_from_auth_without_loading_current_tab_data` owns
a disposable database, creates active verified, unverified, and moderator
sessions, and renders populated detail routes through the production router.
It proves:

- the verified user sees vote controls only for the other user's current
  proposal, never their own or a stale proposal;
- unverified and anonymous users see no vote/Suggest controls;
- the moderator sees the pending-photo moderation link;
- verified Approvals and History retain Suggest and Report/modal controls;
- a Current community-reader outage retains the verified Suggest control;
- Approvals reads `[gallery=0,pending=1,community=0,summary=1,proposals=1,history=0]`;
- History reads `[0,1,0,1,0,1]`; Current outage reads `[1,1,1,1,0,0]`.

## Validation

- Focused authenticated approvals regression: **1 passed**.
- Full sequential HTTP suite: **179 passed**.
- Browser suite: **27 passed**.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy -p bikesnest-web --all-targets --locked -- -D warnings`: passed.
- `git diff --check`: passed.

No migration, server-side eligibility change, production/provider action,
asset build, release, deployment, or commit was performed.
