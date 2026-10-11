-- Rudder control-plane schema.
-- Service-kind details live in services.spec (JSONB). Secrets are ciphertext.

CREATE EXTENSION IF NOT EXISTS pgcrypto;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL UNIQUE CHECK (email = lower(email)),
    display_name TEXT NOT NULL DEFAULT '',
    password_hash TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('admin', 'viewer')),
    totp_secret BYTEA,
    totp_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    csrf_token TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    user_agent TEXT,
    ip TEXT
);
CREATE INDEX sessions_user_id_idx ON sessions (user_id);

CREATE TABLE api_tokens (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    scopes TEXT[] NOT NULL DEFAULT ARRAY['read']::TEXT[],
    expires_at TIMESTAMPTZ,
    last_used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX api_tokens_user_id_idx ON api_tokens (user_id);

CREATE TABLE audit_log (
    id BIGSERIAL PRIMARY KEY,
    actor_user_id UUID REFERENCES users (id) ON DELETE SET NULL,
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::JSONB,
    ip TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX audit_log_created_at_idx ON audit_log (created_at DESC);

CREATE TABLE servers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('local', 'remote')),
    status TEXT NOT NULL DEFAULT 'unknown' CHECK (status IN ('online', 'offline', 'unknown', 'pending')),
    public_ip TEXT,
    description TEXT NOT NULL DEFAULT '',
    agent_version TEXT,
    docker_version TEXT,
    os_info TEXT,
    cpu_count INTEGER,
    memory_bytes BIGINT,
    disk_bytes BIGINT,
    last_heartbeat_at TIMESTAMPTZ,
    join_token_hash TEXT,
    join_token_expires_at TIMESTAMPTZ,
    client_cert_pem TEXT,
    client_cert_fingerprint TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE projects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE environments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    project_id UUID NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (project_id, name)
);

CREATE TABLE services (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    environment_id UUID NOT NULL REFERENCES environments (id) ON DELETE CASCADE,
    server_id UUID NOT NULL REFERENCES servers (id) ON DELETE RESTRICT,
    name TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('application', 'database', 'compose')),
    status TEXT NOT NULL DEFAULT 'created',
    source_type TEXT,
    spec JSONB NOT NULL DEFAULT '{}'::JSONB,
    webhook_secret_enc BYTEA,
    current_container_id TEXT,
    current_image TEXT,
    current_digest TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (environment_id, name)
);
CREATE INDEX services_server_id_idx ON services (server_id);

CREATE TABLE env_vars (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id UUID NOT NULL REFERENCES services (id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value_enc BYTEA NOT NULL,
    is_build_secret BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (service_id, key, is_build_secret)
);

CREATE TABLE certificates (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    cert_pem_enc BYTEA NOT NULL,
    key_pem_enc BYTEA NOT NULL,
    chain_pem_enc BYTEA,
    common_name TEXT,
    sans TEXT[] NOT NULL DEFAULT '{}',
    issuer TEXT,
    not_before TIMESTAMPTZ,
    not_after TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE cache_policies (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    memory_max_bytes BIGINT,
    disk_max_bytes BIGINT,
    max_object_bytes BIGINT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE cache_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    policy_id UUID NOT NULL REFERENCES cache_policies (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    match_spec JSONB NOT NULL,
    action_spec JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (policy_id, position)
);

CREATE TABLE domains (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id UUID NOT NULL REFERENCES services (id) ON DELETE CASCADE,
    host TEXT NOT NULL,
    path_prefix TEXT NOT NULL DEFAULT '/',
    target_port INTEGER NOT NULL,
    https_redirect BOOLEAN NOT NULL DEFAULT TRUE,
    ssl_mode TEXT NOT NULL CHECK (ssl_mode IN ('letsencrypt', 'custom', 'none')),
    certificate_id UUID REFERENCES certificates (id) ON DELETE SET NULL,
    basic_auth_user TEXT,
    basic_auth_hash TEXT,
    cache_enabled BOOLEAN NOT NULL DEFAULT FALSE,
    cache_policy_id UUID REFERENCES cache_policies (id) ON DELETE SET NULL,
    is_preview BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (host, path_prefix)
);
CREATE INDEX domains_service_id_idx ON domains (service_id);
CREATE INDEX domains_host_idx ON domains (host);

CREATE TABLE deployments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id UUID NOT NULL REFERENCES services (id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    trigger TEXT NOT NULL DEFAULT 'manual',
    image_ref TEXT,
    git_sha TEXT,
    logs TEXT NOT NULL DEFAULT '',
    error TEXT,
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX deployments_service_idx ON deployments (service_id, created_at DESC);

CREATE TABLE service_containers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id UUID NOT NULL REFERENCES services (id) ON DELETE CASCADE,
    deployment_id UUID REFERENCES deployments (id) ON DELETE SET NULL,
    container_id TEXT NOT NULL,
    container_name TEXT NOT NULL,
    image_ref TEXT,
    generation INTEGER NOT NULL,
    state TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX service_containers_service_idx ON service_containers (service_id, generation DESC);

CREATE TABLE registries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    url TEXT NOT NULL,
    username TEXT,
    password_enc BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE git_providers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('github_app', 'github_pat', 'gitlab_pat', 'generic_ssh')),
    config_enc BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE deploy_keys (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id UUID REFERENCES services (id) ON DELETE CASCADE,
    public_key TEXT NOT NULL,
    private_key_enc BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE s3_destinations (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    endpoint TEXT,
    region TEXT NOT NULL DEFAULT 'us-east-1',
    bucket TEXT NOT NULL,
    access_key_id_enc BYTEA NOT NULL,
    secret_access_key_enc BYTEA NOT NULL,
    path_style BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE backup_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    server_id UUID REFERENCES servers (id) ON DELETE CASCADE,
    service_id UUID REFERENCES services (id) ON DELETE CASCADE,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('database', 'volume', 'bind', 'path', 'panel_db', 'panel_settings')),
    target_spec JSONB NOT NULL DEFAULT '{}'::JSONB,
    schedule_cron TEXT,
    timezone TEXT NOT NULL DEFAULT 'UTC',
    destination_ids UUID[] NOT NULL DEFAULT '{}',
    path_prefix TEXT NOT NULL DEFAULT 'rudder',
    filename_template TEXT NOT NULL DEFAULT '{name}-{timestamp}',
    retention_spec JSONB NOT NULL DEFAULT '{}'::JSONB,
    compression TEXT NOT NULL DEFAULT 'zstd' CHECK (compression IN ('none', 'gzip', 'zstd')),
    compression_level INTEGER NOT NULL DEFAULT 3,
    encryption_passphrase_enc BYTEA,
    tool_options JSONB NOT NULL DEFAULT '{}'::JSONB,
    consistency_spec JSONB NOT NULL DEFAULT '{}'::JSONB,
    timeout_secs INTEGER NOT NULL DEFAULT 3600,
    retries INTEGER NOT NULL DEFAULT 2,
    bandwidth_limit_bps BIGINT,
    verify_after_upload BOOLEAN NOT NULL DEFAULT FALSE,
    notify_on_success BOOLEAN NOT NULL DEFAULT FALSE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE backup_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    job_id UUID NOT NULL REFERENCES backup_jobs (id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    size_bytes BIGINT,
    checksum TEXT,
    object_keys TEXT[] NOT NULL DEFAULT '{}',
    logs TEXT NOT NULL DEFAULT '',
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX backup_runs_job_idx ON backup_runs (job_id, created_at DESC);

CREATE TABLE notification_channels (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('telegram', 'discord', 'slack', 'webhook', 'smtp')),
    config_enc BYTEA NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE alerts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    target_spec JSONB NOT NULL DEFAULT '{}'::JSONB,
    threshold DOUBLE PRECISION,
    duration_secs INTEGER NOT NULL DEFAULT 0,
    channel_ids UUID[] NOT NULL DEFAULT '{}',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    last_fired_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE metrics_samples (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY,
    sampled_at TIMESTAMPTZ NOT NULL,
    server_id UUID,
    service_id UUID,
    domain_id UUID,
    source TEXT NOT NULL,
    metric TEXT NOT NULL,
    value DOUBLE PRECISION NOT NULL,
    labels JSONB NOT NULL DEFAULT '{}'::JSONB,
    PRIMARY KEY (id, sampled_at)
) PARTITION BY RANGE (sampled_at);

CREATE TABLE metrics_samples_default PARTITION OF metrics_samples DEFAULT;

CREATE TABLE metrics_rollups (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY,
    bucket_start TIMESTAMPTZ NOT NULL,
    resolution TEXT NOT NULL CHECK (resolution IN ('1m', '5m', '1h')),
    server_id UUID,
    service_id UUID,
    domain_id UUID,
    source TEXT NOT NULL,
    metric TEXT NOT NULL,
    avg_value DOUBLE PRECISION NOT NULL,
    min_value DOUBLE PRECISION NOT NULL,
    max_value DOUBLE PRECISION NOT NULL,
    sample_count INTEGER NOT NULL,
    PRIMARY KEY (id, bucket_start)
) PARTITION BY RANGE (bucket_start);

CREATE TABLE metrics_rollups_default PARTITION OF metrics_rollups DEFAULT;

CREATE INDEX metrics_samples_lookup ON metrics_samples (source, metric, sampled_at DESC);
CREATE INDEX metrics_rollups_lookup ON metrics_rollups (resolution, source, metric, bucket_start DESC);

CREATE TABLE log_entries (
    id BIGINT GENERATED BY DEFAULT AS IDENTITY,
    logged_at TIMESTAMPTZ NOT NULL,
    server_id UUID,
    project_id UUID,
    service_id UUID,
    source TEXT NOT NULL,
    level TEXT NOT NULL DEFAULT 'info',
    message TEXT NOT NULL,
    http_status INTEGER,
    fields JSONB NOT NULL DEFAULT '{}'::JSONB,
    PRIMARY KEY (id, logged_at)
) PARTITION BY RANGE (logged_at);

CREATE TABLE log_entries_default PARTITION OF log_entries DEFAULT;

CREATE INDEX log_entries_time_idx ON log_entries (logged_at DESC);
CREATE INDEX log_entries_source_idx ON log_entries (source, logged_at DESC);
CREATE INDEX log_entries_service_idx ON log_entries (service_id, logged_at DESC);
CREATE INDEX log_entries_level_idx ON log_entries (level, logged_at DESC);
CREATE INDEX log_entries_message_trgm ON log_entries USING gin (message gin_trgm_ops);

CREATE TABLE retention_settings (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    log_days JSONB NOT NULL DEFAULT '{"app":14,"panel":14,"proxy":14,"cache":14,"agent":14,"system":14}'::JSONB,
    metrics JSONB NOT NULL DEFAULT '{"raw_hours":24,"m1_days":7,"m5_days":14,"h1_days":30}'::JSONB,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO retention_settings (id) VALUES (TRUE);

CREATE TABLE update_history (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    component TEXT NOT NULL,
    from_version TEXT,
    to_version TEXT NOT NULL,
    status TEXT NOT NULL,
    logs TEXT NOT NULL DEFAULT '',
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);

CREATE TABLE panel_settings (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    panel_domain TEXT,
    panel_ssl_mode TEXT NOT NULL DEFAULT 'none' CHECK (panel_ssl_mode IN ('letsencrypt', 'custom', 'none')),
    panel_certificate_id UUID REFERENCES certificates (id) ON DELETE SET NULL,
    acme_email TEXT,
    github_repo TEXT NOT NULL DEFAULT 'vantanminh/lunchdock',
    update_channel TEXT NOT NULL DEFAULT 'stable' CHECK (update_channel IN ('stable', 'prerelease')),
    auto_update BOOLEAN NOT NULL DEFAULT FALSE,
    maintenance_window TEXT,
    build_concurrency_default INTEGER NOT NULL DEFAULT 1,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

INSERT INTO panel_settings (id) VALUES (TRUE);

CREATE TABLE internal_ca (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    cert_pem TEXT NOT NULL,
    key_pem_enc BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE build_queue (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    deployment_id UUID NOT NULL REFERENCES deployments (id) ON DELETE CASCADE,
    server_id UUID NOT NULL REFERENCES servers (id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued',
    cancel_requested BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    finished_at TIMESTAMPTZ
);
CREATE INDEX build_queue_server_status_idx ON build_queue (server_id, status, created_at);

CREATE TABLE webhook_deliveries (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id UUID REFERENCES services (id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    payload_sha TEXT,
    accepted BOOLEAN NOT NULL,
    detail TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE cleanup_schedules (
    server_id UUID PRIMARY KEY REFERENCES servers (id) ON DELETE CASCADE,
    cron TEXT,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    last_run_at TIMESTAMPTZ
);

CREATE OR REPLACE FUNCTION rudder_ensure_daily_partition(parent text, day date)
RETURNS void
LANGUAGE plpgsql
AS $$
DECLARE
    part_name text := parent || '_' || to_char(day, 'YYYYMMDD');
    start_ts timestamptz := day::timestamptz;
    end_ts timestamptz := (day + 1)::timestamptz;
BEGIN
    IF to_regclass(part_name) IS NOT NULL THEN
        RETURN;
    END IF;
    BEGIN
        EXECUTE format(
            'CREATE TABLE %I PARTITION OF %I FOR VALUES FROM (%L) TO (%L)',
            part_name,
            parent,
            start_ts,
            end_ts
        );
    EXCEPTION
        WHEN duplicate_table THEN
            NULL;
    END;
END;
$$;

SELECT rudder_ensure_daily_partition('log_entries', CURRENT_DATE);
SELECT rudder_ensure_daily_partition('log_entries', CURRENT_DATE + 1);
SELECT rudder_ensure_daily_partition('metrics_samples', CURRENT_DATE);
SELECT rudder_ensure_daily_partition('metrics_samples', CURRENT_DATE + 1);
SELECT rudder_ensure_daily_partition('metrics_rollups', CURRENT_DATE);
SELECT rudder_ensure_daily_partition('metrics_rollups', CURRENT_DATE + 1);
