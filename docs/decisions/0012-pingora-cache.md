---
created_at: "2026-10-11T05:37:21.658956006+00:00"
doc: docs/decisions/0012-pingora-cache.md
id: "0012"
notes: null
status: accepted
title: Pingora cache with an in-memory LRU store fed by a panel-written rules file
type: decision
updated_at: "2026-10-11T05:37:21.658957692+00:00"
verify: null
---

# Pingora cache with an in-memory LRU store fed by a panel-written rules file

Date: 2026-10-11

## Status

Accepted

## Context

Rudder needs an HTTP cache in front of user apps: per-site policies with presets, purge, an `X-Cache` header, metrics, and rule changes without restarts. The fixed stack says it is our own Rust server on `pingora-proxy` and `pingora-cache`. It runs once per server next to Traefik, has to stay under 30 MB idle, and must not need a database or a connection back to the panel. Traefik already terminates TLS and routes through the file provider (decision 0008).

## Decision

- **Placement.** Traefik keeps TLS and host routing. For a domain with `cache_enabled`, the panel points the Traefik service at `http://rudder-cache:8080` instead of the container. The cache finds the site by `Host` plus longest path prefix and proxies misses to the original `container:port` over the `rudder-proxy` network. Plain HTTP between Traefik and the cache; the cache never terminates TLS.
- **Rules document.** On every publish the panel writes `cache/rules.json` in the node data directory (`rudder_core::CacheDocument`: sites, policies, memory budget, version). The cache polls the file every second by content hash and swaps rules under a lock. A file that fails to parse or validate is logged and the previous rules stay active. Removing a site drops its objects. `POST /reload` forces a check.
- **Policies.** Policies are ordered rules evaluated in `rudder-core` (shared by panel and cache). First match wins. A rule matches on path prefixes, extensions, and a `*` glob, and either bypasses or caches with a TTL, an optional respect-origin mode (`s-maxage`/`max-age`, `no-store`/`private`/`no-cache` honored), stale-while-revalidate and stale-if-error windows, query normalization (sorted, allow-list, or ignored), bypass cookies, and cacheable statuses. Only `GET` is looked up; `Authorization` bypasses. Responses with `Set-Cookie`, or with `Vary` on anything but `Accept-Encoding`, are never stored. Six presets ship (static assets, honor origin, WordPress, Ghost, Next.js, API micro-cache). A cache-enabled domain with no policy uses "Honor origin headers".
- **Key.** The primary key is host, encoding class (`br`, `gzip`, or `identity`, picked from `Accept-Encoding`), and the normalized path and query. The upstream request's `Accept-Encoding` is rewritten to that class so stored variants match their key.
- **Storage.** Our own `Storage` implementation keeps whole objects in memory with a global byte budget (sum of the budgets set on policies in use, default 256 MiB) and least-recently-used eviction down to 90% of the budget. `max_object_bytes` per policy (default 16 MiB) goes to Pingora's max-file-size tracker. A Pingora cache lock collapses concurrent misses and enables stale-while-revalidate background refreshes. No disk tier yet (backlog).
- **Admin API.** A second listener (default `127.0.0.1:9090`, `0.0.0.0:9090` inside the container, not published) serves `GET /health` without auth, and `GET /stats`, `GET /metrics` (Prometheus text), `POST /purge` (exact paths across encodings, prefix, or all for a host), and `POST /reload` behind `Authorization: Bearer <token>`. The token lives in `cache/admin.token` (mode 0600). The panel creates it on first publish, and the cache creates it if started alone.
- **Panel.** `NodeDriver` gained `cache_purge` and `cache_stats`. `LocalDriver` calls the admin URL (`RUDDER_CACHE_ADMIN_URL`) with the token from the data directory. Remote servers will implement the same methods through the agent (M11). The API covers policy CRUD, presets, per-domain enable/policy, service purge, and server stats. Changing a policy republishes every server that uses it.

## Consequences

- Cached objects are lost on restart. Acceptable for an edge cache. A disk tier is backlog work.
- Traefik-to-cache adds one local hop for cached domains only. Uncached domains route exactly as before.
- `X-Cache` is `HIT`, `MISS`, `STALE`, `EXPIRED`, `REVALIDATED`, or `BYPASS`. A request a rule sends to the cache counts as `MISS` even when the response was not storable.
- Purge by prefix or "all" scans the in-memory index. Fine for memory-sized stores; a disk tier would need its own index.
- Policy changes do not purge existing objects; they apply to new fills. Users purge from the service Cache tab when they need it immediately.
- Remote servers store cache settings, but the rules file reaches them only once the agent applies routing (M11).
