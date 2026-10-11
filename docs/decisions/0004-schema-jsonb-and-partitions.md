---
created_at: "2026-10-11T02:01:24.378771382+00:00"
doc: docs/decisions/0004-schema-jsonb-and-partitions.md
id: "0004"
notes: null
status: accepted
title: "Initial schema, JSON specs, and daily partitions"
type: decision
updated_at: "2026-10-11T02:01:24.378773823+00:00"
verify: null
---

# Initial schema, JSON specs, and daily partitions

Date: 2026-10-11

## Status

Accepted

## Context

The product needs users, servers, projects, services, domains, certificates, cache rules, deployments, backups, notifications, metrics, logs, retention, updates, and an audit log. Service kinds (Git app, image app, database, compose) do not share one column set. Logs and metrics must be dropped by age without a full-table delete.

## Decision

Migration `0001_init.sql` creates the relational tables listed in the product model. Kind-specific desired state is stored in `services.spec` as JSONB. Secrets are `BYTEA` ciphertext, never plaintext columns.

`log_entries`, `metrics_samples`, and `metrics_rollups` are partitioned by day. A default partition catches rows if a daily partition is missing. `rudder_ensure_daily_partition` creates a day's partition when the default partition has no conflicting rows. The server calls it on startup for today and tomorrow. Foreign keys are omitted on the partitioned tables so partition maintenance does not have to cascade through them; service and server ids are still stored for filtering.

A single `panel_settings` row and a single `retention_settings` row hold singleton configuration. The default metrics retention document is raw 24h, 1-minute 7d, 5-minute 14d, 1-hour 30d. Log retention defaults to 14 days per source.

## Alternatives Considered

1. A separate table per service kind. More joins, more migrations, little extra integrity beyond what serde validation provides at the boundary.
2. Unpartitioned log and metric tables with `DELETE` jobs. Simpler, and it will not keep up with container logs.

## Consequences

Positive:

- New service fields can land in the spec struct without a migration when they are not queried in SQL.
- Retention can drop a partition per day.

Tradeoffs:

- Spec JSON is not checked by Postgres. The application must validate it before insert.
- Queries that filter logs by service scan the partition's index rather than a foreign key.

## Follow-Up

- Background jobs create future partitions and drop expired ones.
