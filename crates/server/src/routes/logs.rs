//! Central log search, ingest, export, live tail, retention, and the container terminal.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use bytes::Bytes;
use chrono::{DateTime, Duration, Utc};
use futures_util::{SinkExt, StreamExt};
use rudder_core::{TerminalInput, LOG_LEVELS, LOG_SOURCES, MAX_LOG_MESSAGE};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::logs::retention::{self, MetricsRetention, RetentionSettings, MAX_LOG_DAYS};
use crate::logs::{insert_batch, search, LogFilter, LogView, NewLog, Page};
use crate::nodes::driver_for;
use crate::state::AppState;

use super::catalog::{bad, core_error, not_found};

pub const MAX_PAGE: i64 = 1000;
pub const MAX_INGEST: usize = 1000;
pub const MAX_EXPORT: usize = 100_000;

#[derive(Debug, Default, Deserialize)]
pub struct LogQuery {
    pub source: Option<String>,
    pub service_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    /// Comma-separated levels.
    pub level: Option<String>,
    /// Case-insensitive substring of the message.
    pub q: Option<String>,
    /// `2xx`, `3xx`, `4xx`, or `5xx`.
    pub status: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub before_time: Option<DateTime<Utc>>,
    pub before_id: Option<i64>,
    pub limit: Option<i64>,
    /// Export format: `ndjson` (default) or `csv`.
    pub format: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct LogPage {
    /// Newest first.
    pub entries: Vec<LogView>,
    /// Pass as `before_time` and `before_id` to fetch older lines.
    pub next_before_time: Option<DateTime<Utc>>,
    pub next_before_id: Option<i64>,
}

#[derive(Deserialize, ToSchema)]
pub struct IngestEntry {
    /// Defaults to now. Must be within the last 30 days.
    pub timestamp: Option<DateTime<Utc>>,
    pub source: String,
    /// Detected from the message when omitted.
    pub level: Option<String>,
    pub message: String,
    pub server_id: Option<Uuid>,
    /// The project is filled in from the service.
    pub service_id: Option<Uuid>,
    pub http_status: Option<i32>,
    #[schema(value_type = Option<Object>)]
    pub fields: Option<serde_json::Map<String, serde_json::Value>>,
}

#[derive(Deserialize, ToSchema)]
pub struct IngestRequest {
    pub entries: Vec<IngestEntry>,
}

#[derive(Serialize, ToSchema)]
pub struct IngestResult {
    pub accepted: usize,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateRetention {
    /// Days to keep per source, 1 to 365. Sources not listed keep their value.
    #[serde(default)]
    pub log_days: HashMap<String, u32>,
    /// Replaces the metric retention when present.
    pub metrics: Option<MetricsRetention>,
}

#[derive(Debug, Default, Deserialize)]
pub struct TerminalQuery {
    pub cols: Option<u16>,
    pub rows: Option<u16>,
    /// Compose service name, required for stacks.
    pub container: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum TerminalMessage {
    Input { data: String },
    Resize { cols: u16, rows: u16 },
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/logs", get(list_logs))
        .route("/api/v1/logs/ingest", post(ingest))
        .route("/api/v1/logs/export", get(export))
        .route("/api/v1/logs/tail", get(tail))
        .route(
            "/api/v1/settings/retention",
            get(get_retention).put(update_retention),
        )
        .route("/api/v1/services/{id}/terminal", get(terminal))
}

fn parse_filter(query: &LogQuery) -> Result<LogFilter, ApiError> {
    if let Some(source) = &query.source {
        rudder_core::validate_log_source(source).map_err(core_error)?;
    }
    let mut levels = Vec::new();
    for level in query.level.iter().flat_map(|l| l.split(',')) {
        let level = level.trim();
        if level.is_empty() {
            continue;
        }
        if !LOG_LEVELS.contains(&level) {
            return Err(bad(
                "invalid_level",
                format!("level must be one of {}", LOG_LEVELS.join(", ")),
            ));
        }
        levels.push(level.to_owned());
    }
    let (status_min, status_max) = match query.status.as_deref() {
        None | Some("") => (None, None),
        Some(class @ ("1xx" | "2xx" | "3xx" | "4xx" | "5xx")) => {
            let base = i32::from(class.as_bytes()[0] - b'0') * 100;
            (Some(base), Some(base + 99))
        }
        Some(_) => return Err(bad("invalid_status", "status must be 1xx to 5xx")),
    };
    let search = query
        .q
        .as_ref()
        .map(|q| q.trim().to_owned())
        .filter(|q| !q.is_empty());
    if search.as_ref().is_some_and(|q| q.len() > 500) {
        return Err(bad("invalid_query", "search text is limited to 500 bytes"));
    }
    Ok(LogFilter {
        source: query.source.clone(),
        service_id: query.service_id,
        server_id: query.server_id,
        project_id: query.project_id,
        levels,
        search,
        status_min,
        status_max,
    })
}

fn parse_page(query: &LogQuery, default_limit: i64) -> Result<Page, ApiError> {
    let until = query
        .until
        .unwrap_or_else(|| Utc::now() + Duration::minutes(5));
    let since = query.since.unwrap_or(until - Duration::hours(24));
    if since > until {
        return Err(bad("invalid_range", "since must be before until"));
    }
    let before = match (query.before_time, query.before_id) {
        (Some(time), Some(id)) => Some((time, id)),
        (None, None) => None,
        _ => {
            return Err(bad(
                "invalid_cursor",
                "before_time and before_id go together",
            ))
        }
    };
    Ok(Page {
        since,
        until,
        before,
        limit: query.limit.unwrap_or(default_limit).clamp(1, MAX_PAGE),
    })
}

async fn list_logs(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(query): Query<LogQuery>,
) -> Result<Json<LogPage>, ApiError> {
    let filter = parse_filter(&query)?;
    let page = parse_page(&query, 200)?;
    let entries = search(&state.pool, &filter, &page)
        .await
        .map_err(ApiError::database)?;
    let full = entries.len() as i64 == page.limit;
    let last = entries.last().filter(|_| full);
    Ok(Json(LogPage {
        next_before_time: last.map(|row| row.logged_at),
        next_before_id: last.map(|row| row.id),
        entries,
    }))
}

async fn ingest(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<IngestRequest>,
) -> Result<Json<IngestResult>, ApiError> {
    auth.require_write()?;
    if body.entries.len() > MAX_INGEST {
        return Err(bad(
            "too_many",
            format!("at most {MAX_INGEST} entries per request"),
        ));
    }
    let now = Utc::now();
    let mut services: HashMap<Uuid, Uuid> = HashMap::new();
    let mut rows = Vec::with_capacity(body.entries.len());
    for entry in body.entries {
        rudder_core::validate_log_source(&entry.source).map_err(core_error)?;
        let logged_at = entry.timestamp.unwrap_or(now);
        if logged_at < now - Duration::days(30) || logged_at > now + Duration::minutes(5) {
            return Err(bad(
                "invalid_timestamp",
                "timestamps must be within the last 30 days",
            ));
        }
        if entry.message.len() > MAX_LOG_MESSAGE * 4 {
            return Err(bad("too_large", "message is too long"));
        }
        let parsed = rudder_core::parse_line(&entry.message);
        let level = match entry.level.as_deref() {
            Some(level) => rudder_core::normalize_level(level)
                .ok_or_else(|| bad("invalid_level", "unknown level"))?,
            None => parsed.level,
        };
        if entry.http_status.is_some_and(|s| !(100..600).contains(&s)) {
            return Err(bad("invalid_status", "http_status must be 100 to 599"));
        }
        let project_id = match entry.service_id {
            Some(id) => Some(match services.get(&id) {
                Some(project) => *project,
                None => {
                    let project = service_project(&state, id).await?;
                    services.insert(id, project);
                    project
                }
            }),
            None => None,
        };
        let mut fields = parsed.fields;
        if let Some(extra) = entry.fields {
            fields.extend(extra);
        }
        let fields = serde_json::Value::Object(fields);
        if fields.to_string().len() > 16 * 1024 {
            return Err(bad("too_large", "fields are limited to 16 KiB"));
        }
        rows.push(NewLog {
            logged_at,
            server_id: entry.server_id,
            project_id,
            service_id: entry.service_id,
            source: entry.source,
            level: level.to_owned(),
            message: parsed.message,
            http_status: entry.http_status.or(parsed.http_status),
            fields,
        });
    }
    let inserted = insert_batch(&state.pool, &rows).await.map_err(|error| {
        if matches!(&error, sqlx::Error::Database(db) if db.is_foreign_key_violation()) {
            bad("unknown_server", "server_id does not exist")
        } else {
            ApiError::database(error)
        }
    })?;
    let accepted = inserted.len();
    state.logs.publish(inserted);
    Ok(Json(IngestResult { accepted }))
}

async fn service_project(state: &AppState, service_id: Uuid) -> Result<Uuid, ApiError> {
    sqlx::query_scalar!(
        r#"
        SELECT e.project_id
        FROM services s JOIN environments e ON e.id = s.environment_id
        WHERE s.id = $1
        "#,
        service_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| bad("unknown_service", "service_id does not exist"))
}

fn csv_cell(value: &str) -> String {
    let value = match value.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{value}"),
        _ => value.to_owned(),
    };
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn export_line(row: &LogView, csv: bool) -> String {
    if !csv {
        let mut line = serde_json::to_string(row).unwrap_or_default();
        line.push('\n');
        return line;
    }
    let opt = |id: Option<Uuid>| id.map(|id| id.to_string()).unwrap_or_default();
    format!(
        "{},{},{},{},{},{},{},{}\n",
        row.logged_at.to_rfc3339(),
        row.source,
        row.level,
        row.http_status.map(|s| s.to_string()).unwrap_or_default(),
        opt(row.server_id),
        opt(row.project_id),
        opt(row.service_id),
        csv_cell(&row.message),
    )
}

struct ExportCursor {
    pool: sqlx::PgPool,
    filter: LogFilter,
    page: Page,
    csv: bool,
    sent: usize,
    header: bool,
    done: bool,
}

async fn export(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(query): Query<LogQuery>,
) -> Result<Response, ApiError> {
    let filter = parse_filter(&query)?;
    let mut page = parse_page(&query, MAX_PAGE)?;
    page.limit = MAX_PAGE;
    let csv = match query.format.as_deref() {
        None | Some("ndjson") => false,
        Some("csv") => true,
        Some(_) => return Err(bad("invalid_format", "format must be ndjson or csv")),
    };
    let cursor = ExportCursor {
        pool: state.pool.clone(),
        filter,
        page,
        csv,
        sent: 0,
        header: csv,
        done: false,
    };
    let stream = futures_util::stream::unfold(cursor, |mut cursor| async move {
        if cursor.done {
            return None;
        }
        let mut chunk = String::new();
        if cursor.header {
            chunk.push_str(
                "logged_at,source,level,http_status,server_id,project_id,service_id,message\n",
            );
            cursor.header = false;
        }
        match search(&cursor.pool, &cursor.filter, &cursor.page).await {
            Ok(rows) => {
                let room = MAX_EXPORT - cursor.sent;
                for row in rows.iter().take(room) {
                    chunk.push_str(&export_line(row, cursor.csv));
                }
                cursor.sent += rows.len().min(room);
                if (rows.len() as i64) < cursor.page.limit || cursor.sent >= MAX_EXPORT {
                    cursor.done = true;
                }
                cursor.page.before = rows.last().map(|row| (row.logged_at, row.id));
            }
            Err(error) => {
                tracing::warn!(%error, "log export query failed");
                cursor.done = true;
                return Some((Err(std::io::Error::other("export failed")), cursor));
            }
        }
        Some((Ok::<_, std::io::Error>(Bytes::from(chunk)), cursor))
    });
    let (content_type, name) = if csv {
        ("text/csv; charset=utf-8", "rudder-logs.csv")
    } else {
        ("application/x-ndjson", "rudder-logs.ndjson")
    };
    Ok((
        [
            (header::CONTENT_TYPE, content_type.to_owned()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{name}\""),
            ),
        ],
        Body::from_stream(stream),
    )
        .into_response())
}

/// Browsers send `Origin` on WebSocket upgrades; it must match the panel host.
fn same_origin(headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let origin_host = origin
        .split_once("://")
        .map_or(origin, |(_, rest)| rest)
        .trim_end_matches('/');
    origin_host.eq_ignore_ascii_case(host)
}

fn forbidden_origin() -> ApiError {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "bad_origin",
        "cross-origin WebSocket requests are not allowed",
    )
}

async fn tail(
    State(state): State<AppState>,
    _auth: AuthUser,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    if !same_origin(&headers) {
        return Err(forbidden_origin());
    }
    let filter = parse_filter(&query)?;
    let receiver = state.logs.subscribe();
    Ok(upgrade.on_upgrade(move |socket| run_tail(socket, receiver, filter)))
}

async fn run_tail(
    socket: WebSocket,
    mut receiver: broadcast::Receiver<Arc<LogView>>,
    filter: LogFilter,
) {
    let (mut sink, mut incoming) = socket.split();
    loop {
        tokio::select! {
            item = receiver.recv() => {
                let message = match item {
                    Ok(row) if filter.matches(&row) => json!({ "type": "entry", "entry": &*row }),
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        json!({ "type": "lagged", "skipped": skipped })
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if sink.send(Message::Text(message.to_string().into())).await.is_err() {
                    return;
                }
            }
            incoming = incoming.next() => match incoming {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                Some(Ok(_)) => {}
            }
        }
    }
}

async fn get_retention(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<RetentionSettings>, ApiError> {
    auth.require_admin()?;
    Ok(Json(
        retention::load(&state.pool)
            .await
            .map_err(ApiError::database)?,
    ))
}

async fn update_retention(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<UpdateRetention>,
) -> Result<Json<RetentionSettings>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let mut current = retention::load(&state.pool)
        .await
        .map_err(ApiError::database)?;
    for (source, days) in &body.log_days {
        if !LOG_SOURCES.contains(&source.as_str()) {
            return Err(bad(
                "invalid_source",
                format!("source must be one of {}", LOG_SOURCES.join(", ")),
            ));
        }
        if !(1..=MAX_LOG_DAYS).contains(days) {
            return Err(bad(
                "invalid_days",
                format!("retention is 1 to {MAX_LOG_DAYS} days"),
            ));
        }
        current.log_days.insert(source.clone(), *days);
    }
    if let Some(metrics) = body.metrics {
        metrics
            .validate()
            .map_err(|message| bad("invalid_days", message))?;
        current.metrics = metrics;
    }
    let value = serde_json::to_value(&current.log_days)
        .map_err(|_| bad("invalid", "retention settings"))?;
    let metrics =
        serde_json::to_value(current.metrics).map_err(|_| bad("invalid", "retention settings"))?;
    sqlx::query!(
        "UPDATE retention_settings SET log_days = $1, metrics = $2, updated_at = now() WHERE id",
        value,
        metrics
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "settings.retention",
        "settings",
        None,
        json!({ "log_days": &current.log_days, "metrics": current.metrics }),
        None,
    )
    .await;
    Ok(Json(current))
}

async fn terminal_container(
    state: &AppState,
    id: Uuid,
    container: Option<&str>,
) -> Result<(Uuid, String), ApiError> {
    let row = sqlx::query!(
        "SELECT server_id, kind, current_container_id FROM services WHERE id = $1",
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))?;
    if row.kind == "compose" {
        let name = container.ok_or_else(|| {
            bad(
                "container_required",
                "choose a compose service with ?container=",
            )
        })?;
        let stack = crate::stacks::load(state, id).await?;
        let target = stack
            .spec
            .containers
            .get(name)
            .cloned()
            .ok_or_else(|| not_found("that compose service is not in this stack"))?;
        return Ok((row.server_id, target));
    }
    let target = row
        .current_container_id
        .ok_or_else(|| bad("not_running", "this service has no running container"))?;
    Ok((row.server_id, target))
}

async fn terminal(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<TerminalQuery>,
    upgrade: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    auth.require_write()?;
    if !same_origin(&headers) {
        return Err(forbidden_origin());
    }
    let (server_id, container) = terminal_container(&state, id, query.container.as_deref()).await?;
    let driver = driver_for(&state, server_id).await?;
    let cols = query.cols.unwrap_or(80).clamp(10, 500);
    let rows = query.rows.unwrap_or(24).clamp(5, 300);
    let (input_tx, input_rx) = mpsc::channel::<TerminalInput>(64);
    let input = futures_util::stream::unfold(input_rx, |mut rx| async move {
        rx.recv().await.map(|item| (item, rx))
    });
    let output = driver
        .terminal(&container, cols, rows, Box::pin(input))
        .await
        .map_err(core_error)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "service.terminal",
        "service",
        Some(&id.to_string()),
        json!({ "container": query.container }),
        None,
    )
    .await;
    Ok(upgrade.on_upgrade(move |socket| run_terminal(socket, input_tx, output)))
}

async fn run_terminal(
    socket: WebSocket,
    input: mpsc::Sender<TerminalInput>,
    mut output: rudder_core::ByteStream,
) {
    let (mut sink, mut incoming) = socket.split();
    loop {
        tokio::select! {
            chunk = output.next() => match chunk {
                Some(Ok(bytes)) => {
                    if sink.send(Message::Binary(bytes)).await.is_err() {
                        return;
                    }
                }
                Some(Err(error)) => {
                    let _ = sink
                        .send(Message::Text(json!({ "type": "error", "message": error.to_string() }).to_string().into()))
                        .await;
                    let _ = sink.close().await;
                    return;
                }
                None => {
                    let _ = sink.send(Message::Text(json!({ "type": "exit" }).to_string().into())).await;
                    let _ = sink.close().await;
                    return;
                }
            },
            message = incoming.next() => {
                let item = match message {
                    Some(Ok(Message::Text(text))) => match serde_json::from_str::<TerminalMessage>(&text) {
                        Ok(TerminalMessage::Input { data }) => TerminalInput::Data(Bytes::from(data)),
                        Ok(TerminalMessage::Resize { cols, rows }) => TerminalInput::Resize {
                            cols: cols.clamp(10, 500),
                            rows: rows.clamp(5, 300),
                        },
                        Err(_) => continue,
                    },
                    Some(Ok(Message::Binary(bytes))) => TerminalInput::Data(bytes),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                    Some(Ok(_)) => continue,
                };
                if input.send(item).await.is_err() {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cells_quote_and_neutralize_formulas() {
        assert_eq!(csv_cell("a\"b"), "\"a\"\"b\"");
        assert_eq!(csv_cell("=1+1"), "\"'=1+1\"");
    }

    #[test]
    fn origin_must_match_host() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "panel.test".parse().unwrap());
        assert!(same_origin(&headers));
        headers.insert(header::ORIGIN, "https://panel.test".parse().unwrap());
        assert!(same_origin(&headers));
        headers.insert(header::ORIGIN, "https://evil.test".parse().unwrap());
        assert!(!same_origin(&headers));
    }

    #[test]
    fn filters_parse_levels_and_status_classes() {
        let query = LogQuery {
            level: Some("warn,error".into()),
            status: Some("5xx".into()),
            ..Default::default()
        };
        let filter = parse_filter(&query).unwrap();
        assert_eq!(filter.levels, ["warn", "error"]);
        assert_eq!(
            (filter.status_min, filter.status_max),
            (Some(500), Some(599))
        );
        let bad_level = LogQuery {
            level: Some("loud".into()),
            ..Default::default()
        };
        assert!(parse_filter(&bad_level).is_err());
    }
}
