-- Self-update: verified release checks and per-component update history.

ALTER TABLE panel_settings
    ADD COLUMN update_checked_at TIMESTAMPTZ,
    ADD COLUMN update_last_check JSONB;

ALTER TABLE update_history
    ADD COLUMN server_id UUID REFERENCES servers (id) ON DELETE CASCADE,
    ADD COLUMN created_by UUID REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN manifest JSONB,
    ADD CONSTRAINT update_history_component CHECK (component IN ('panel', 'agent', 'cache')),
    ADD CONSTRAINT update_history_status CHECK (status IN ('running', 'succeeded', 'failed', 'rolled_back'));

CREATE INDEX update_history_started ON update_history (started_at DESC);
