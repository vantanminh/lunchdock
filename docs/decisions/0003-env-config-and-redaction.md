---
created_at: "2026-10-11T02:01:24.342976807+00:00"
doc: docs/decisions/0003-env-config-and-redaction.md
id: "0003"
notes: null
status: accepted
title: Environment configuration and redacted secrets
type: decision
updated_at: "2026-10-11T02:01:24.342978747+00:00"
verify: null
---

# Environment configuration and redacted secrets

Date: 2026-10-11

## Status

Accepted

## Context

The panel needs a database URL, a 32-byte AES-256-GCM master key, a bind address, and a data directory. Those values differ between a laptop, CI, and `install.sh`. The master key and database password must never show up in logs or `Debug` output.

## Decision

Load configuration from the environment (`DATABASE_URL`, `RUDDER_MASTER_KEY` or `RUDDER_MASTER_KEY_FILE`, `RUDDER_BIND`, `RUDDER_DATA_DIR`, `RUDDER_GITHUB_REPO`, `RUDDER_LOG_FORMAT`, `RUDDER_COOKIE_SECURE`). The master key is 32 raw bytes, 64 hex characters, or standard/URL-safe base64. Hex is recognized only when the string is exactly 64 hex digits. `Config`'s `Debug` impl redacts the key and the database userinfo. The default GitHub repository for update checks is `vantanminh/lunchdock`, matching this source repository, and it can be overridden.

Structured logs default to JSON so the panel can ingest its own events later. `RUDDER_LOG_FORMAT=pretty` is for a terminal.

## Alternatives Considered

1. A required YAML config file. Worse for containers and systemd, where env and secret files are the normal interface.
2. Generating a throwaway master key when none is configured. That would silently make ciphertext unrestorable after a restart.

## Consequences

Positive:

- `install.sh` can generate a key file and pass the path.
- Tests construct a config from a map without mutating process environment.

Tradeoffs:

- Operators must supply the key out of band. The process refuses to start without one.

## Follow-Up

- Encrypt env vars, registry passwords, and certificate keys with this master key.
