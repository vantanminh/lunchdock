-- How much RAM the cache may use, and a short history of how well it is working.
-- The percent is a share of the host. The optional cap is a hard ceiling in bytes.
-- When the cap is null, the panel still will not publish more than its built-in ceiling.

ALTER TABLE panel_settings
    ADD COLUMN cache_memory_percent SMALLINT NOT NULL DEFAULT 15
        CONSTRAINT panel_settings_cache_memory_percent CHECK (cache_memory_percent BETWEEN 1 AND 40),
    ADD COLUMN cache_memory_cap_bytes BIGINT
        CONSTRAINT panel_settings_cache_memory_cap CHECK (
            cache_memory_cap_bytes IS NULL
            OR (cache_memory_cap_bytes BETWEEN 16777216 AND 4294967296)
        );

CREATE TABLE cache_stat_samples (
    id BIGSERIAL PRIMARY KEY,
    server_id UUID NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    sampled_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    objects BIGINT NOT NULL,
    stored_bytes BIGINT NOT NULL,
    memory_max_bytes BIGINT NOT NULL,
    evictions BIGINT NOT NULL,
    hits BIGINT NOT NULL,
    misses BIGINT NOT NULL,
    stale BIGINT NOT NULL,
    bypass BIGINT NOT NULL,
    bytes_served BIGINT NOT NULL
);
CREATE INDEX cache_stat_samples_server_idx ON cache_stat_samples (server_id, sampled_at DESC);
