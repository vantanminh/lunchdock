---
created_at: "2026-10-11T10:20:54.740899151+00:00"
doc: docs/decisions/0017-self-update.md
id: "0017"
notes: null
status: accepted
title: Self-update from signed releases with backup and rollback
type: decision
updated_at: "2026-10-11T10:20:54.740899723+00:00"
verify: null
---

# Self-update from signed releases with backup and rollback

## Context

Operators should not have to SSH into every host to upgrade Rudder. An update
mechanism is also a remote-code-execution channel, so it must accept only builds
the project signed. It must also survive a bad release that migrates the
database and then fails to start. Agents run under systemd on hosts the panel
reaches only through their session, and caches run as containers on every server.

## Decision

- Releases are GitHub Releases. Each one carries `release.json` (format
  `rudder-release`, the version, the panel and cache images pinned by
  `@sha256` digest, and the sha256 of every agent build), `SHA256SUMS` over all
  assets, and `SHA256SUMS.minisig`, a minisign signature (Ed25519, legacy
  signatures refused).
- The trusted public key is compiled in from `RUDDER_UPDATE_PUBKEY` by the
  release workflow and can be overridden by the same variable at runtime.
  Without a key, releases are shown but cannot be applied.
- A release is verified when the signature covers `SHA256SUMS`, `SHA256SUMS`
  covers `release.json`, the manifest version equals the tag (which blocks
  replaying an older signed release), every image is digest-pinned, and the
  version is newer than the running one on the selected channel (stable or
  prerelease). Assets are capped at 4 MiB.
- A panel update takes a process lock. It re-checks the release and records an
  `update_history` row. It then writes an encrypted pre-update backup to
  `data_dir/updates/` (sealed with the master key, mode 0600, last three kept)
  and pulls the image by digest. Finally it starts a one-off `rudder-updater`
  container, a clone of the panel container on the new image.
- The updater does the following:
  - Renames `rudder` to `rudder-previous` and starts a copy of it on the new
    image. The copy keeps the environment, mounts, ports, restart policy, and
    networks.
  - Waits up to 180 s for `/api/health` inside the new container to report the
    new version. On success it removes `rudder-previous`.
  - On failure it stops the new container. If the schema moved past the
    recorded migration, it restores the database from the pre-update backup.
    It then renames the old container back and waits for the old version.
- Components update after the panel, agents first and then caches, one server
  at a time:
  - **Agents:** the panel streams its bundled agent binary over the session.
    The agent checks the sha256 (against the `.sha256` file and the signed
    manifest) and the `--version` output, keeps the old binary as `.prev`,
    writes a pending marker, and exits with code 75 for systemd to restart it.
    If no session is welcomed within the grace period (default 120 s), the
    agent restores `.prev`.
  - **Caches:** the cache container is replaced with the manifest's cache image
    behind an HTTP probe on `:9090/health`, and the old container is restored
    if the probe fails.
- A background task checks every `RUDDER_UPDATE_CHECK_SECS` (default 6 h,
  `0` disables it). It applies updates only when auto-update is on and the time
  is inside the UTC maintenance window.
- The release workflow builds musl binaries for x86_64 and aarch64, pushes
  multi-arch images, generates the manifest, signs and verifies the sums, adds
  build-provenance attestations, and publishes the release.

## Consequences

- The minisign secret key is the root of trust for every installation. It
  lives only in the `MINISIGN_SECRET_KEY` Actions secret.
- A rollback restores the database snapshot taken just before the update, so
  changes made during the update window are lost. Panel backups do not carry
  sessions, so everyone signs in again after a rollback that restored the
  database.
- Agents installed before self-update existed cannot report their target and
  must be reinstalled once with `install-agent.sh`.
- Restarting agents relies on a supervisor (systemd `Restart=always`). Agents
  run by hand stay down after exit 75.
- A panel that is not a Docker container named `rudder` (for example a binary
  run directly) cannot self-update; it reports why, and is upgraded with
  `install.sh --version`.
