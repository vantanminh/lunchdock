---
created_at: "2026-10-11T08:49:55.960576691+00:00"
doc: docs/decisions/0016-panel-backup-and-restore.md
id: "0016"
notes: null
status: accepted
title: Panel backups carry the master key and restore offline
type: decision
updated_at: "2026-10-11T08:49:55.960577160+00:00"
verify: null
---

# Panel backups carry the master key and restore offline

## Context

Losing the panel host must not mean rebuilding every project by hand. The panel's
state is its PostgreSQL database, and most sensitive columns in it are ciphertext
under the master key. A database dump without that key cannot decrypt environment
values, credentials, certificates, or the agent CA, so restoring it would leave a
panel that cannot deploy anything or talk to its agents. Restoring the panel's own
database from inside the running panel is unsafe: the process holds connections,
caches state, and would be replaced mid-request.

## Decision

- A panel backup is a tar of `manifest.json`, `master.key`, `database.pgdump`
  (`pg_dump --format=custom --no-owner --no-acl`), and a secret-free
  `settings.json`, compressed with zstd and always sealed with the existing
  `RDRENC01` passphrase envelope. Passphrases need 12 or more characters. Backup
  jobs with target `panel_db` refuse to save without one.
- Log entries, metric samples and rollups, and sessions are excluded with
  `--exclude-table-data` unless history is requested.
- `pg_dump` and `pg_restore` run with `Command::new` argv. The connection comes
  from libpq `PG*` environment variables parsed from `DATABASE_URL`, so the
  password never appears in argv. The panel image ships `postgresql16-client`.
- The dump goes to a private scratch directory (0700, files 0600) under the data
  directory, because tar needs entry sizes up front. The directory is removed when
  the archive stream ends or is dropped.
- Panel backups reuse backup jobs (`panel_db`, `panel_settings`) for schedules,
  S3 fan-out, verification, retention, and failure events. They can also be
  downloaded directly (`POST /api/v1/system/backup`, admin with write) or taken
  from the host with `rudder-server backup`.
- Restore is offline only: `rudder-server restore`, called by
  `install.sh --restore` before the panel container starts. It refuses archives
  with unknown entries, a newer schema than the binary, a non-empty database, or
  a different existing master key, unless `--force` is given. It restores in one
  transaction and then writes the key file atomically with mode 0600. The
  in-panel restore endpoint answers `409 offline_restore` for panel runs.
- The settings export removes every `*_enc` and `*_hash` key, TOTP secrets, and
  CSRF tokens at any depth, so new encrypted columns stay out without code changes.
- On startup the panel republishes the local server, so a restored panel rewrites
  Traefik configuration before anything is redeployed.

## Consequences

- Whoever holds a panel backup and its passphrase holds every secret in Rudder.
  The passphrase is the only protection, so the UI asks for it twice and the docs
  say to store it apart from the backup.
- Restores need the PostgreSQL client major version to match or exceed the server
  (16). The panel image pins `postgresql16-client` and the installer pins
  `postgres:16-alpine`.
- Containers and volumes are not part of a panel backup. After a restore, services
  are redeployed and their data comes back from their own backup jobs.
- A downloaded backup is assembled in browser memory before it is saved. Large
  installations should use scheduled S3 jobs or `install.sh --backup`.
