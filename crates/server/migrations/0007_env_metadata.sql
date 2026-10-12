-- Environment variables: remember when each value was last written, so a saved
-- value can be told from a stale one without ever showing it.

ALTER TABLE env_vars
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();

UPDATE env_vars SET updated_at = created_at;
