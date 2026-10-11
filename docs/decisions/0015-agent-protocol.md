---
created_at: "2026-10-11T08:08:36.995146372+00:00"
doc: docs/decisions/0015-agent-protocol.md
id: "0015"
notes: null
status: accepted
title: "Agent protocol, enrollment, and remote driver"
type: decision
updated_at: "2026-10-11T08:08:36.995147125+00:00"
verify: null
---

# Agent protocol, enrollment, and remote driver

## Context

Remote servers need the same `NodeDriver` surface as the local Docker socket:
container lifecycle, files, compose, health probes, streaming exec and archives,
log following, interactive terminals, stats, cache calls, and pruning. Servers
are often behind NAT, so the panel cannot dial them. Credentials must not be
long-lived shared secrets, and a lost connection must not lose metrics or logs.

## Decision

- **One driver implementation.** `LocalDriver` moved into a `rudder-node` crate
  used by both the panel and the agent. The agent runs the exact code the panel
  runs for its own machine.
- **Wire form.** `rudder_node::ops::Request` is a serde enum with one variant per
  driver call (tagged by `op`). A `Command` carries it as JSON; the agent answers
  with a `CommandResult` holding the JSON return value. Validation errors are
  marked so they surface as `Error::Validation` on the panel.
- **Transport.** The agent dials out over gRPC (tonic, rustls, aws-lc) to
  `RUDDER_AGENT_BIND` (default `0.0.0.0:7443`). `Session` is one bidirectional
  stream for Hello/Welcome, commands, results, telemetry, and acks. Every
  streaming call (exec read/write, archive read/write, log follow, terminal,
  image save/load) opens its own `Channel` RPC named by a random stream id the
  panel registered first, so bulk transfers get their own HTTP/2 flow control
  and never stall commands. Unclaimed stream slots expire after 60 seconds.
- **Enrollment.** Adding a remote server creates a one-time join token (SHA-256
  stored, one hour). `rudder-agent enroll` generates its key locally and posts
  the token and a CSR to `POST /api/v1/agent/enroll` (rate limited, audited). The
  panel signs only the CSR's public key with its own CA (rcgen, key sealed with
  the master key in `agent_ca`), CN = server id, ten-year validity, and stores
  the certificate's SHA-256 fingerprint. The token is cleared in the same
  transaction.
- **Authentication.** The listener requires a client certificate from the panel
  CA. Each RPC maps the peer certificate fingerprint to a remote server row;
  unknown fingerprints get `PermissionDenied`. Re-enrolling replaces the
  fingerprint and drops the old session; deleting the server does the same. The
  panel's own certificate is regenerated at each start for the fixed name
  `rudder-panel`, which the agent verifies against the CA, so the panel address
  can be an IP or any hostname.
- **Telemetry.** The agent samples host and container stats every 10 seconds
  and follows `rudder.managed` containers plus `rudder-traefik` and
  `rudder-cache`, whether or not the panel is reachable. Items wait in a bounded
  buffer (20,000 log lines or 4 MiB, 360 stat samples). Overflow drops the
  oldest and the count travels with the next batch (`servers.telemetry_dropped`).
  Batches stay in flight until acked and are resent after a reconnect, so
  delivery is at least once. Per-container log cursors advance only on ack and
  persist in `log-cursors.json`, so an agent restart re-reads from Docker instead
  of losing lines. The panel reuses the local collector's attribution, secret
  redaction, and metrics math for agent data.
- **Builds.** Git builds still run on the panel host (git, Docker CLI, nixpacks
  live there). For a remote target the image is streamed with `docker save` to
  the server's engine (`load_image`) before the rollout.
- **Routing.** `publish_server` writes routes, certificates, and cache rules
  through the server's driver. Changes made while an agent is offline are applied
  when it reconnects.

## Consequences

- The agent is a single static binary (musl) with no runtime dependencies other
  than Docker; it idles around 8 MB RSS.
- Every custom certificate is written to every connected server, because the
  Traefik TLS store is shared per server. All servers belong to one owner, so
  this is accepted.
- Git-sourced compose stacks on a remote server receive only the generated
  compose file and `.env`; other repository files are not copied (BL-001).
- The agent buffer is in memory: an agent restart while disconnected loses
  unsent stats samples (logs are re-read from Docker using the acked cursors).
- Port 7443 must be reachable from servers to the panel; nothing needs to be
  reachable in the other direction.
