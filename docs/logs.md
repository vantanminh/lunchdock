# Logs and terminal

Rudder stores logs from every source in the panel's Postgres database, so search, live tail, and export work without a separate log stack.

| Source | Where it comes from |
| --- | --- |
| `app` | stdout and stderr of containers labelled `rudder.managed=true`: applications, databases, and compose stack services. |
| `proxy` | The JSON access log of the `rudder-traefik` container. The request host is matched against your domains, so proxy lines appear on the service they belong to. |
| `cache` | Output of the `rudder-cache` container. |
| `panel` | The control plane's own events at INFO and above. |
| `agent` | Lines shipped by remote agents. |
| `system` | Anything sent to the ingest API as `system`. |

The panel follows containers on its own server, checking for new ones every 10 seconds. After a restart it resumes from the last stored line of each container (up to an hour back). Environment variable values and database passwords are replaced with `***` before a line is stored.

Levels are taken from JSON fields (`level`, `severity`, …), from uppercase words such as `ERROR` or `WARN`, or from the HTTP status: 5xx is `error`, 4xx is `warn`.

## Searching

Open **Logs** in the sidebar, or the **Logs** tab of a service. Filter by source, level, HTTP status class, and time range, and type text to search messages. The search is case-insensitive, and `%` and `_` are matched literally. Click a line to see its structured fields. **Load older lines** pages back through the results.

**Live tail** opens a WebSocket and streams new lines that match the current filters. If the browser falls behind, it shows how many lines were skipped.

**Export NDJSON** and **Export CSV** download up to 100,000 matching lines. In the CSV, cells that start with `=`, `+`, `-`, or `@` are prefixed with `'` so spreadsheets do not run them as formulas.

## API

| Method | Path | Notes |
| --- | --- | --- |
| `GET` | `/api/v1/logs` | Query: `source`, `service_id`, `server_id`, `project_id`, `level` (comma list), `q`, `status` (`2xx`…`5xx`), `since`, `until` (RFC 3339, default the last 24 hours), `limit` (up to 1000). Results are newest first. Pass `next_before_time` and `next_before_id` back as `before_time` and `before_id` to get the next page. |
| `POST` | `/api/v1/logs/ingest` | `{"entries":[{"source":"app","message":"…","service_id":"…","level":"warn","timestamp":"…","fields":{}}]}`. Up to 1000 entries per request. Timestamps must fall within the last 30 days. Needs write permission. |
| `GET` | `/api/v1/logs/export` | Same filters, plus `format=ndjson` or `format=csv`. Streamed. |
| `GET` | `/api/v1/logs/tail` | WebSocket. Same filters. Messages are `{"type":"entry","entry":{…}}` or `{"type":"lagged","skipped":N}`. |
| `GET`/`PUT` | `/api/v1/settings/retention` | Admin only. `{"log_days":{"proxy":7}}`. |

Browsers must open WebSockets from the panel's own origin. Other clients can connect with an `Authorization: Bearer` token and no `Origin` header.

## Retention

Each source keeps its lines for its own number of days (default 14, maximum 365). Set this under **Logs → Retention**. An hourly job does three things:

- creates daily partitions up to two days ahead;
- deletes expired lines for each source;
- drops whole daily partitions (`log_entries_YYYYMMDD`) that are older than the longest retention.

## Terminal

The **Terminal** tab of a service opens an interactive shell in its running container. Rudder runs `bash -l` if the image has bash, otherwise `sh -l`, as the container's default user. For a compose stack, pick the compose service first. The terminal follows the browser window's size.

Only admins can open a terminal (an admin token needs the `write` scope). Every session is written to the audit log as `service.terminal`. Rudder does not record keystrokes or output.

## Limits

- Logs are an operational aid, not an audit trail. If the panel's in-memory queue (20,000 lines) fills up or the panel stops in the middle of a batch, lines can be lost.
- Docker reports times to the second when Rudder resumes following a container, so a few lines written in the same second as a restart can be skipped.
- Lines from remote servers arrive through the agent. The panel never connects to a remote Docker daemon to read logs.
