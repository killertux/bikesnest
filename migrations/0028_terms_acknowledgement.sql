-- Versioned terms acknowledgement is distinct from privacy consent. The
-- feature is dormant unless explicitly enabled in application configuration.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM policy_version
        GROUP BY kind, locale, effective_at
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION 'policy_version has duplicate kind/locale/effective_at rows; resolve the publication inventory before migration 0028';
    END IF;
END;
$$;

ALTER TABLE policy_version
    ADD COLUMN requires_acknowledgement BOOLEAN NOT NULL DEFAULT FALSE,
    ADD CONSTRAINT policy_version_ack_terms_only CHECK (
        NOT requires_acknowledgement OR kind = 'terms'
    ),
    ADD CONSTRAINT policy_version_kind_locale_effective_key
        UNIQUE (kind, locale, effective_at);

CREATE TABLE terms_notice_presentation (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id           BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    policy_version_id BIGINT NOT NULL REFERENCES policy_version(id) ON DELETE RESTRICT,
    terms_version     TEXT NOT NULL,
    shown_locale      TEXT NOT NULL CHECK (shown_locale IN ('pt-BR', 'en')),
    presented_at      TIMESTAMPTZ NOT NULL,
    UNIQUE (user_id, terms_version)
);

CREATE TABLE terms_acknowledgement (
    id                BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id           BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    policy_version_id BIGINT NOT NULL REFERENCES policy_version(id) ON DELETE RESTRICT,
    terms_version     TEXT NOT NULL,
    shown_locale      TEXT NOT NULL CHECK (shown_locale IN ('pt-BR', 'en')),
    acknowledged_at   TIMESTAMPTZ NOT NULL,
    source            TEXT NOT NULL CHECK (source IN ('signup', 'in_product')),
    UNIQUE (user_id, terms_version)
);

CREATE INDEX terms_notice_presentation_user_idx
    ON terms_notice_presentation (user_id, presented_at DESC);
CREATE INDEX terms_acknowledgement_user_idx
    ON terms_acknowledgement (user_id, acknowledged_at DESC);

CREATE FUNCTION preserve_policy_version_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.kind IS DISTINCT FROM OLD.kind
       OR NEW.locale IS DISTINCT FROM OLD.locale
       OR NEW.version IS DISTINCT FROM OLD.version
       OR NEW.effective_at IS DISTINCT FROM OLD.effective_at
       OR NEW.content IS DISTINCT FROM OLD.content
       OR NEW.requires_acknowledgement IS DISTINCT FROM OLD.requires_acknowledgement
       OR OLD.superseded_at IS NOT NULL
       OR NEW.superseded_at IS NULL
       OR NEW.superseded_at <= OLD.effective_at THEN
        RAISE EXCEPTION 'policy versions are immutable; only initial supersession may be scheduled';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER policy_version_preserve_identity
BEFORE UPDATE ON policy_version
FOR EACH ROW EXECUTE FUNCTION preserve_policy_version_identity();

CREATE FUNCTION reject_policy_version_delete() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'policy versions cannot be deleted';
END;
$$;

CREATE TRIGGER policy_version_no_delete
BEFORE DELETE ON policy_version
FOR EACH ROW EXECUTE FUNCTION reject_policy_version_delete();

CREATE FUNCTION reject_terms_proof_update() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'terms proof rows are immutable';
END;
$$;

CREATE TRIGGER terms_notice_presentation_no_update
BEFORE UPDATE ON terms_notice_presentation
FOR EACH ROW EXECUTE FUNCTION reject_terms_proof_update();

CREATE TRIGGER terms_acknowledgement_no_update
BEFORE UPDATE ON terms_acknowledgement
FOR EACH ROW EXECUTE FUNCTION reject_terms_proof_update();
