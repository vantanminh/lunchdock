-- Monitoring: alert state, notification delivery tracking, Docker prune, and rollup watermarks.

ALTER TABLE alerts
    ADD COLUMN state TEXT NOT NULL DEFAULT 'ok' CHECK (state IN ('ok', 'pending', 'firing')),
    ADD COLUMN state_since TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN last_value DOUBLE PRECISION,
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT now();
CREATE UNIQUE INDEX alerts_name_key ON alerts (name);

ALTER TABLE system_events
    ADD COLUMN delivery_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN delivered_channels UUID[] NOT NULL DEFAULT '{}',
    ADD COLUMN delivery_error TEXT;

ALTER TABLE servers
    ADD COLUMN auto_prune BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN last_pruned_at TIMESTAMPTZ;

CREATE TABLE metrics_watermarks (
    resolution TEXT PRIMARY KEY CHECK (resolution IN ('1m', '5m', '1h')),
    done_until TIMESTAMPTZ NOT NULL
);

CREATE INDEX metrics_samples_server_idx
    ON metrics_samples (server_id, metric, sampled_at DESC) WHERE service_id IS NULL;
CREATE INDEX metrics_samples_service_idx
    ON metrics_samples (service_id, metric, sampled_at DESC) WHERE service_id IS NOT NULL;
CREATE INDEX metrics_rollups_server_idx
    ON metrics_rollups (resolution, server_id, metric, bucket_start DESC) WHERE service_id IS NULL;
CREATE INDEX metrics_rollups_service_idx
    ON metrics_rollups (resolution, service_id, metric, bucket_start DESC) WHERE service_id IS NOT NULL;
CREATE INDEX log_entries_proxy_service_idx
    ON log_entries (service_id, logged_at) WHERE source = 'proxy';
