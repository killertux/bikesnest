-- A proposal can collect enough community approvals and still fail to
-- publish automatically, because its stored payload no longer merges into a
-- valid change. Instead of retrying on every later vote, the proposal is
-- marked as needing a moderator decision. It stays PENDING (so it remains in
-- the moderation queue) until a moderator approves it with corrected values
-- or rejects it.
ALTER TABLE parking_proposal ADD COLUMN escalated_at TIMESTAMPTZ;
