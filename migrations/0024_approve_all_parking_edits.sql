-- Every edit to published listing information enters the approval workflow.
ALTER TABLE parking_proposal DROP CONSTRAINT parking_proposal_kind_check;
ALTER TABLE parking_proposal ADD CONSTRAINT parking_proposal_kind_check
    CHECK (kind IN ('move_location', 'change_existence', 'edit_details'));
