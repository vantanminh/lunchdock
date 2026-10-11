---
created_at: "2026-10-11T02:01:24.267453242+00:00"
doc: docs/decisions/0001-workspace-layout.md
id: "0001"
notes: null
status: accepted
title: Cargo workspace and crate boundaries
type: decision
updated_at: "2026-10-11T02:01:24.267455867+00:00"
verify: null
---

# Cargo workspace and crate boundaries

Date: 2026-10-11

## Status

Accepted

## Context

Rudder is one product with a control plane, a node agent, a cache server, and a CLI. They share validation and the agent wire contract, but they must ship as separate static binaries so a remote server does not run the panel.

## Decision

Use a Cargo workspace with six crates:

- `rudder-core` holds domain errors and validation. It does not open sockets or talk to Docker.
- `rudder-proto` holds the agent gRPC contract.
- `rudder-server` is the panel binary.
- `rudder-agent`, `rudder-cache`, and `rudder-cli` are the other binaries.

The agent session carries opaque JSON payloads for driver operations. Protobuf defines the stream envelope (register, heartbeat, command, logs, metrics, terminal bytes). Serde types in `rudder-core` will be the source of truth for those payloads when the driver trait lands.

## Alternatives Considered

1. One binary with feature flags. That links the panel into every agent.
2. Fully typed protobuf for every Docker call. That duplicates the domain model in two languages' worth of generated structs.

## Consequences

Positive:

- Each process can be built and measured on its own.
- Local and remote drivers can share one JSON payload schema.

Tradeoffs:

- A payload change must stay compatible with agents that have not updated yet. The panel will negotiate versions on the welcome message.

## Follow-Up

- Implement `NodeDriver` in `rudder-core` and the local driver in the server crate.
