-- Certificate authority that issues agent client certificates. One row.
CREATE TABLE agent_ca (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    cert_pem TEXT NOT NULL,
    -- Private key sealed with the master key.
    key_sealed BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE servers
    ADD COLUMN hostname TEXT,
    ADD COLUMN enrolled_at TIMESTAMPTZ,
    ADD COLUMN telemetry_dropped BIGINT NOT NULL DEFAULT 0;

CREATE UNIQUE INDEX servers_client_cert_fingerprint_key
    ON servers (client_cert_fingerprint) WHERE client_cert_fingerprint IS NOT NULL;
