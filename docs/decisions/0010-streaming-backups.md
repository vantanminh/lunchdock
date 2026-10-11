---
created_at: "2026-10-11T04:44:42.786316762+00:00"
doc: docs/decisions/0010-streaming-backups.md
id: "0010"
notes: null
status: accepted
title: Streaming database and volume backups to S3
type: decision
updated_at: "2026-10-11T04:44:42.786317335+00:00"
verify: null
---

# Streaming database and volume backups to S3
Date: 2026-10-11

## Status

Accepted

## Context

Managed databases need generated credentials, lifecycle control, and backups to several S3-compatible buckets. Dumps can be larger than the panel's free disk, the panel may run on a small VPS, and the same code must later work on remote servers through the agent. Passwords must never reach logs, list APIs, or command lines.

## Decision

Databases are containers created through `NodeDriver` from the existing `database_runtime` plan. Passwords are 32 random alphanumerics stored encrypted in `env_vars`; only an admin reveal endpoint returns them, and every reveal is audited. Database tools receive passwords through exec environment variables (`PGPASSWORD`, `MYSQL_PWD`, `REDISCLI_AUTH`, and a fixed `sh -c 'exec "$@" --password="$MONGO_PASSWORD"'` wrapper for Mongo tools), never argv.

`NodeDriver` gains streaming methods with "not supported" defaults: `exec_read`, `exec_write`, `archive_read`, `archive_write`, `remove_volume`, and `read_file`. Volume and host-path archives use a short-lived `alpine:3.20` helper container that mounts the source and runs `tar` over exec, so no temp files are written on the host. A remote driver implements the same methods in M11.

A backup is one stream: dump or tar, glob filter (tar rewrite on a blocking thread), compression (gzip or zstd via `async-compression`), optional encryption, SHA-256 meter, token-bucket throttle, then a fan-out into bounded channels, one per destination. Each destination receives an S3 multipart upload through `object_store` (rustls via reqwest) with 8 MiB parts and two parts in flight. A run that reaches only some destinations is `partial`; a source error aborts every upload. Optional verification downloads the object and compares the checksum.

Client-side encryption uses the `RDRENC01` envelope: Argon2id key derivation from a per-job passphrase, AES-256-GCM frames with a counter nonce, and a last-frame flag in the AAD so truncation is detected. The passphrase is stored encrypted with the master key; `rudder backup decrypt` restores without the panel.

Redis is backed up by `BGSAVE`, polling `INFO persistence` until `rdb_bgsave_in_progress` is 0 (not `LASTSAVE`, which has one-second resolution), then archiving the data volume. Restores stream the object back through `exec_write` into `pg_restore`/`psql`/`mysql`/`mariadb`/`mongorestore`, or through `archive_write` for Redis and archives.

Schedules are cron expressions or presets evaluated in the job's IANA timezone with `cron` and `chrono-tz`; the scheduler checks every 30 seconds and anchors on the newer of the job creation time and the last run. Concurrency is limited per server by `servers.backup_concurrency` semaphores. Retention uses `rudder_core::expired_ids`, deletes expired objects, and marks their runs `expired`. Failures write `system_events` rows that the alert system delivers in M10. Runs left `queued` or `running` by a restart are marked failed at boot.

## Consequences

Backups need the dump tools inside the database image, which the official images provide. The helper image must be pullable on each server. MinIO no longer publishes images or binaries, so the Docker-gated tests use RustFS as the S3-compatible server and rclone to create the bucket. Losing both the panel and the passphrase makes an encrypted backup unreadable, which the docs state plainly.
