-- New accounts share their display name on new reviews and change proposals.
-- Preserve every existing account preference and historical attribution flag.
ALTER TABLE users ALTER COLUMN public_contribution_name SET DEFAULT TRUE;
