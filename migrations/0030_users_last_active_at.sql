-- Durable per-account activity timestamp.
--
-- Inactive-account anonymization used to derive "last activity" from
-- `max(sessions.last_seen_at)`, but the retention job deletes idle and expired
-- sessions earlier in the same run. Once a user's last session was purged the
-- fallback was `users.created_at`, so a long-standing account that was active
-- a month ago looked years idle and was anonymized.
--
-- `last_active_at` survives session purges. It is advanced by a trigger on
-- `sessions` whenever a session is created (password login, registration,
-- OAuth) or its `last_seen_at` is refreshed. The session store already
-- throttles the `last_seen_at` refresh, so the trigger inherits that throttle
-- and adds no write on an ordinary authenticated request.

ALTER TABLE users ADD COLUMN last_active_at TIMESTAMPTZ;

UPDATE users u
SET last_active_at = COALESCE(
        (SELECT max(s.last_seen_at) FROM sessions s WHERE s.user_id = u.id),
        u.created_at
    );

ALTER TABLE users
    ALTER COLUMN last_active_at SET DEFAULT now(),
    ALTER COLUMN last_active_at SET NOT NULL;

-- Only ever moves forward, and only writes when it actually moves, so a
-- session row touched with an older timestamp never rewinds the account.
--
-- SKIP LOCKED keeps a login or session refresh from ever waiting on (or
-- deadlocking with) a transaction that holds the users row, such as a
-- password change that revokes sessions after updating the account. A skipped
-- mark is not lost: the session row still carries `last_seen_at`, the
-- retention job folds purged sessions into `last_active_at` before deleting
-- them, and its inactivity checks also consult live sessions.
CREATE FUNCTION sessions_touch_user_last_active() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE users
       SET last_active_at = NEW.last_seen_at
     WHERE id = (
            SELECT id FROM users
             WHERE id = NEW.user_id
               AND last_active_at < NEW.last_seen_at
             FOR UPDATE SKIP LOCKED
         );
    RETURN NULL;
END;
$$;

CREATE TRIGGER sessions_touch_user_last_active
    AFTER INSERT OR UPDATE OF last_seen_at ON sessions
    FOR EACH ROW EXECUTE FUNCTION sessions_touch_user_last_active();

-- The retention candidate scan filters on it.
CREATE INDEX users_last_active_at_idx ON users (last_active_at);
