//! Server and service metrics every 10 seconds, alert evaluation, and daily Docker prune.

pub mod notify;
pub mod rollup;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use rudder_core::{
    percent, rate, step_alert, AlertState, AlertTransition, ContainerStats, HostStats, NodeDriver,
    SAMPLE_INTERVAL_SECS,
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

use crate::events::{self, Event};
use crate::AppState;

/// One metric value on its way to `metrics_samples`.
#[derive(Debug, Clone)]
pub struct Sample {
    pub server_id: Uuid,
    pub service_id: Option<Uuid>,
    pub source: &'static str,
    pub metric: &'static str,
    pub value: f64,
}

#[derive(Default)]
struct Counters {
    at: Option<Instant>,
    host: Option<(u64, u64)>,
    containers: HashMap<String, (u64, u64)>,
}

pub async fn run(state: AppState) {
    let mut counters = Counters::default();
    let mut ticker = tokio::time::interval(Duration::from_secs(SAMPLE_INTERVAL_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        if let Err(error) = sample_local(&state, &mut counters).await {
            tracing::debug!(%error, "metrics sample failed");
        }
        if let Err(error) = evaluate_alerts(&state.pool, Utc::now()).await {
            tracing::warn!(%error, "alert evaluation failed");
        }
        if let Err(error) = auto_prune(&state).await {
            tracing::warn!(%error, "auto prune failed");
        }
    }
}

async fn local_server(pool: &PgPool) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar!("SELECT id FROM servers WHERE kind = 'local' LIMIT 1")
        .fetch_optional(pool)
        .await
}

/// Compose project names and plain service ids, for attributing containers.
struct ServiceIndex {
    ids: HashSet<Uuid>,
    projects: HashMap<String, Uuid>,
}

async fn service_index(pool: &PgPool) -> Result<ServiceIndex, sqlx::Error> {
    let rows = sqlx::query!(r#"SELECT id, kind, spec->>'project' AS project FROM services"#)
        .fetch_all(pool)
        .await?;
    let mut index = ServiceIndex {
        ids: HashSet::new(),
        projects: HashMap::new(),
    };
    for row in rows {
        index.ids.insert(row.id);
        if row.kind == "compose" {
            if let Some(project) = row.project {
                index.projects.insert(project, row.id);
            }
        }
    }
    Ok(index)
}

fn container_service(index: &ServiceIndex, stats: &ContainerStats) -> Option<Uuid> {
    let label = |key: &str| {
        stats
            .labels
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    if let Some(project) = label("rudder.project") {
        return index.projects.get(project).copied();
    }
    label("rudder.service")
        .and_then(|value| Uuid::parse_str(value).ok())
        .filter(|id| index.ids.contains(id))
}

async fn sample_local(state: &AppState, counters: &mut Counters) -> anyhow::Result<()> {
    let Some(server_id) = local_server(&state.pool).await? else {
        return Ok(());
    };
    let driver = state.local_driver.clone();
    let host = driver.host_stats().await?;
    let containers = driver.container_stats().await.unwrap_or_default();
    let index = service_index(&state.pool).await?;
    let now = Instant::now();
    let elapsed = counters
        .at
        .map_or(0.0, |at| now.duration_since(at).as_secs_f64());
    counters.at = Some(now);
    let samples = build_samples(server_id, &host, &containers, &index, counters, elapsed);
    insert_samples(&state.pool, Utc::now(), &samples).await?;
    Ok(())
}

fn build_samples(
    server_id: Uuid,
    host: &HostStats,
    containers: &[ContainerStats],
    index: &ServiceIndex,
    counters: &mut Counters,
    elapsed: f64,
) -> Vec<Sample> {
    let mut out = host_samples(server_id, host, counters.host, elapsed);
    counters.host = Some((host.net_rx_bytes, host.net_tx_bytes));

    #[derive(Default)]
    struct Totals {
        cpu: f64,
        memory: u64,
        limit: u64,
        rx: f64,
        tx: f64,
        restarts: i64,
        containers: u32,
    }
    let mut per_service: HashMap<Uuid, Totals> = HashMap::new();
    let mut seen = HashMap::new();
    for stats in containers {
        let previous = counters.containers.get(&stats.id).copied();
        seen.insert(stats.id.clone(), (stats.net_rx_bytes, stats.net_tx_bytes));
        let Some(service_id) = container_service(index, stats) else {
            continue;
        };
        let totals = per_service.entry(service_id).or_default();
        totals.cpu += stats.cpu_percent;
        totals.memory += stats.memory_bytes;
        totals.limit += stats.memory_limit_bytes;
        totals.restarts += stats.restart_count;
        totals.containers += 1;
        if let Some((rx, tx)) = previous {
            totals.rx += rate(rx, stats.net_rx_bytes, elapsed);
            totals.tx += rate(tx, stats.net_tx_bytes, elapsed);
        }
    }
    counters.containers = seen;
    for (service_id, totals) in per_service {
        let mut push = |metric: &'static str, value: f64| {
            out.push(Sample {
                server_id,
                service_id: Some(service_id),
                source: "service",
                metric,
                value,
            })
        };
        push("cpu_percent", totals.cpu);
        push("memory_bytes", totals.memory as f64);
        if totals.limit > 0 && totals.limit < host.memory_total_bytes {
            push("memory_percent", percent(totals.memory, totals.limit));
        } else {
            push(
                "memory_percent",
                percent(totals.memory, host.memory_total_bytes),
            );
        }
        push("net_rx_bps", totals.rx);
        push("net_tx_bps", totals.tx);
        push("restarts", totals.restarts as f64);
        push("containers", f64::from(totals.containers));
    }
    out
}

/// Server metrics. `previous_net` is the last cumulative (rx, tx) reading.
pub fn host_samples(
    server_id: Uuid,
    host: &HostStats,
    previous_net: Option<(u64, u64)>,
    elapsed: f64,
) -> Vec<Sample> {
    let mut out = Vec::with_capacity(8);
    let mut push = |metric: &'static str, value: f64| {
        out.push(Sample {
            server_id,
            service_id: None,
            source: "server",
            metric,
            value,
        })
    };
    push("cpu_percent", host.cpu_percent);
    push("memory_percent", host.memory_percent());
    push("memory_used_bytes", host.memory_used_bytes as f64);
    push("disk_percent", host.disk_percent());
    push("disk_used_bytes", host.disk_used_bytes as f64);
    push("load1", host.load1);
    if let Some((rx, tx)) = previous_net {
        push("net_rx_bps", rate(rx, host.net_rx_bytes, elapsed));
        push("net_tx_bps", rate(tx, host.net_tx_bytes, elapsed));
    }
    out
}

pub async fn insert_samples(
    pool: &PgPool,
    at: DateTime<Utc>,
    samples: &[Sample],
) -> Result<(), sqlx::Error> {
    if samples.is_empty() {
        return Ok(());
    }
    let servers: Vec<Uuid> = samples.iter().map(|s| s.server_id).collect();
    let services: Vec<Option<Uuid>> = samples.iter().map(|s| s.service_id).collect();
    let sources: Vec<String> = samples.iter().map(|s| s.source.to_owned()).collect();
    let metrics: Vec<String> = samples.iter().map(|s| s.metric.to_owned()).collect();
    let values: Vec<f64> = samples
        .iter()
        .map(|s| if s.value.is_finite() { s.value } else { 0.0 })
        .collect();
    sqlx::query!(
        r#"
        INSERT INTO metrics_samples (sampled_at, server_id, service_id, source, metric, value)
        SELECT $1, * FROM UNNEST($2::uuid[], $3::uuid[], $4::text[], $5::text[], $6::float8[])
        "#,
        at,
        &servers,
        &services as &[Option<Uuid>],
        &sources,
        &metrics,
        &values,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// How far back threshold alerts average samples, to smooth single spikes.
const ALERT_WINDOW_SECS: f64 = 30.0;

struct AlertRow {
    id: Uuid,
    name: String,
    kind: String,
    target_spec: serde_json::Value,
    threshold: Option<f64>,
    duration_secs: i32,
    state: String,
    state_since: DateTime<Utc>,
}

fn target(spec: &serde_json::Value, key: &str) -> Option<Uuid> {
    spec.get(key)
        .and_then(|v| v.as_str())
        .and_then(|v| Uuid::parse_str(v).ok())
}

/// Current value of an alert's signal and whether it breaches. `None` means no data.
async fn observe(pool: &PgPool, alert: &AlertRow) -> Result<Option<(f64, bool)>, sqlx::Error> {
    let threshold = alert.threshold.unwrap_or(0.0);
    let spec = &alert.target_spec;
    let average = |server: Option<Uuid>, service: Option<Uuid>, metric: &'static str| async move {
        sqlx::query_scalar!(
            r#"
            SELECT avg(value) AS "avg"
            FROM metrics_samples
            WHERE sampled_at > now() - make_interval(secs => $4)
              AND metric = $3
              AND (($1::uuid IS NOT NULL AND server_id = $1 AND service_id IS NULL)
                OR ($2::uuid IS NOT NULL AND service_id = $2))
            "#,
            server,
            service,
            metric,
            ALERT_WINDOW_SECS,
        )
        .fetch_one(pool)
        .await
    };
    let value = match alert.kind.as_str() {
        "server_cpu" | "server_memory" | "server_disk" => {
            let metric = match alert.kind.as_str() {
                "server_cpu" => "cpu_percent",
                "server_memory" => "memory_percent",
                _ => "disk_percent",
            };
            average(target(spec, "server_id"), None, metric).await?
        }
        "service_cpu" => average(None, target(spec, "service_id"), "cpu_percent").await?,
        "service_memory" => average(None, target(spec, "service_id"), "memory_bytes")
            .await?
            .map(|bytes| bytes / (1024.0 * 1024.0)),
        "service_down" => {
            let Some(service_id) = target(spec, "service_id") else {
                return Ok(None);
            };
            let expected = sqlx::query_scalar!(
                "SELECT status = 'running' AS \"running!\" FROM services WHERE id = $1",
                service_id
            )
            .fetch_optional(pool)
            .await?
            .unwrap_or(false);
            if !expected {
                return Ok(Some((1.0, false)));
            }
            let up = average(None, Some(service_id), "containers")
                .await?
                .unwrap_or(0.0);
            return Ok(Some((up, up < 1.0)));
        }
        "http_errors" => {
            let Some(service_id) = target(spec, "service_id") else {
                return Ok(None);
            };
            let window = f64::from(alert.duration_secs.max(60));
            let count = sqlx::query_scalar!(
                r#"
                SELECT count(*) AS "count!"
                FROM log_entries
                WHERE source = 'proxy' AND service_id = $1 AND http_status >= 500
                  AND logged_at > now() - make_interval(secs => $2)
                "#,
                service_id,
                window,
            )
            .fetch_one(pool)
            .await?;
            return Ok(Some((count as f64, count as f64 > threshold)));
        }
        "server_offline" => {
            let Some(server_id) = target(spec, "server_id") else {
                return Ok(None);
            };
            let row = sqlx::query!(
                "SELECT kind, last_heartbeat_at FROM servers WHERE id = $1",
                server_id
            )
            .fetch_optional(pool)
            .await?;
            let Some(row) = row else { return Ok(None) };
            if row.kind == "local" {
                return Ok(Some((0.0, false)));
            }
            let age = row
                .last_heartbeat_at
                .map_or(f64::INFINITY, |at| (Utc::now() - at).num_seconds() as f64);
            let limit = f64::from(alert.duration_secs.max(30));
            return Ok(Some((age.min(1e9), age > limit)));
        }
        _ => return Ok(None),
    };
    Ok(value.map(|v| (v, v > threshold)))
}

/// Advance every enabled alert once and record fired/resolved events.
pub async fn evaluate_alerts(pool: &PgPool, now: DateTime<Utc>) -> Result<(), sqlx::Error> {
    let alerts = sqlx::query_as!(
        AlertRow,
        r#"
        SELECT id, name, kind, target_spec, threshold, duration_secs, state, state_since
        FROM alerts
        WHERE enabled AND kind <> 'event'
        "#
    )
    .fetch_all(pool)
    .await?;
    for alert in alerts {
        let Some((value, breached)) = observe(pool, &alert).await? else {
            continue;
        };
        let duration = match alert.kind.as_str() {
            "http_errors" | "server_offline" => 0,
            _ => i64::from(alert.duration_secs),
        };
        let current = AlertState::parse(&alert.state);
        let (next, transition) = step_alert(current, alert.state_since, breached, duration, now);
        let since = if next == current {
            alert.state_since
        } else {
            now
        };
        sqlx::query!(
            r#"
            UPDATE alerts
            SET state = $2, state_since = $3, last_value = $4,
                last_fired_at = CASE WHEN $5 THEN $3 ELSE last_fired_at END
            WHERE id = $1
            "#,
            alert.id,
            next.as_str(),
            since,
            value,
            transition == AlertTransition::Fired,
        )
        .execute(pool)
        .await?;
        let event = match transition {
            AlertTransition::None => continue,
            AlertTransition::Fired => {
                Event::critical("alert.fired", format!("Alert firing: {}", alert.name))
            }
            AlertTransition::Resolved => {
                Event::info("alert.resolved", format!("Alert resolved: {}", alert.name))
            }
        };
        let mut event = event
            .message(describe(&alert, value))
            .payload(json!({ "alert_id": alert.id, "kind": alert.kind, "value": value }));
        if let Some(server) = target(&alert.target_spec, "server_id") {
            event = event.server(server);
        }
        if let Some(service) = target(&alert.target_spec, "service_id") {
            event = event.service(service);
        }
        events::record(pool, event).await;
    }
    Ok(())
}

fn describe(alert: &AlertRow, value: f64) -> String {
    let threshold = alert.threshold.unwrap_or(0.0);
    match alert.kind.as_str() {
        "server_cpu" | "server_memory" | "server_disk" | "service_cpu" => {
            format!("{value:.1}% (threshold {threshold:.0}%)")
        }
        "service_memory" => format!("{value:.0} MiB (threshold {threshold:.0} MiB)"),
        "service_down" => "no running container".to_owned(),
        "http_errors" => format!("{value:.0} HTTP 5xx responses (threshold {threshold:.0})"),
        "server_offline" => format!("last seen {value:.0}s ago"),
        _ => format!("{value}"),
    }
}

async fn auto_prune(state: &AppState) -> anyhow::Result<()> {
    let due = sqlx::query_scalar!(
        r#"
        SELECT id FROM servers
        WHERE auto_prune AND (last_pruned_at IS NULL OR last_pruned_at < now() - INTERVAL '1 day')
        "#
    )
    .fetch_all(&state.pool)
    .await?;
    for server_id in due {
        let Ok(driver) = crate::nodes::driver_for(state, server_id).await else {
            continue;
        };
        prune_server(&state.pool, driver.as_ref(), server_id, "schedule").await?;
    }
    Ok(())
}

/// Prune one server, record the time and an event. Returns reclaimed bytes.
pub async fn prune_server(
    pool: &PgPool,
    driver: &dyn NodeDriver,
    server_id: Uuid,
    trigger: &str,
) -> Result<u64, sqlx::Error> {
    sqlx::query!(
        "UPDATE servers SET last_pruned_at = now() WHERE id = $1",
        server_id
    )
    .execute(pool)
    .await?;
    match driver.prune().await {
        Ok(bytes) => {
            events::record(
                pool,
                Event::info("docker.pruned", "Docker images and build cache pruned")
                    .server(server_id)
                    .message(format!("{bytes} bytes reclaimed"))
                    .payload(json!({ "bytes": bytes, "trigger": trigger })),
            )
            .await;
            Ok(bytes)
        }
        Err(error) => {
            events::record(
                pool,
                Event::warning("docker.prune_failed", "Docker prune failed")
                    .server(server_id)
                    .message(error.to_string()),
            )
            .await;
            Ok(0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_aggregate_per_service_and_compute_rates() {
        let server = Uuid::new_v4();
        let app = Uuid::new_v4();
        let stack = Uuid::new_v4();
        let index = ServiceIndex {
            ids: HashSet::from([app, stack]),
            projects: HashMap::from([("shop-abc123".to_owned(), stack)]),
        };
        let host = HostStats {
            cpu_percent: 12.5,
            cpu_count: 2,
            memory_total_bytes: 1000,
            memory_used_bytes: 250,
            disk_total_bytes: 100,
            disk_used_bytes: 40,
            load1: 0.5,
            net_rx_bytes: 2000,
            net_tx_bytes: 4000,
        };
        let container = |id: &str, labels: Vec<(&str, String)>, rx: u64| ContainerStats {
            id: id.into(),
            name: id.into(),
            cpu_percent: 5.0,
            memory_bytes: 100,
            memory_limit_bytes: 0,
            net_rx_bytes: rx,
            net_tx_bytes: 0,
            block_read_bytes: 0,
            block_write_bytes: 0,
            restart_count: 1,
            labels: labels.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        };
        let containers = vec![
            container("a", vec![("rudder.service", app.to_string())], 100),
            container(
                "b",
                vec![
                    ("rudder.project", "shop-abc123".into()),
                    ("rudder.service", "web".into()),
                ],
                0,
            ),
            container(
                "c",
                vec![
                    ("rudder.project", "shop-abc123".into()),
                    ("rudder.service", "db".into()),
                ],
                0,
            ),
            container(
                "stray",
                vec![("rudder.service", Uuid::new_v4().to_string())],
                0,
            ),
        ];
        let mut counters = Counters {
            at: None,
            host: Some((1000, 1000)),
            containers: HashMap::from([("a".to_owned(), (0, 0))]),
        };
        let samples = build_samples(server, &host, &containers, &index, &mut counters, 10.0);
        let find = |service: Option<Uuid>, metric: &str| {
            samples
                .iter()
                .find(|s| s.service_id == service && s.metric == metric)
                .map(|s| s.value)
        };
        assert_eq!(find(None, "cpu_percent"), Some(12.5));
        assert_eq!(find(None, "memory_percent"), Some(25.0));
        assert_eq!(find(None, "disk_percent"), Some(40.0));
        assert_eq!(find(None, "net_rx_bps"), Some(100.0));
        assert_eq!(find(None, "net_tx_bps"), Some(300.0));
        assert_eq!(find(Some(app), "net_rx_bps"), Some(10.0));
        assert_eq!(find(Some(stack), "cpu_percent"), Some(10.0));
        assert_eq!(find(Some(stack), "containers"), Some(2.0));
        assert_eq!(find(Some(stack), "memory_percent"), Some(20.0));
        assert_eq!(
            samples.iter().filter(|s| s.service_id.is_some()).count(),
            14
        );
        assert_eq!(counters.containers.len(), 4);
    }
}
