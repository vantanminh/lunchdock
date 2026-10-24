-- AI agents sign in with OAuth 2.1 (authorization code + PKCE) and refresh tokens.
-- Access tokens are short-lived. Refresh tokens rotate. Reuse revokes the grant.

CREATE TABLE oauth_clients (
    client_id TEXT PRIMARY KEY,
    client_name TEXT NOT NULL,
    redirect_uris TEXT[] NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE mcp_grants (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    client_id TEXT NOT NULL REFERENCES oauth_clients (client_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    template TEXT NOT NULL,
    scopes TEXT[] NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX mcp_grants_user_idx ON mcp_grants (user_id, created_at DESC);

CREATE TABLE oauth_codes (
    code_hash TEXT PRIMARY KEY,
    grant_id UUID NOT NULL REFERENCES mcp_grants (id) ON DELETE CASCADE,
    client_id TEXT NOT NULL,
    redirect_uri TEXT NOT NULL,
    code_challenge TEXT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at TIMESTAMPTZ
);

CREATE TABLE mcp_refresh_tokens (
    token_hash TEXT PRIMARY KEY,
    grant_id UUID NOT NULL REFERENCES mcp_grants (id) ON DELETE CASCADE,
    family_id UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    rotated_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX mcp_refresh_family_idx ON mcp_refresh_tokens (family_id);

CREATE TABLE mcp_access_tokens (
    token_hash TEXT PRIMARY KEY,
    grant_id UUID NOT NULL REFERENCES mcp_grants (id) ON DELETE CASCADE,
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX mcp_access_grant_idx ON mcp_access_tokens (grant_id);
