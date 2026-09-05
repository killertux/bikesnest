-- Existing contributions remain anonymous. Attribution is captured only at
-- creation and is permanently cleared when the account preference is disabled.
ALTER TABLE users
    ADD COLUMN public_contribution_name BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN public_contribution_name_updated_at TIMESTAMPTZ;
ALTER TABLE review ADD COLUMN public_author BOOLEAN NOT NULL DEFAULT FALSE;
