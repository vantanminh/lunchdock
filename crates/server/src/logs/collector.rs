//! Follows container output on the local server and feeds it into the log pipeline.
//!
//! Remote servers ship their own lines through the agent; this loop only
//! touches the local Docker daemon.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use rudder_core::{ContainerInfo, LogStreamKind, NodeDriver, CACHE_CONTAINER, TRAEFIK_CONTAINER};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use uuid::Uuid;

use super::NewLog;
use crate::AppState;

const TICK: Duration = Duration::from_secs(10);
/// A restarted panel resumes from the last stored line if it is this recent.
const RESUME_WINDOW_SECS: i64 = 3600;
/// A container seen for the first time is read from slightly before now.
const FIRST_SEEN_LOOKBACK_SECS: i64 = 15;

/// Host to (service, project), used to attribute proxy access logs.
type HostMap = Arc<RwLock<HashMap<String, (Uuid, Uuid)>>>;

#[derive(Debug, Clone)]
struct Target {
    source: &'static str,
    service_id: Option<Uuid>,
    project_id: Option<Uuid>,
    compose_service: Option<String>,
    secrets: Vec<String>,
}

pub async fn run(state: AppState) {
    let mut followed: HashMap<String, JoinHandle<()>> = HashMap::new();
    let hosts: HostMap = Arc::default();
    loop {
        if let Err(error) = tick(&state, &mut followed, &hosts).await {
            tracing::debug!(target: "rudder_server::logs", %error, "log collector tick failed");
        }
        tokio::time::sleep(TICK).await;
    }
}

async fn tick(
    state: &AppState,
    followed: &mut HashMap<String, JoinHandle<()>>,
    hosts: &HostMap,
) -> anyhow::Result<()> {
    let server_id = sqlx::query_scalar!("SELECT id FROM servers WHERE kind = 'local' LIMIT 1")
        .fetch_optional(&state.pool)
        .await?;
    let Some(server_id) = server_id else {
        return Ok(());
    };
    refresh_hosts(state, hosts).await?;
    followed.retain(|_, handle| !handle.is_finished());
    let containers = state.local_driver.list_containers().await?;
    let live: std::collections::HashSet<&str> = containers.iter().map(|c| c.id.as_str()).collect();
    followed.retain(|id, handle| {
        let keep = live.contains(id.as_str());
        if !keep {
            handle.abort();
        }
        keep
    });
    for container in &containers {
        if followed.contains_key(&container.id) {
            continue;
        }
        let Some(target) = resolve(state, container).await? else {
            continue;
        };
        let since = resume_point(state, server_id, &container.id).await?;
        let driver: Arc<dyn NodeDriver> = state.local_driver.clone();
        let sender = state.logs.sender();
        let task = follow(
            driver,
            sender,
            server_id,
            container.clone(),
            target,
            since,
            hosts.clone(),
        );
        followed.insert(container.id.clone(), tokio::spawn(task));
    }
    Ok(())
}

async fn refresh_hosts(state: &AppState, hosts: &HostMap) -> Result<(), sqlx::Error> {
    let rows = sqlx::query!(
        r#"
        SELECT d.host, s.id AS service_id, e.project_id
        FROM domains d
        JOIN services s ON s.id = d.service_id
        JOIN environments e ON e.id = s.environment_id
        "#
    )
    .fetch_all(&state.pool)
    .await?;
    let map = rows
        .into_iter()
        .map(|row| {
            (
                row.host.to_ascii_lowercase(),
                (row.service_id, row.project_id),
            )
        })
        .collect();
    if let Ok(mut slot) = hosts.write() {
        *slot = map;
    }
    Ok(())
}

async fn resolve(state: &AppState, container: &ContainerInfo) -> anyhow::Result<Option<Target>> {
    if container.name == TRAEFIK_CONTAINER {
        return Ok(Some(system_target("proxy")));
    }
    if container.name == CACHE_CONTAINER {
        return Ok(Some(system_target("cache")));
    }
    if container.labels.get("rudder.managed").map(String::as_str) != Some("true") {
        return Ok(None);
    }
    let service_label = container.labels.get("rudder.service");
    let row = if let Some(project) = container.labels.get("rudder.project") {
        sqlx::query!(
            r#"
            SELECT s.id, e.project_id
            FROM services s JOIN environments e ON e.id = s.environment_id
            WHERE s.kind = 'compose' AND s.spec->>'project' = $1
            "#,
            project
        )
        .fetch_optional(&state.pool)
        .await?
        .map(|r| (r.id, r.project_id))
    } else if let Some(id) = service_label.and_then(|v| Uuid::parse_str(v).ok()) {
        sqlx::query!(
            r#"
            SELECT s.id, e.project_id
            FROM services s JOIN environments e ON e.id = s.environment_id
            WHERE s.id = $1
            "#,
            id
        )
        .fetch_optional(&state.pool)
        .await?
        .map(|r| (r.id, r.project_id))
    } else {
        None
    };
    let Some((service_id, project_id)) = row else {
        return Ok(None);
    };
    Ok(Some(Target {
        source: "app",
        service_id: Some(service_id),
        project_id: Some(project_id),
        compose_service: container
            .labels
            .contains_key("rudder.project")
            .then(|| service_label.cloned())
            .flatten(),
        secrets: service_secrets(state, service_id).await,
    }))
}

fn system_target(source: &'static str) -> Target {
    Target {
        source,
        service_id: None,
        project_id: None,
        compose_service: None,
        secrets: Vec::new(),
    }
}

/// Values that must never reach the log store: env values and database credentials.
async fn service_secrets(state: &AppState, service_id: Uuid) -> Vec<String> {
    let mut out: Vec<String> = crate::stacks::load_env(state, service_id)
        .await
        .map(|env| env.into_iter().map(|(_, value)| value).collect())
        .unwrap_or_default();
    if let Ok(info) = crate::routes::databases::load_database(state, service_id).await {
        out.extend(info.secrets().into_iter().map(str::to_owned));
    }
    out.retain(|value| value.len() >= 4);
    out
}

async fn resume_point(
    state: &AppState,
    server_id: Uuid,
    container_id: &str,
) -> Result<DateTime<Utc>, sqlx::Error> {
    let last = sqlx::query_scalar!(
        r#"
        SELECT max(logged_at)
        FROM log_entries
        WHERE logged_at > now() - make_interval(secs => $3)
          AND server_id = $1 AND fields->>'container_id' = $2
        "#,
        server_id,
        container_id,
        RESUME_WINDOW_SECS as f64,
    )
    .fetch_one(&state.pool)
    .await?;
    Ok(last.unwrap_or_else(|| Utc::now() - chrono::Duration::seconds(FIRST_SEEN_LOOKBACK_SECS)))
}

async fn follow(
    driver: Arc<dyn NodeDriver>,
    sender: mpsc::Sender<NewLog>,
    server_id: Uuid,
    container: ContainerInfo,
    target: Target,
    since: DateTime<Utc>,
    hosts: HostMap,
) {
    let mut stream = match driver.follow_logs(&container.id, since.timestamp()).await {
        Ok(stream) => stream,
        Err(error) => {
            tracing::debug!(target: "rudder_server::logs", %error, container = %container.name, "follow failed");
            return;
        }
    };
    let secrets: Vec<&str> = target.secrets.iter().map(String::as_str).collect();
    while let Some(item) = stream.next().await {
        let Ok(line) = item else { break };
        let logged_at = line
            .timestamp
            .as_deref()
            .and_then(rudder_core::parse_docker_timestamp)
            .unwrap_or_else(Utc::now);
        if logged_at <= since || line.text.trim().is_empty() {
            continue;
        }
        let text = if secrets.is_empty() {
            line.text
        } else {
            rudder_core::redact(&line.text, &secrets)
        };
        let parsed = rudder_core::parse_line(&text);
        let mut fields = parsed.fields;
        fields.insert("container".into(), container.name.clone().into());
        fields.insert("container_id".into(), container.id.clone().into());
        let stream_name = match line.stream {
            LogStreamKind::Stdout => "stdout",
            LogStreamKind::Stderr => "stderr",
        };
        fields.insert("stream".into(), stream_name.into());
        if let Some(name) = &target.compose_service {
            fields.insert("compose_service".into(), name.clone().into());
        }
        let (mut service_id, mut project_id) = (target.service_id, target.project_id);
        if target.source == "proxy" {
            if let Some(host) = fields.get("RequestHost").and_then(|v| v.as_str()) {
                let host = host.split(':').next().unwrap_or(host).to_ascii_lowercase();
                if let Some((service, project)) =
                    hosts.read().ok().and_then(|map| map.get(&host).copied())
                {
                    service_id = Some(service);
                    project_id = Some(project);
                }
            }
        }
        let entry = NewLog {
            logged_at,
            server_id: Some(server_id),
            project_id,
            service_id,
            source: target.source.into(),
            level: parsed.level.into(),
            message: parsed.message,
            http_status: parsed.http_status,
            fields: serde_json::Value::Object(fields),
        };
        if sender.send(entry).await.is_err() {
            return;
        }
    }
}
