# Panel backup and disaster recovery

The panel's state is its PostgreSQL database plus the master key that decrypts the
secrets stored in it (environment values, database passwords, registry and S3
credentials, deploy keys, certificates, the agent CA). Everything under
`/var/lib/rudder` — Traefik dynamic config, certificate files, cache rules, compose
files — is regenerated from the database when the panel starts and when services
are deployed.

## What a panel backup contains

A panel backup is a tar archive, compressed with zstd and always encrypted with
the `RDRENC01` envelope (Argon2id key from a passphrase, AES-256-GCM frames):

| Entry | Content |
| --- | --- |
| `manifest.json` | Format version, Rudder version, schema migration, creation time |
| `master.key` | The 32-byte master key, hex |
| `database.pgdump` | `pg_dump --format=custom --no-owner --no-acl` of the panel database |
| `settings.json` | The secret-free settings export, for humans |

Log lines, metric samples and rollups, and sessions are left out unless
**Include history** is ticked. Restored users sign in again.

The passphrase must be at least 12 characters. Rudder never stores the passphrase
of a downloaded backup. For scheduled jobs it is stored encrypted with the master
key, like every other backup passphrase, so keep a copy somewhere else. Without
the passphrase the backup cannot be opened.

## Taking backups

- **System → Panel backup** downloads one now (admin with write access).
- **System → Scheduled panel backups** creates a backup job with target
  `panel_db` (database and master key) or `panel_settings` (settings export only),
  uploaded to S3 destinations on a cron schedule with retention, like any other
  backup job. Runs show up on the Backups page; failures raise `backup.failed`
  events for alerts.
- From the host: `sudo sh install.sh --backup rudder-panel.tar.zst.enc` asks for a
  passphrase (or reads `RUDDER_BACKUP_PASSPHRASE`) and runs
  `rudder-server backup --out -` inside the panel container.

`GET /api/v1/system/settings-export` returns the configuration as JSON. Columns
ending in `_enc` or `_hash`, TOTP secrets, and CSRF tokens are removed at any
depth, so the export cannot be used to restore; use it to review or diff
configuration.

## Restoring on a new host

1. Copy the backup to the new server. To fetch it from S3, use the **Download**
   link of the run on the old panel or any S3 client.
2. Run the installer with `--restore`:

   ```bash
   curl -fsSL https://github.com/vantanminh/rudder/releases/latest/download/install.sh -o install.sh
   sudo sh install.sh --restore rudder-panel-20261011-084801.tar.zst.enc
   ```

   It asks for the passphrase (or reads `RUDDER_RESTORE_PASSPHRASE`), starts
   PostgreSQL, and runs `rudder-server restore` from the panel image before the
   panel starts. The restore:

   - refuses a backup whose schema is newer than the binary (install that version first);
   - refuses a database that already has tables and a different existing master key, unless `--force` is given;
   - restores with `pg_restore --single-transaction --exit-on-error`, so a failure leaves the database untouched;
   - writes `/etc/rudder/master.key` (mode 0600) from the backup.

3. The panel starts, applies newer migrations, and republishes the local Traefik
   configuration. Containers do not exist on the new host yet: deploy each service
   (or restore its data backups) to bring it back. Agents on other servers keep
   their certificates, because the agent CA is restored, and reconnect once DNS or
   `RUDDER_AGENT_URL` points at the new panel.

`--force` also replaces a running installation on the same host: it stops the
panel, drops the panel database schema, and restores the backup.

## Restoring by hand

Without the installer:

```bash
RUDDER_BACKUP_PASSPHRASE=... rudder backup decrypt --in rudder-panel.tar.zst.enc --out panel.tar.zst
zstd -d panel.tar.zst && tar xf panel.tar
pg_restore --no-owner --no-acl --single-transaction -d "$DATABASE_URL" database.pgdump
install -m 0600 master.key /etc/rudder/master.key
```

Or, with the panel binary and `DATABASE_URL` set:

```bash
RUDDER_RESTORE_PASSPHRASE=... rudder-server restore --file rudder-panel.tar.zst.enc --master-key-file /etc/rudder/master.key
```
