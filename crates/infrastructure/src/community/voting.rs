//! The one definition of whose proposal votes count.
//!
//! The tally a listing shows, the tally that publishes a proposal, the tally a
//! moderator decision records, and the check made when a vote is cast must all
//! agree, so every one of them splices in this predicate instead of spelling
//! it out.

/// SQL predicate over a `users` row aliased `u`: an active account with a
/// verified email address. Expands to a string literal so it can be used in
/// `concat!`.
macro_rules! eligible_voter {
    () => {
        "u.account_state = 'ACTIVE' AND u.email_verified_at IS NOT NULL"
    };
}
pub(crate) use eligible_voter;

/// SQL `SET` items that record a proposal's eligible approve/reject tally on
/// `parking_proposal` when it is decided. Correlates on the row being
/// updated, so it is only valid inside `UPDATE parking_proposal SET …`.
pub(crate) const DECISION_TALLY_SET: &str = concat!(
    "decision_approvals = (SELECT COUNT(*) FROM parking_proposal_vote v JOIN users u ON u.id = v.voter_id \
     WHERE v.proposal_id = parking_proposal.id AND v.vote = 'APPROVE' AND ",
    eligible_voter!(),
    "), decision_rejections = (SELECT COUNT(*) FROM parking_proposal_vote v JOIN users u ON u.id = v.voter_id \
     WHERE v.proposal_id = parking_proposal.id AND v.vote = 'REJECT' AND ",
    eligible_voter!(),
    ")"
);

/// `LEFT JOIN` clauses that attach each proposal's votes and, for eligible
/// voters only, the voter row aliased `u`. Count with
/// `COUNT(*) FILTER (WHERE v.vote = '…' AND u.id IS NOT NULL)`. Expects the
/// proposal aliased `p`.
macro_rules! eligible_votes_join {
    () => {
        concat!(
            " LEFT JOIN parking_proposal_vote v ON v.proposal_id = p.id \
              LEFT JOIN users u ON u.id = v.voter_id AND ",
            $crate::community::voting::eligible_voter!(),
            " "
        )
    };
}
pub(crate) use eligible_votes_join;
