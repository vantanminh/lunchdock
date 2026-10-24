-- Rudder Access identities are separate from panel users.
-- Network isolation itself lives in the service spec and needs no column.

ALTER TABLE users
    ADD COLUMN is_owner BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN recovery_hashes TEXT[] NOT NULL DEFAULT '{}';

UPDATE users
SET is_owner = TRUE
WHERE id = (
    SELECT id
    FROM users
    WHERE role = 'admin'
    ORDER BY created_at ASC, id ASC
    LIMIT 1
);

CREATE TABLE access_settings (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    portal_host TEXT,
    turnstile_site_key TEXT NOT NULL DEFAULT '',
    turnstile_secret_enc BYTEA,
    turnstile_hostnames TEXT[] NOT NULL DEFAULT '{}',
    google_client_id TEXT NOT NULL DEFAULT '',
    google_client_secret_enc BYTEA,
    email_provider TEXT NOT NULL DEFAULT 'none'
        CHECK (email_provider IN ('none', 'resend', 'smtp', 'cloudflare')),
    email_from TEXT NOT NULL DEFAULT '',
    email_config_enc BYTEA,
    gate_secret_enc BYTEA,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO access_settings (id) VALUES (TRUE);

CREATE TABLE access_departments (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    parent_id UUID REFERENCES access_departments (id) ON DELETE RESTRICT,
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE access_ranks (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    slug TEXT NOT NULL UNIQUE,
    level INTEGER NOT NULL CHECK (level BETWEEN 0 AND 1000),
    description TEXT NOT NULL DEFAULT '',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE access_identities (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL UNIQUE CHECK (email = lower(email)),
    display_name TEXT NOT NULL DEFAULT '',
    google_sub TEXT UNIQUE,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'blocked')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_login_at TIMESTAMPTZ
);

CREATE TABLE access_memberships (
    identity_id UUID NOT NULL REFERENCES access_identities (id) ON DELETE CASCADE,
    department_id UUID NOT NULL REFERENCES access_departments (id) ON DELETE CASCADE,
    rank_id UUID REFERENCES access_ranks (id) ON DELETE SET NULL,
    PRIMARY KEY (identity_id, department_id)
);

CREATE TABLE access_admins (
    identity_id UUID PRIMARY KEY REFERENCES access_identities (id) ON DELETE CASCADE,
    is_owner BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX access_admins_one_owner ON access_admins (is_owner) WHERE is_owner;

CREATE TABLE access_policies (
    service_id UUID PRIMARY KEY REFERENCES services (id) ON DELETE CASCADE,
    enabled BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE access_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    service_id UUID NOT NULL REFERENCES access_policies (service_id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('email', 'department', 'rank')),
    email TEXT,
    department_id UUID REFERENCES access_departments (id) ON DELETE CASCADE,
    rank_id UUID REFERENCES access_ranks (id) ON DELETE CASCADE,
    CHECK (
        (kind = 'email' AND email IS NOT NULL AND department_id IS NULL AND rank_id IS NULL)
        OR (kind = 'department' AND department_id IS NOT NULL AND email IS NULL AND rank_id IS NULL)
        OR (kind = 'rank' AND rank_id IS NOT NULL AND email IS NULL AND department_id IS NULL)
    )
);
CREATE INDEX access_rules_service_idx ON access_rules (service_id);

CREATE TABLE access_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    identity_id UUID NOT NULL REFERENCES access_identities (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    csrf_token TEXT NOT NULL,
    host TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    user_agent TEXT,
    ip TEXT
);
CREATE INDEX access_sessions_identity_idx ON access_sessions (identity_id);

CREATE TABLE access_codes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email TEXT NOT NULL,
    code_hash TEXT NOT NULL,
    purpose TEXT NOT NULL CHECK (purpose IN ('login', 'handoff')),
    return_host TEXT,
    return_path TEXT,
    expires_at TIMESTAMPTZ NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    used_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX access_codes_email_idx ON access_codes (email, created_at DESC);

CREATE TABLE access_service_tokens (
    service_id UUID PRIMARY KEY REFERENCES services (id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    token_enc BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE rate_limit_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    host TEXT,
    path_prefix TEXT NOT NULL DEFAULT '/',
    methods TEXT[] NOT NULL DEFAULT '{}',
    limit_count INTEGER NOT NULL CHECK (limit_count BETWEEN 1 AND 100000),
    window_secs INTEGER NOT NULL CHECK (window_secs BETWEEN 1 AND 86400),
    burst INTEGER NOT NULL DEFAULT 0 CHECK (burst BETWEEN 0 AND 100000),
    key_kind TEXT NOT NULL CHECK (key_kind IN ('ip', 'header')),
    header_name TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE rate_limit_samples (
    rule_id UUID NOT NULL REFERENCES rate_limit_rules (id) ON DELETE CASCADE,
    bucket TIMESTAMPTZ NOT NULL,
    allowed BIGINT NOT NULL DEFAULT 0,
    blocked BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (rule_id, bucket)
);

CREATE TABLE rate_limit_blocks (
    id BIGSERIAL PRIMARY KEY,
    rule_id UUID NOT NULL REFERENCES rate_limit_rules (id) ON DELETE CASCADE,
    host TEXT NOT NULL,
    path TEXT NOT NULL,
    ip TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX rate_limit_blocks_created_idx ON rate_limit_blocks (created_at DESC);

CREATE TABLE security_scans (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    started_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ,
    status TEXT NOT NULL CHECK (status IN ('running', 'ok', 'failed')),
    error TEXT
);

CREATE TABLE security_findings (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scan_id UUID NOT NULL REFERENCES security_scans (id) ON DELETE CASCADE,
    host TEXT NOT NULL,
    kind TEXT NOT NULL,
    severity TEXT NOT NULL CHECK (severity IN ('info', 'low', 'medium', 'high', 'critical')),
    path TEXT,
    summary TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX security_findings_scan_idx ON security_findings (scan_id);

CREATE TABLE ssl_events (
    id BIGSERIAL PRIMARY KEY,
    host TEXT,
    certificate_id UUID,
    kind TEXT NOT NULL,
    summary TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ssl_events_created_idx ON ssl_events (created_at DESC);

CREATE TABLE edge_counters (
    id BOOLEAN PRIMARY KEY DEFAULT TRUE CHECK (id),
    http_requests BIGINT NOT NULL DEFAULT 0,
    ip_requests BIGINT NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
INSERT INTO edge_counters (id) VALUES (TRUE);
