---
created_at: "2026-10-11T05:53:48.672613326+00:00"
doc: docs/decisions/0013-central-logs.md
id: "0013"
notes: null
status: accepted
title: Central logs in partitioned Postgres with a broadcast tail
type: decision
updated_at: "2026-10-11T05:53:48.672613982+00:00"
verify: null
---

# Central logs in partitioned Postgres with a broadcast tail

## Context

M9 needs searchable logs from apps, Traefik, the cache, and the panel itself, with
live tail, export, and per-source retention, on a single small VPS. Running Loki,
Elasticsearch, or Vector beside the panel would blow the memory budget (control plane
under 60 MB idle) and add another stateful service to back up.

## Decision

- Logs live in the existing Postgres `log_entries` table, range-partitioned by day
  (`log_entries_YYYYMMDD`) with a trigram index on `message`. Search is keyset-paginated
  on `(logged_at, id)` and uses `ILIKE ... ESCAPE` with user text escaped.
- A single in-process `LogHub` owns a bounded mpsc queue (20k lines). One writer task
  batches up to 500 lines or 500 ms into one `INSERT ... SELECT FROM UNNEST` and then
  broadcasts the stored rows (4096-slot broadcast) to live-tail subscribers. A full queue
  drops lines rather than blocking producers; a lagging tail client gets a
  `{"type":"lagged"}` notice.
- The local collector polls `list_containers` every 10 s through `NodeDriver` and
  follows `rudder.managed` containers (`app`), `rudder-traefik` (`proxy`, JSON access
  log, host mapped to service through `domains`), and `rudder-cache` (`cache`). It resumes
  from the last stored line of that container within an hour, else 15 s back. Env values
  and database credentials are redacted before parsing. Remote servers ship their own
  lines through the agent (M11), so the collector never dials remote Docker.
- Panel events at INFO and above are copied by a `tracing` layer into the same queue
  (`panel` source), skipping the log pipeline's own target and noisy library targets so
  the pipeline cannot feed itself.
- Retention runs hourly: create partitions up to two days ahead, delete expired rows per
  source, and drop whole partitions older than the longest retention. Partition names come
  only from `pg_inherits` and must match `log_entries_YYYYMMDD` before being quoted into
  `DROP TABLE`.
- Live tail and the terminal are WebSockets on the panel origin. Browser upgrades must
  carry an `Origin` that matches `Host` (cookie auth on GET has no CSRF header). The
  terminal requires write permission, uses a fixed shell argv, and is audited.

## Consequences

- No new service or dependency; backups of the panel database include recent logs.
- Throughput is bounded by Postgres inserts (thousands of lines per second in batches),
  which is enough for a single-server panel but not a log analytics platform.
- Lines can be lost if the queue overflows or the panel restarts mid-batch; logs are
  operational aids, not an audit trail (the audit log stays separate).
- Docker `since` has second granularity, so resumption skips lines at or before the last
  stored timestamp; identical-timestamp lines straddling a restart may be skipped.
