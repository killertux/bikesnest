-- A published policy row's database identity is part of exact-link and terms
-- proof authority. Preserve it alongside the content and release metadata.
CREATE OR REPLACE FUNCTION preserve_policy_version_identity() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.id IS DISTINCT FROM OLD.id
       OR NEW.kind IS DISTINCT FROM OLD.kind
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
