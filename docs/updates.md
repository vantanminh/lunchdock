# Updates

Rudder updates itself from signed GitHub Releases. The panel replaces its own container, then brings every agent and cache to the same version. Each step checks its health and goes back to the previous version on failure.

## What a release contains

Version 0.3.0 was published without installation assets after its release build failed. Use 0.3.1 or newer for installation and updates. Version 0.3.1 includes the load guard and dashboard changes intended for 0.3.0, and waits for the agent's staged binary to close before executing its version check.

| Asset | Purpose |
| --- | --- |
| `release.json` | Version, panel and cache images pinned by `@sha256` digest, sha256 of each agent build |
| `rudder-agent-<target>` | Agent binaries for `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` |
| `rudder-server-<target>`, `rudder-cache-<target>`, `rudder-<target>` | Static panel and cache binaries and the `rudder` CLI |
| `install.sh`, `install-agent.sh` | Installers for this version |
| `SHA256SUMS` | Checksums of every other asset |
| `SHA256SUMS.minisig` | [minisign](https://jedisct1.github.io/minisign/) signature of `SHA256SUMS` |

The panel trusts one minisign public key. Release builds compile it in. You can override it with `RUDDER_UPDATE_PUBKEY`, either as the base64 key or as the whole `.pub` file. Without a key the panel still shows new releases, but it refuses to apply them.

A release is **verified** when:

1. the signature over `SHA256SUMS` checks out with the trusted key (legacy, non-prehashed signatures are refused),
2. `SHA256SUMS` lists `release.json` with the matching hash,
3. the manifest's version equals the release tag, so an older signed release cannot be served under a new tag,
4. both images are pinned by digest, and
5. the version is newer than the running one. The stable channel ignores prereleases, and the prerelease channel includes them.

You can check a release by hand:

```sh
minisign -Vm SHA256SUMS -P "$RUDDER_UPDATE_PUBKEY"
sha256sum -c SHA256SUMS
```

## Updating from the panel

Open **System → Updates**. **Check now** reads the releases of the repository set in panel settings (`vantanminh/lunchdock` by default). The panel shows the newest release and whether it verified, and under **What this update installs** the notes of every release between your version and the newest one, newest first, so you can see what is about to be installed before you press the button. An admin with write access can then click **Update to …**.

The panel then does the following:

1. Writes an encrypted pre-update backup to `<data dir>/updates/pre-update-<from>-<time>.tar.zst.enc`. The file has mode 0600 and is sealed with the master key, and the last three are kept. It is a normal panel backup, so `rudder-server restore --passphrase <hex master key>` can read it.
2. Pulls the panel image by digest.
3. Starts a one-off `rudder-updater` container from the new image, with the same mounts and environment as the panel.

The updater then does the following:

1. Renames `rudder` to `rudder-previous` and starts a new `rudder` container on the new image. The new container keeps the same environment, mounts, ports, restart policy, and networks.
2. Waits up to 180 seconds for `GET /api/health` inside the new container to report the new version.
3. If the check passes, it removes `rudder-previous` and records the update as `succeeded`.
4. If the check fails, it stops the new container. If the new version had already migrated the database, it restores the pre-update backup. It then renames `rudder-previous` back and waits for the old version to answer. The update is recorded as `rolled_back`, or as `failed` if the old panel did not come back, together with the new container's last output.

The page polls `/api/health` and reloads when the panel answers again. When a rollback restored the database, everyone has to sign in again, because panel backups do not carry sessions. Any changes made between the backup and the rollback are also lost.

The panel can only replace itself when it runs as a Docker container named `rudder`, which is what `install.sh` creates. You can change the name with `RUDDER_SELF_CONTAINER`. Any other setup is upgraded by running `install.sh --version <version>` again.

## Agents and caches

After a panel update, **Agents and caches** lists every server with its agent version and cache image. **Update components** works through outdated servers one at a time. On each server it updates the agent first, then the cache. A failed agent update skips that server's cache.

- **Agent:** the panel streams the agent binary bundled in its image (`RUDDER_DIST_DIR/rudder-agent-<target>`) over the existing session. It checks the hash against the `.sha256` file and the signed manifest first. The agent:
  1. writes the binary next to itself as `.new` and checks its sha256,
  2. runs it with `--version` and expects `rudder-agent <version>`,
  3. moves the current binary to `.prev`, puts the new one in place, writes an `update-pending` marker, and exits with code 75.

  systemd (`Restart=always` in the unit `install-agent.sh` writes) starts the new binary. Once the panel welcomes the new session, the marker is cleared. If no session is welcomed within `RUDDER_AGENT_UPDATE_GRACE_SECS` (default 120 seconds), the agent puts `.prev` back and exits, so systemd starts the old version again.
- **Cache:** the server pulls the cache image named in the release and replaces `rudder-cache` with it. The new container keeps the same settings and must answer `GET :9090/health` within 60 seconds. Otherwise the previous container is restored and the update is recorded as `rolled_back`.

Agents installed before self-update existed cannot report their target. Reinstall them once with `install-agent.sh`.

## Automatic updates

| Setting | Default | Meaning |
| --- | --- | --- |
| Channel | `stable` | `stable` or `prerelease` |
| Automatic updates | off | Apply verified releases without a click |
| Maintenance window | none | `HH:MM-HH:MM` in UTC, for example `02:00-04:00`; it may wrap past midnight. With auto-update on and no window, updates apply at any time |

The panel checks every `RUDDER_UPDATE_CHECK_SECS` seconds (default `21600`; `0` turns checks off). The first check runs a minute after startup. When auto-update is on and the current time is inside the window, the panel applies a newer verified release. Otherwise it updates outdated components. An update stuck as `running` for an hour is marked `failed`.

## API

| Method and path | Role | Description |
| --- | --- | --- |
| `GET /api/v1/system/updates` | admin | Versions, last check, settings, components, history |
| `POST /api/v1/system/updates/check` | admin, write | Check for a release now. `latest` is the newest release; `pending` lists every release above the installed version, oldest first, with its notes |
| `PUT /api/v1/system/updates/settings` | admin, write | `{"channel":"stable","auto_update":false,"maintenance_window":"02:00-04:00"}` |
| `POST /api/v1/system/updates/apply` | admin, write | Start a panel update (`202 {"history_id": …}`, `409 update_unavailable`) |
| `POST /api/v1/system/updates/components` | admin, write | Update outdated agents and caches (`202 {"queued": n}`) |

Starting an update, updating components, and changing update settings are recorded in the audit log.

## Configuration

| Variable | Default | Meaning |
| --- | --- | --- |
| `RUDDER_UPDATE_PUBKEY` | built in | minisign public key that signs releases |
| `RUDDER_UPDATE_API_URL` | `https://api.github.com` | GitHub API base, or a mirror that serves the same `/repos/<owner>/<repo>/releases` JSON |
| `RUDDER_UPDATE_CHECK_SECS` | `21600` | Background check interval; `0` disables it |
| `RUDDER_SELF_CONTAINER` | `rudder` | Name of the panel's own container |
| `RUDDER_AGENT_UPDATE_GRACE_SECS` | `120` | How long an updated agent waits for the panel before restoring its previous binary |

## Publishing a release

`.github/workflows/release.yml` runs on `v*` tags. A `preflight` job runs first and fails the run when `CHANGELOG.md` has no entries for the tag or the signing settings are missing. Then:

1. It builds musl binaries for x86_64 and aarch64 with `cargo zigbuild`. `RUDDER_BUILD_VERSION` is set from the tag and `RUDDER_UPDATE_PUBKEY` from the repository variable of the same name.
2. It pushes multi-arch `ghcr.io/<owner>/rudder` and `rudder-cache` images and records their digests.
3. It runs `deploy/release/make-release.sh` to write `release.json` and `SHA256SUMS`.
4. It signs `SHA256SUMS` with the `MINISIGN_SECRET_KEY` secret and checks the signature against the public key before publishing.
5. It adds GitHub build-provenance attestations for every asset and creates the release. The release notes are the `## [x.y.z]` section of `CHANGELOG.md` (`deploy/release/changelog.sh`), which is what the panel shows. Tags with a `-` are published as prereleases.

To create the signing key once:

```sh
minisign -G -W -p rudder.pub -s rudder.key   # -W: no password, for CI
```

Store the contents of `rudder.key` as the `MINISIGN_SECRET_KEY` secret and the second line of `rudder.pub` as the `RUDDER_UPDATE_PUBKEY` variable. Keep an offline copy of the secret key. Every installation trusts it.
