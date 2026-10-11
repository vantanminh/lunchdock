-- Backup execution details and the system event outbox.

ALTER TABLE servers
    ADD COLUMN backup_concurrency INTEGER NOT NULL DEFAULT 2 CHECK (backup_concurrency BETWEEN 1 AND 16);

ALTER TABLE backup_runs
    ADD COLUMN kind TEXT NOT NULL DEFAULT 'backup' CHECK (kind IN ('backup', 'restore')),
    ADD COLUMN trigger TEXT NOT NULL DEFAULT 'manual',
    ADD COLUMN destination_ids UUID[] NOT NULL DEFAULT '{}',
    ADD COLUMN meta JSONB NOT NULL DEFAULT '{}'::JSONB;

CREATE INDEX backup_runs_status_idx ON backup_runs (status);

-- Deploys, backups, alerts, node changes, and updates. Notification channels
-- deliver rows where delivered_at is NULL; the log explorer also reads them.
CREATE TABLE system_events (
    id BIGSERIAL PRIMARY KEY,
    kind TEXT NOT NULL,
    severity TEXT NOT NULL DEFAULT 'info' CHECK (severity IN ('info', 'warning', 'critical')),
    server_id UUID REFERENCES servers (id) ON DELETE SET NULL,
    service_id UUID REFERENCES services (id) ON DELETE SET NULL,
    title TEXT NOT NULL,
    message TEXT NOT NULL DEFAULT '',
    payload JSONB NOT NULL DEFAULT '{}'::JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    delivered_at TIMESTAMPTZ
);
CREATE INDEX system_events_pending_idx ON system_events (created_at) WHERE delivered_at IS NULL;
CREATE INDEX system_events_created_idx ON system_events (created_at DESC);
