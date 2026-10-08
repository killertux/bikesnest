-- Foreign-key lookup indexes for referencing columns that had none (or had
-- only a partial index whose predicate the referential action can't prove).
--
-- Without them, every `ON DELETE CASCADE` / `ON DELETE SET NULL` fired by an
-- account purge, a photo deletion or an audit-event purge does a sequential
-- scan of the referencing table.
--
-- Locking: this migration runs inside sqlx's migration transaction, so each
-- `CREATE INDEX` holds a SHARE lock on its table for the duration of the build
-- (reads continue; inserts/updates/deletes wait). All four tables are small
-- (proposal votes and removal requests are community-scale) or bounded by the
-- job GC sweep (`background_job`), so the builds take well under a second at
-- current sizes; while the build runs, job enqueue/claim on `background_job`
-- waits. If `background_job` has grown large (millions of retained rows),
-- deploy in a quiet window; see also the note in 0017.

-- Account purge cascades into votes by `voter_id`; the primary key
-- `(proposal_id, voter_id)` leads with the proposal, so it can't serve this.
CREATE INDEX parking_proposal_vote_voter_idx
    ON parking_proposal_vote (voter_id);

-- Photo deletion cascades by `photo_id`; account purge sets `requester_id`
-- and `resolved_by` to NULL. Both user columns are often NULL, so they are
-- partial on `IS NOT NULL` — the referential action's `col = $1` implies
-- that predicate, so the planner can still use them.
CREATE INDEX parking_photo_removal_request_photo_idx
    ON parking_photo_removal_request (photo_id);
CREATE INDEX parking_photo_removal_request_requester_idx
    ON parking_photo_removal_request (requester_id)
    WHERE requester_id IS NOT NULL;
CREATE INDEX parking_photo_removal_request_resolved_by_idx
    ON parking_photo_removal_request (resolved_by)
    WHERE resolved_by IS NOT NULL;

-- The mail-linkage indexes were partial on `kind = 'email.send'`. The
-- `ON DELETE SET NULL` actions on `users` and `audit_events` look rows up by
-- the column alone and can't prove that predicate, so they fell back to a
-- sequential scan of the whole job table. Replace them with indexes whose
-- only predicate is `IS NOT NULL`, which the lookup does imply. Every query
-- that used the old indexes (`kind = 'email.send' AND col = $1`) still can.
DROP INDEX background_job_mail_account_idx;
CREATE INDEX background_job_mail_account_idx
    ON background_job (mail_account_id)
    WHERE mail_account_id IS NOT NULL;

DROP INDEX background_job_mail_transition_audit_idx;
CREATE INDEX background_job_mail_transition_audit_idx
    ON background_job (mail_transition_audit_id)
    WHERE mail_transition_audit_id IS NOT NULL;
