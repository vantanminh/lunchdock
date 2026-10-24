# HTTP cache

Each server can run one `rudder-cache` process: a Pingora reverse proxy with an in-memory store. Traefik still terminates TLS. Domains with caching turned on are routed to the cache, and the cache forwards misses to the app container. Domains without caching skip it entirely.

```
client ─TLS─▶ Traefik ─▶ rudder-cache:8080 ─miss─▶ app container:port
                    └──── (cache off) ─────────────▶ app container:port
```

## Turning it on

1. Open a service, then the **Cache** tab.
2. Pick a policy for the domain, or keep the default "Honor origin headers".
3. Click **Turn on**. The panel rewrites Traefik's routes and `cache/rules.json`. The cache picks up the change within a second.

Responses carry `X-Cache: HIT | MISS | STALE | EXPIRED | REVALIDATED | BYPASS`.

## Policies

Manage policies under **Cache** in the sidebar. A policy is an ordered list of rules. The first rule whose match fits the request decides what happens; a request no rule matches goes to the origin uncached.

| Match field | Meaning |
| --- | --- |
| `path_prefixes` | Path starts with one of these. |
| `extensions` | File extension, without the dot, case-insensitive. |
| `path_glob` | `*` matches any run of characters, anchored at both ends. |

| Action field | Default | Meaning |
| --- | --- | --- |
| `mode` | `cache` | `cache` or `bypass`. |
| `ttl_secs` | 300 | Freshness when the origin sends none, or always when `respect_origin` is false. |
| `respect_origin` | true | Use `s-maxage`/`max-age`; never store `no-store`, `private`, or `no-cache` responses. |
| `stale_while_revalidate_secs` | 0 | Serve the stale copy while one request refreshes it in the background. |
| `stale_if_error_secs` | 0 | Serve the stale copy when the origin fails. |
| `ignore_query` | false | Drop the query string from the key. |
| `query_keys` | [] | When set, only these query parameters are part of the key. |
| `bypass_cookies` | [] | Requests with a cookie whose name starts with one of these skip the cache. |
| `statuses` | 200, 203, 204, 300, 301, 308, 404, 410 | Statuses that may be stored. |

The following always apply:

- Only `GET` requests are looked up.
- Requests with `Authorization` bypass the cache.
- Responses with `Set-Cookie`, or with `Vary` on anything other than `Accept-Encoding`, are never stored.
- Query parameters are sorted, so `?b=2&a=1` and `?a=1&b=2` share an object.
- Brotli, gzip, and uncompressed variants are stored separately.

### Presets

| Preset | What it does |
| --- | --- |
| Static assets | CSS, JS, images, fonts, and media for 7 days; everything else uncached. |
| Honor origin headers | Cache only what the origin marks cacheable. |
| WordPress | Skips `/wp-admin`, login, REST, cart, checkout, and logged-in/WooCommerce cookies. Static files 30 days, pages 5 minutes with stale windows. |
| Ghost | Skips `/ghost`, members, and signed-in cookies. Static files 7 days, pages 10 minutes. |
| Next.js | `/_next/static` for a year, `/_next/image` for a day keyed by `url`, `w`, `q`; pages as the origin says. |
| API micro-cache | 5-second cache for `200` GET responses; session cookies skip it. |

### Memory

The store holds whole objects in RAM. The server's budget starts as the sum of the memory budgets set on the policies in use, or 256 MiB when none sets one. It is then limited to a share of the host (15% by default, from 1% to 40%) and to 384 MiB unless you set a higher ceiling. One object is never larger than that budget, and a response bigger than 32 MiB is not stored. When the store is full, the least recently used objects are evicted down to 90% of the budget. Restarting the cache empties the memory store. Objects stay on disk when a policy sets a disk budget.

**Operate → Cache** shows the cap in effect, the hit ratio, and how much memory the cache is using. Readings are kept for 14 days.

New installs limit the `rudder-cache` container to `RUDDER_CACHE_MEMORY` (default `512m`) so it is stopped before it can fill the machine. Re-run `install.sh` on an existing machine to apply that limit. If you raise the panel ceiling above the container limit, raise `RUDDER_CACHE_MEMORY` too and run the installer again.

## Purge

From the Cache tab you can purge:

- exact paths, including the query string (all encodings of a path are removed);
- one prefix, written as `/blog/*`;
- everything for the service's cached domains.

API:

```bash
curl -X POST https://panel.example.com/api/v1/services/$SERVICE/cache/purge \
  -H "Authorization: Bearer $TOKEN" -H 'content-type: application/json' \
  -d '{"paths": ["/", "/blog/?page=2"]}'
```

The token needs the `write` scope.

## Stats and metrics

- **Service Cache tab:** hit ratio, hits, misses, bypasses, objects, and bytes per host.
- **Cache page:** totals per server. `GET /api/v1/servers/{id}/cache/stats` returns the same data.

The cache's admin listener also serves Prometheus metrics at `GET /metrics` (bearer token from `cache/admin.token`):

- `rudder_cache_requests_total{host,outcome}`
- `rudder_cache_served_bytes_total{host}`
- `rudder_cache_host_objects{host}`, `rudder_cache_host_stored_bytes{host}`
- `rudder_cache_objects`, `rudder_cache_stored_bytes`, `rudder_cache_memory_max_bytes`
- `rudder_cache_evictions_total`, `rudder_cache_reloads_total`

## Running the cache

```
rudder-cache --rules /var/lib/rudder/cache/rules.json \
  [--listen 0.0.0.0:8080] [--admin-listen 127.0.0.1:9090] \
  [--token-file /var/lib/rudder/cache/admin.token] [--memory-bytes N]
```

The installer runs it as the `rudder-cache` container on the `rudder-proxy` network with the data directory's `cache/` folder mounted. The panel reaches the admin listener at `RUDDER_CACHE_ADMIN_URL` (default `http://127.0.0.1:9090`). The admin port is never published to the internet; every endpoint except `/health` requires the token.

| Admin endpoint | Purpose |
| --- | --- |
| `GET /health` | Liveness, no auth. |
| `GET /stats` | JSON counters (`rudder_core::CacheStats`). |
| `GET /metrics` | Prometheus text. |
| `POST /purge` | `{"host": "...", "paths": [...]}`, `{"host": "...", "prefix": "/x"}`, or `{"host": "...", "all": true}`. |
| `POST /reload` | Re-read the rules file now; `422` if it is invalid (the previous rules stay). |

## Limits

- **Disk tier.** Set a disk budget on a cache policy (blank means memory only). The budgets of the policies in use are added together. Objects are written under `cache/disk` on that server and are still there after the cache process restarts. The hottest objects are also kept in memory. A disk budget of zero, or leaving every policy blank, stores nothing on disk and drops files left from an earlier budget. Purge removes the disk copies too.
- Policy changes apply to new fills. Purge to drop objects stored under the old policy.
- On remote servers the rules and routes are stored in the panel, but they only reach the server once the agent applies them (M11).
