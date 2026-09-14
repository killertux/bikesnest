-- Security notices reuse the account-linked mail lifecycle. The recipient
-- digest and transition reference are admission evidence, not long-term
-- history; terminalization and account deletion clear both.
ALTER TABLE background_job
    DROP CONSTRAINT background_job_mail_purpose_check,
    ADD CONSTRAINT background_job_mail_purpose_check CHECK (
        mail_purpose IN (
            'verify', 'reset', 'change', 'password_changed', 'email_changed'
        )
    ),
    ADD COLUMN mail_recipient_hash TEXT,
    ADD COLUMN mail_transition_audit_id BIGINT
        REFERENCES audit_events(id) ON DELETE SET NULL;

CREATE INDEX background_job_mail_transition_audit_idx
    ON background_job (mail_transition_audit_id)
    WHERE kind = 'email.send' AND mail_transition_audit_id IS NOT NULL;
