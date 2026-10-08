-- Account-linked transactional mail.  Existing email.send rows predate the
-- linkage and cannot be proven safe to deliver, so the upgrade fails them
-- closed and removes their recipient/link payload immediately.
ALTER TABLE background_job
    ADD COLUMN mail_account_id BIGINT REFERENCES users(id) ON DELETE SET NULL,
    ADD COLUMN mail_token_hash TEXT,
    ADD COLUMN mail_token_expires_at TIMESTAMPTZ,
    ADD COLUMN mail_purpose TEXT
        CHECK (mail_purpose IN ('verify', 'reset', 'change')),
    ADD COLUMN payload_redacted_at TIMESTAMPTZ;

UPDATE background_job
SET payload = '{}'::jsonb,
    state = CASE WHEN state IN ('pending', 'running') THEN 'failed' ELSE state END,
    last_error = CASE
        WHEN state IN ('pending', 'running', 'failed') THEN 'legacy mail payload rejected'
        ELSE NULL
    END,
    finished_at = CASE
        WHEN state IN ('pending', 'running') THEN COALESCE(finished_at, now())
        ELSE finished_at
    END,
    claimed_by = NULL,
    lease_expires_at = NULL,
    heartbeat_at = NULL,
    payload_redacted_at = now(),
    updated_at = now()
WHERE kind = 'email.send';

CREATE INDEX background_job_mail_account_idx
    ON background_job (mail_account_id)
    WHERE kind = 'email.send';
