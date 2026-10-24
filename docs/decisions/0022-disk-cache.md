---
created_at: "2026-10-24T01:29:11.269358440+00:00"
doc: docs/decisions/0022-disk-cache.md
id: "0022"
notes: Uses the existing disk_max_bytes column. Memory stays the hot tier.
status: accepted
title: Keep cached objects on disk up to a policy budget
type: decision
updated_at: "2026-10-24T01:29:11.269358935+00:00"
verify: null
---

# Keep cached objects on disk up to a policy budget

Date: 2026-10-24

## Status

Accepted

## Context

The HTTP cache keeps whole objects in memory. They disappear when the cache process restarts, and the store cannot grow past RAM. `cache_policies.disk_max_bytes` was already in the schema and unused.

## Decision

A policy may set a disk budget. The cache server adds up the budgets of the policies in use. Zero, or every policy left blank, keeps the current memory-only behavior and deletes files left from an earlier budget.

Objects are files under `cache/disk` on that server, with `index.json` for eviction and purge. A missing index is rebuilt from the files. Memory remains the hot tier: a disk hit is copied into memory, and a new object is written to both when a disk budget is set. Purge and host removal apply to both tiers.

## Consequences

- Restarting the cache process no longer drops objects that were stored on disk.
- The Cache page shows the disk budget and how much of it is used.
- An older cache binary ignores the new rules field and stays memory-only until it is updated.
