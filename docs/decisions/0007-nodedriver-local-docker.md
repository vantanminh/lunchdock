---
created_at: "2026-10-11T02:34:34.457898483+00:00"
doc: docs/decisions/0007-nodedriver-local-docker.md
id: "0007"
notes: null
status: accepted
title: NodeDriver trait with a local Docker socket driver
type: decision
updated_at: "2026-10-11T02:34:34.457900487+00:00"
verify: null
---

# NodeDriver trait with a local Docker socket driver

Date: 2026-10-11

## Status

Accepted

## Context

Every server, including the panel host and later agents, must create containers, pull images, run Compose, and write Traefik files. Business logic cannot special-case "this call is local".

## Decision

`NodeDriver` is an `async_trait` in `rudder-core`. `LocalDriver` implements it with bollard against the Unix socket. `docker compose` is the only shell-out, and it receives a prebuilt argv (`compose_args`) with the project name validated and the file path as one argument. File writes are confined to the panel data directory. Environment variable values are encrypted and the API returns keys only. The panel inserts one `local` server row on boot. A remote server row stores a one-hour join token as a SHA-256 hash and returns the plaintext once.

## Alternatives Considered

1. Call bollard directly from each route. Remote servers would then fork every handler.
2. A dyn-compatible hand-written future trait. `async_trait` is the maintained equivalent.

## Consequences

Positive:

- Deploy, backup, and log code can take `&dyn NodeDriver` when the agent lands.

Tradeoffs:

- `async_trait` boxes each call. That cost is noise next to Docker.
- The join command is a placeholder string until the agent installer exists.
