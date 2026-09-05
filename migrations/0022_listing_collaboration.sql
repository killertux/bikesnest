-- Persisted collaboration metadata. Public pages consume aggregates only;
-- individual votes and pending photo assets are never public read models.

ALTER TABLE parking_proposal
    ADD COLUMN public_author BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN public_author_revoked_at TIMESTAMPTZ,
    ADD COLUMN decision_reason TEXT,
    ADD COLUMN decision_approvals BIGINT,
    ADD COLUMN decision_rejections BIGINT;

CREATE TABLE parking_proposal_vote (
    proposal_id BIGINT NOT NULL REFERENCES parking_proposal(id) ON DELETE CASCADE,
    voter_id BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    vote TEXT NOT NULL CHECK (vote IN ('APPROVE', 'REJECT')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (proposal_id, voter_id)
);
CREATE INDEX parking_proposal_vote_proposal_idx
    ON parking_proposal_vote (proposal_id, vote);

-- A removal request names only the approved photo row. It does not snapshot a
-- storage key or a presigned URL, so history cannot revive hidden media.
CREATE TABLE parking_photo_removal_request (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    photo_id BIGINT NOT NULL REFERENCES parking_photo(id) ON DELETE CASCADE,
    requester_id BIGINT REFERENCES users(id) ON DELETE SET NULL,
    reason TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING', 'APPROVED', 'REJECTED', 'SUPERSEDED')),
    resolved_by BIGINT REFERENCES users(id) ON DELETE SET NULL,
    resolved_at TIMESTAMPTZ,
    decision_reason TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX parking_photo_removal_request_pending_idx
    ON parking_photo_removal_request (status, created_at);
