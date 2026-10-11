---
created_at: "2026-10-11T06:45:35.943265864+00:00"
doc: docs/decisions/0014-monitoring-and-alerts.md
id: "0014"
notes: null
status: accepted
title: Monitoring in Postgres with watermarked rollups and an outbox for notifications
type: decision
updated_at: "2026-10-11T06:45:35.943266346+00:00"
verify: null
---

# Monitoring in Postgres with watermarked rollups and an outbox for notifications

## Context

M10 needs 10-second server and service metrics, charts from 15 minutes to 30 days, alert
rules, and notifications to Telegram, Discord, Slack, webhooks, and email. Everything has
to fit in the control plane's memory budget (under 60 MB idle) on one small VPS.
Prometheus, Grafana, or Alertmanager next to the panel would cost more memory than the
whole panel, and each would be another stateful service to back up.

## Decision

- Samples go into the existing day-partitioned `metrics_samples` table, and rollups into
  `metrics_rollups` (`1m`, `5m`, `1h`, each with avg/min/max and the sample count). Server
  stats are read from `/proc` (`RUDDER_HOST_PROC`, `/host/proc` in the container) and
  `statvfs`. Container stats come from the Docker stats API. Both go through new
  `NodeDriver::host_stats` and the existing `container_stats`, so remote servers can
  supply the same data through the agent in M11.
- One loop every 10 s samples the local server, evaluates alerts, and runs the daily
  auto-prune. A separate minute loop does rollups. Each resolution keeps a watermark in
  `metrics_watermarks` and only processes whole buckets (20 s slack for raw data). A
  Postgres advisory lock serializes runs, so restarts and two panels never double-count.
  5m and 1h are built from the next finer level with `date_bin`, weighted by sample count.
  Proxy `requests` and `http_5xx` come from the Traefik access log in `log_entries` and are
  kept as per-minute rates at every level.
- The chart API picks the resolution from the range: raw up to 1 h, 1m up to 6 h, 5m up to
  48 h, otherwise 1h.
- Alerts use a pure state machine (`rudder_core::step_alert`): `ok`, then `pending` once a
  30 s average breaches, then `firing` after the configured duration, and back to `ok` when
  it clears. Fired and resolved transitions are written to `system_events`, as are deploy,
  build, and backup events.
- `system_events` is the notification outbox. A delivery loop every 5 s sends undelivered
  events to the alert's channels plus any `event` rules whose patterns and minimum severity
  match. Per-channel success is kept in `delivered_channels`, so a retry goes only to the
  channels that failed, up to 5 attempts. Channel configs are sealed with the master key
  `Cipher`. The API shows only a summary, and empty secret fields on update keep the stored
  value. HTTP channels use reqwest (rustls, 15 s timeout, no redirects, HTTPS for
  Discord/Slack). Webhooks are signed with HMAC-SHA256 in `X-Rudder-Signature`. SMTP uses
  lettre with rustls and STARTTLS, implicit TLS, or plain for local relays.
- Pruning removes dangling images and the build cache only. It never touches volumes or
  tagged images.

## Consequences

- No new services or memory-heavy dependencies. Metrics are covered by the same Postgres
  backup as everything else.
- At 10 s with about 10 metrics per service, a host with 30 services writes roughly
  2.6 M raw rows a day. With the default 24 h raw retention and partition drops, the raw
  table stays bounded. Longer ranges read rollups only.
- Alert evaluation is per panel tick, so alert latency is 10 s plus the rule duration.
  Evaluation runs on the panel even for remote servers, using samples the agent ships.
- Notification delivery is at-least-once per channel. A crash between send and record can
  send a duplicate, and receivers can deduplicate by the event `id`.
- Channels that need inbound connectivity, such as Telegram, Discord, or Slack, cannot be
  tested from an air-gapped panel. Webhook and SMTP work inside private networks.
