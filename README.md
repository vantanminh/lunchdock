# Rudder

Rudder is a lightweight, self-hosted deployment platform. One VPS runs the control panel. Extra servers run a small agent and are managed from the same dashboard. There is no cluster scheduler: each server is independent.

The panel deploys applications from Git or container images (including private GitHub Container Registry), one-click databases, and Docker Compose stacks. It terminates TLS with Traefik, optionally caches HTTP with a Pingora-based cache server, and stores backups on S3-compatible storage.

## Workspace

| Path | Role |
| --- | --- |
| `crates/core` | Domain types, validation, shared errors |
| `crates/proto` | Agent session protobuf |
| `crates/server` | Control plane (Axum, PostgreSQL) |
| `crates/agent` | Node agent |
| `crates/cache` | HTTP cache server |
| `crates/cli` | `rudder` command line client |
| `web` | React panel |

## Develop

PostgreSQL 16 and a 32-byte master key are required. Copy `.env.example` and export the variables, then:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm --dir web install
pnpm --dir web build
```

Compile-time checked SQL uses `sqlx`. CI sets `SQLX_OFFLINE=true` and reads the committed `.sqlx` cache. After a query change, apply migrations and refresh the cache:

```bash
DATABASE_URL=postgres://rudder:rudder@127.0.0.1:5432/rudder cargo sqlx prepare --workspace
```

Measured idle memory is recorded once the release binaries exist.
