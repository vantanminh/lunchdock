# Databases and backups

Rudder runs managed databases as containers on a server through the `NodeDriver`
and streams their backups to one or more S3-compatible buckets. Nothing is staged
on the panel's disk: dump output flows through compression, optional encryption,
hashing, and a bandwidth limiter straight into a multipart upload.

## Managed databases

| Engine   | Versions      | Dump tool                         | Restore tool                 |
|----------|---------------|-----------------------------------|------------------------------|
| Postgres | 17, 16        | `pg_dump -Fc` (or `pg_dumpall`)   | `pg_restore --clean` / `psql` |
| MySQL    | 8.4, 8.0      | `mysqldump --single-transaction`  | `mysql`                      |
| MariaDB  | 11.4, 10.11   | `mariadb-dump --single-transaction` | `mariadb`                  |
| MongoDB  | 8, 7          | `mongodump --archive`             | `mongorestore --archive --drop` |
| Redis    | 7.4, 7.2      | `BGSAVE`, then the RDB file       | copy RDB into the volume     |

- Passwords are 32 random alphanumerics, stored AES-256-GCM encrypted in the
  service's environment. The environment list shows keys only. Admins can reveal
  the connection string from the database page (`GET /api/v1/services/{id}/connection`);
  each reveal is written to the audit log.
- Databases join the internal `rudder-proxy` network. A public port is optional.
- Data lives in the named volume `db-<container>`. Deleting a database keeps the
  volume unless `?purge_volume=true` is passed.
- On a remote server without a connected agent the row is stored and the API
  answers `409 remote_unavailable`; start it once the agent connects.

## Backup jobs

A job targets a `database` service, a Docker `volume`, a container `bind`
mount, or a host `path`. Options:

- **Schedule**: five-field cron or a preset (`@hourly`, `@daily` = 03:00,
  `@weekly`, `@monthly`, `@every6h`) evaluated in the job's IANA timezone.
  "Back up now" queues a manual run.
- **Destinations**: one or more S3 destinations. A run that reaches only some of
  them ends as `partial`.
- **Object key**: `path_prefix/filename_template + extension`. Template tokens:
  `{name}`, `{service}`, `{engine}`, `{timestamp}` (`YYYYMMDDTHHMMSSZ`),
  `{date}`, `{unix}`. Extensions describe the pipeline, for example
  `.dump.zst.enc`.
- **Retention**: `keep_last`, `keep_days`, `daily`, `weekly`, `monthly`. Expired
  objects are deleted and their runs are marked `expired`.
- **Compression**: `none`, `gzip` (level 1–9), `zstd` (level 1–19).
- **Encryption**: optional per-job passphrase (see below).
- **Tool options**: `dump_all` (Postgres), include/exclude databases and tables,
  extra dump arguments (validated; output-redirecting flags such as `--file`
  are refused), include/exclude globs for archive backups.
- **Consistency**: stop the service during a volume/bind/path backup and start it
  again afterwards; pre/post hooks run inside the container via `NodeDriver::exec`.
- **Limits**: timeout, retries with exponential backoff (5 s doubling, max
  5 min), bandwidth limit in bytes/second, and `servers.backup_concurrency`
  parallel runs per server (default 2).
- **Verify**: optionally download the object again and compare its SHA-256.

Every run records status, duration, size, checksum, object keys, and a log with
secrets redacted. Failures create a `backup.failed` system event that the alert
system (M10) delivers to notification channels.

## Restoring

From a run in the history, choose **Restore**:

- Database backups restore into the same service or another database service of
  the same engine (`target_service_id`).
- Volume backups restore into the original volume or a new one (`target_volume`).
- Bind and path backups restore into the original or another host path
  (`target_path`).

Restores are recorded as runs with `kind = restore`.

## Encrypted backups

With a passphrase, the compressed stream is sealed in the `RDRENC01` envelope:

```
magic "RDRENC01" | salt (16) | nonce prefix (8) | chunk size (u32 BE)
frames: length (u32 BE) | AES-256-GCM ciphertext+tag
```

The key is Argon2id(passphrase, salt; m=19456 KiB, t=2, p=1). Each frame's nonce
is the prefix followed by a 32-bit counter, and the final frame is authenticated
as last, so truncation and reordering are detected.

The passphrase is stored encrypted with the panel master key so scheduled runs
and in-panel restores work. **Keep a copy elsewhere**: if the panel is lost, the
passphrase is the only way to read the backup.

### Manual restore without the panel

1. Download the object (from the run's **Download** button, or with any S3 client).
2. Decrypt it:

   ```bash
   export RUDDER_BACKUP_PASSPHRASE='your passphrase'
   rudder backup decrypt --in pg-20260101T030000Z.dump.zst.enc --out pg.dump.zst
   ```

3. Decompress: `zstd -d pg.dump.zst` or `gunzip pg.dump.gz`.
4. Load it with the engine's tool:

   ```bash
   pg_restore --clean --if-exists --no-owner -d "$DATABASE_URL" pg.dump   # Postgres custom format
   psql "$DATABASE_URL" -f all.sql                                        # pg_dumpall output
   mysql -u root -p < app.sql                                             # MySQL / MariaDB
   mongorestore --archive=app.archive --drop --uri "$MONGO_URL"           # MongoDB
   ```

   For Redis, stop the container, copy `dump.rdb` into the volume's data
   directory, and start it. Volume, bind, and path backups are plain tar streams:
   `tar -xf backup.tar -C /target`.

## Testing

`crates/server/tests/backups.rs` covers the API without Docker. With
`RUDDER_DOCKER_TESTS=1` it also starts Postgres, Redis, MariaDB, MongoDB, and an
S3-compatible server (RustFS; MinIO no longer publishes images) and runs real
backups and restores against them.
