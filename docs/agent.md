# Remote servers and the agent

A remote server runs `rudder-agent`, a single static binary. The agent dials the panel, runs the panel's Docker calls on its own machine, and ships that machine's logs and metrics back. The panel never connects to the server, so servers behind NAT or a firewall work as long as they can reach the panel.

```
panel :7443 (gRPC, mTLS)  ◀── dial out ──  rudder-agent ── Docker Engine
                                             └─ rudder-traefik, rudder-cache, apps
```

## Add a server

1. In **Servers**, add a server with kind `remote`. The panel shows a one-line command with a one-time join token that expires in an hour.
2. Run the command on the server as root:

   ```bash
   curl -fsSL https://panel.example.com/install-agent.sh | sudo sh -s -- \
     --server https://panel.example.com --token join_...
   ```

   The installer needs Docker, systemd, `curl`, and `sha256sum`. It:
   - downloads `rudder-agent` for the machine's architecture from the panel (`/downloads/...`), or from the GitHub release when the panel has no bundled copy, and checks its SHA-256;
   - runs `rudder-agent enroll`, which creates a private key on the server and exchanges the token and a certificate request for a client certificate;
   - creates the `rudder-proxy` network and starts `rudder-traefik` (ports 80 and 443, JSON access log) and `rudder-cache` (admin port on `127.0.0.1:9090` only);
   - installs and starts the `rudder-agent` systemd unit.
3. The server turns **online** within a few seconds. The Servers page shows the agent version, hostname, OS, Docker version, and last contact.

Options: `--version v1.2.3` pins the GitHub release, `--email` sets the Let's Encrypt account email, and `--no-edge` skips Traefik and the cache. Running the installer again upgrades the binary and keeps the existing identity. To move a server to a new identity, use **Join command** on the server row to get a fresh token, delete `/var/lib/rudder-agent` on the server, and rerun the command. The old certificate stops working as soon as the new one is issued.

## What works on a remote server

Everything the local server supports goes through the agent: image deploys and rollbacks, Git builds (built on the panel host, then streamed to the server with `docker save`/`docker load`), databases, backups and restores, compose stacks, domains and certificates, the HTTP cache, logs and the live tail, the web terminal, metrics and alerts, and Docker prune.

Routing changes made while an agent is offline are stored and written to the server when it reconnects. Deploys to a server whose agent is offline fail immediately with `remote_unavailable`.

Git-sourced compose stacks on a remote server receive the generated compose file and `.env` only. Other files from the repository (build contexts, mounted configuration) are not copied yet.

## Security

- The join token is stored only as a SHA-256 hash, works once, and expires after an hour. Enrollment is rate limited and audited (`server.enroll`).
- The agent's private key never leaves the server. The panel signs the request's public key with its own certificate authority, whose key is sealed with the master key.
- Every gRPC call requires a client certificate from that CA, and the certificate's fingerprint must match the server row. Deleting a server, or enrolling it again, disconnects the old session and rejects its certificate.
- The agent verifies the panel against the same CA using the name `rudder-panel`, so the panel can be reached by IP address.
- The panel only sends `NodeDriver` operations. File writes are confined to `/var/lib/rudder` on the server, with mode 0600.

## Telemetry and outages

The agent samples host and container stats every 10 seconds and follows the output of Rudder-managed containers, `rudder-traefik`, and `rudder-cache`. It keeps doing this while the panel is unreachable and holds up to 20,000 log lines (4 MiB) and one hour of stats in memory. When the buffer is full the oldest items are dropped; the server row counts them (`telemetry_dropped`). Batches are resent until the panel acknowledges them. Log positions are saved only after acknowledgement, so restarting the agent re-reads missed lines from Docker.

The agent reconnects with backoff from 1 to 30 seconds. The panel marks the server offline when the session ends and records `server.online` and `server.offline` events, which the alert rules can notify on.

## Panel settings

| Variable | Default | Meaning |
| --- | --- | --- |
| `RUDDER_AGENT_BIND` | `0.0.0.0:7443` | gRPC listener for agents, or `off` |
| `RUDDER_AGENT_URL` | panel host + agent port | Address agents are told to dial (`https://` only), for example when a proxy or NAT changes the port |
| `RUDDER_DIST_DIR` | `/usr/share/rudder/dist` | Agent binaries served at `/downloads/rudder-agent-<arch>-unknown-linux-musl` with a `.sha256` file |

## Agent files and commands

| Path | Content |
| --- | --- |
| `/usr/local/bin/rudder-agent` | The binary |
| `/var/lib/rudder-agent/` | `key.pem`, `cert.pem`, `ca.pem`, `agent.json`, `log-cursors.json` (mode 0600, directory 0700) |
| `/var/lib/rudder/` | Files the panel writes: Traefik routes and certificates, cache rules, compose stacks |
| `/etc/systemd/system/rudder-agent.service` | The unit |

```
rudder-agent enroll --server <panel URL> --token <token> [--data-dir /var/lib/rudder-agent]
rudder-agent run [--data-dir /var/lib/rudder-agent] [--node-dir /var/lib/rudder]
                 [--docker-socket <path>] [--cache-admin-url <url>]
rudder-agent --version
```

`journalctl -u rudder-agent` shows the agent's JSON log. The same lines appear in the panel's log explorer under source `agent`.

## Building the agent

```bash
rustup target add x86_64-unknown-linux-musl
sudo apt-get install musl-tools
CC_x86_64_unknown_linux_musl=musl-gcc \
  cargo build --release -p rudder-agent --target x86_64-unknown-linux-musl
```

The result is a static-pie binary of about 11 MB.
