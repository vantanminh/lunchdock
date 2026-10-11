---
created_at: "2026-10-11T02:01:24.304638969+00:00"
doc: docs/decisions/0002-sqlx-compile-time-queries.md
id: "0002"
notes: null
status: accepted
title: Compile-time checked SQL with an offline cache
type: decision
updated_at: "2026-10-11T02:01:24.304640999+00:00"
verify: null
---

# Compile-time checked SQL with an offline cache

Date: 2026-10-11

## Status

Accepted

## Context

The panel database is PostgreSQL via sqlx and `sqlx::migrate!`. Query macros need a live database at compile time, which CI does not have during the rustc step unless a service is already migrated. Hand-written SQL strings would drift from the schema.

## Decision

Row-shaped reads and writes use `sqlx::query!` / `sqlx::query_as!`. CI sets `SQLX_OFFLINE=true` and compiles against the committed `.sqlx` directory. After a query or migration change, regenerate with `cargo sqlx prepare --workspace` against a migrated database.

Statements that only call maintenance functions (partition creation) stay on `sqlx::query` because they have no stable output row.

## Alternatives Considered

1. Runtime-only `sqlx::query` everywhere. Faster to iterate, no compile-time column checks.
2. Requiring PostgreSQL for every CI compile, with no offline cache. More moving parts in the workflow.

## Consequences

Positive:

- A renamed column fails the build.
- CI does not need a migrated database at compile time. Tests still run against PostgreSQL 16.

Tradeoffs:

- Forgetting to refresh `.sqlx` makes CI fail after a query change. That failure is the point.

## Follow-Up

- Keep the cache updated in the same commit as the query.
