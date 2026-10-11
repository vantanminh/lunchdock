//! Metrics charts and overview, alert rules, notification channels, and Docker prune.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use rudder_core::{
    alert_needs_threshold, resolution_for, validate_alert_kind, Resolution, ALERT_KINDS,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::monitor::notify::{self, ChannelConfig, Notice};
use crate::nodes::driver_for;
use crate::state::AppState;

use super::catalog::{bad, conflict, core_error, is_unique, not_found};

const MAX_METRICS: usize = 8;
const PROXY_METRICS: &[&str] = &["requests", "http_5xx"];

#[derive(Debug, Deserialize)]
pub struct MetricsQuery {
    pub server_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    /// Comma-separated metric names.
    pub metrics: String,
    /// `15m`, `1h`, `6h`, `24h`, `7d`, or `30d`.
    pub range: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct MetricPoint {
    pub t: DateTime<Utc>,
    pub avg: f64,
    pub min: f64,
    pub max: f64,
}

#[derive(Serialize, ToSchema)]
pub struct MetricSeries {
    pub metric: String,
    pub points: Vec<MetricPoint>,
}

#[derive(Serialize, ToSchema)]
pub struct MetricsView {
    /// `raw`, `1m`, `5m`, or `1h`.
    pub resolution: String,
    pub since: DateTime<Utc>,
    pub series: Vec<MetricSeries>,
}

#[derive(Serialize, ToSchema)]
pub struct ServerUsage {
    pub server_id: Uuid,
    pub name: String,
    pub status: String,
    pub cpu_percent: Option<f64>,
    pub memory_percent: Option<f64>,
    pub disk_percent: Option<f64>,
    pub load1: Option<f64>,
    pub sampled_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct ServiceUsage {
    pub service_id: Uuid,
    pub name: String,
    pub server_id: Uuid,
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<f64>,
    pub containers: Option<f64>,
}

#[derive(Serialize, ToSchema)]
pub struct MetricsOverview {
    pub servers: Vec<ServerUsage>,
    pub services: Vec<ServiceUsage>,
}

#[derive(Serialize, ToSchema)]
pub struct AlertKindView {
    pub kind: String,
    pub description: String,
    pub needs_threshold: bool,
    /// `server`, `service`, or `events`.
    pub target: String,
}

#[derive(Serialize, ToSchema)]
pub struct AlertView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    /// `server_id`, `service_id`, or `events` (patterns) and `min_severity`.
    #[schema(value_type = Object)]
    pub target_spec: serde_json::Value,
    pub threshold: Option<f64>,
    pub duration_secs: i32,
    pub channel_ids: Vec<Uuid>,
    pub enabled: bool,
    /// `ok`, `pending`, or `firing`.
    pub state: String,
    pub state_since: DateTime<Utc>,
    pub last_value: Option<f64>,
    pub last_fired_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveAlert {
    pub name: String,
    pub kind: String,
    #[schema(value_type = Object)]
    pub target_spec: serde_json::Value,
    pub threshold: Option<f64>,
    pub duration_secs: Option<i32>,
    pub channel_ids: Vec<Uuid>,
    pub enabled: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct ChannelView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    /// Non-secret description such as the chat id or webhook host.
    pub summary: String,
    pub enabled: bool,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveChannel {
    pub name: String,
    /// `{"kind":"telegram","bot_token","chat_id"}`, `{"kind":"discord"|"slack","webhook_url"}`,
    /// `{"kind":"webhook","url","secret"?}`, or
    /// `{"kind":"smtp","host","port"?,"security":"starttls"|"tls"|"none","username"?,"password"?,"from","to":[]}`.
    /// On update, empty secret fields keep the stored value.
    #[schema(value_type = Object)]
    pub config: serde_json::Value,
    pub enabled: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct PruneResult {
    pub reclaimed_bytes: u64,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/metrics", get(metrics))
        .route("/api/v1/metrics/overview", get(overview))
        .route("/api/v1/alert-kinds", get(alert_kinds))
        .route("/api/v1/alerts", get(list_alerts).post(create_alert))
        .route(
            "/api/v1/alerts/{id}",
            put(update_alert).delete(delete_alert),
        )
        .route(
            "/api/v1/notification-channels",
            get(list_channels).post(create_channel),
        )
        .route(
            "/api/v1/notification-channels/{id}",
            put(update_channel).delete(delete_channel),
        )
        .route(
            "/api/v1/notification-channels/{id}/test",
            post(test_channel),
        )
        .route("/api/v1/servers/{id}/prune", post(prune))
}

fn range_secs(range: Option<&str>) -> Result<i64, ApiError> {
    Ok(match range.unwrap_or("1h") {
        "15m" => 900,
        "1h" => 3600,
        "6h" => 6 * 3600,
        "24h" => 24 * 3600,
        "7d" => 7 * 86_400,
        "30d" => 30 * 86_400,
        _ => {
            return Err(bad(
                "invalid_range",
                "range must be 15m, 1h, 6h, 24h, 7d, or 30d",
            ))
        }
    })
}

async fn metrics(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(query): Query<MetricsQuery>,
) -> Result<Json<MetricsView>, ApiError> {
    if query.server_id.is_some() == query.service_id.is_some() {
        return Err(bad(
            "invalid_scope",
            "give exactly one of server_id or service_id",
        ));
    }
    let names: Vec<String> = query
        .metrics
        .split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
        .collect();
    if names.is_empty()
        || names.len() > MAX_METRICS
        || names.iter().any(|m| {
            m.len() > 40
                || !m
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
    {
        return Err(bad("invalid_metrics", "give 1 to 8 metric names"));
    }
    let seconds = range_secs(query.range.as_deref())?;
    let since = Utc::now() - Duration::seconds(seconds);
    let resolution = resolution_for(seconds);
    let mut series = Vec::with_capacity(names.len());
    for name in names {
        let effective = if resolution == Resolution::Raw && PROXY_METRICS.contains(&name.as_str()) {
            Resolution::M1
        } else {
            resolution
        };
        let points = if effective == Resolution::Raw {
            sqlx::query_as!(
                MetricPoint,
                r#"
                SELECT sampled_at AS "t!", value AS "avg!", value AS "min!", value AS "max!"
                FROM metrics_samples
                WHERE sampled_at >= $1 AND metric = $2
                  AND (($3::uuid IS NOT NULL AND server_id = $3 AND service_id IS NULL)
                    OR ($4::uuid IS NOT NULL AND service_id = $4))
                ORDER BY sampled_at
                "#,
                since,
                name,
                query.server_id,
                query.service_id,
            )
            .fetch_all(&state.pool)
            .await
        } else {
            sqlx::query_as!(
                MetricPoint,
                r#"
                SELECT bucket_start AS "t!", avg_value AS "avg!", min_value AS "min!",
                       max_value AS "max!"
                FROM metrics_rollups
                WHERE resolution = $5 AND bucket_start >= $1 AND metric = $2
                  AND (($3::uuid IS NOT NULL AND server_id = $3 AND service_id IS NULL)
                    OR ($4::uuid IS NOT NULL AND service_id = $4))
                ORDER BY bucket_start
                "#,
                since,
                name,
                query.server_id,
                query.service_id,
                effective.as_str(),
            )
            .fetch_all(&state.pool)
            .await
        }
        .map_err(ApiError::database)?;
        series.push(MetricSeries {
            metric: name,
            points,
        });
    }
    Ok(Json(MetricsView {
        resolution: resolution.as_str().to_owned(),
        since,
        series,
    }))
}

async fn overview(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<MetricsOverview>, ApiError> {
    let servers = sqlx::query_as!(
        ServerUsage,
        r#"
        SELECT s.id AS server_id, s.name, s.status,
               latest.cpu AS cpu_percent, latest.mem AS memory_percent,
               latest.disk AS disk_percent, latest.load1, latest.at AS sampled_at
        FROM servers s
        LEFT JOIN LATERAL (
            SELECT max(sampled_at) AS at,
                   avg(value) FILTER (WHERE metric = 'cpu_percent') AS cpu,
                   avg(value) FILTER (WHERE metric = 'memory_percent') AS mem,
                   avg(value) FILTER (WHERE metric = 'disk_percent') AS disk,
                   avg(value) FILTER (WHERE metric = 'load1') AS load1
            FROM metrics_samples
            WHERE server_id = s.id AND service_id IS NULL
              AND sampled_at > now() - INTERVAL '30 seconds'
        ) latest ON TRUE
        ORDER BY s.name
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let services = sqlx::query_as!(
        ServiceUsage,
        r#"
        SELECT s.id AS service_id, s.name, s.server_id,
               latest.cpu AS cpu_percent, latest.mem AS memory_bytes,
               latest.containers
        FROM services s
        JOIN LATERAL (
            SELECT avg(value) FILTER (WHERE metric = 'cpu_percent') AS cpu,
                   avg(value) FILTER (WHERE metric = 'memory_bytes') AS mem,
                   max(value) FILTER (WHERE metric = 'containers') AS containers
            FROM metrics_samples
            WHERE service_id = s.id AND sampled_at > now() - INTERVAL '30 seconds'
        ) latest ON latest.cpu IS NOT NULL
        ORDER BY latest.cpu DESC NULLS LAST
        LIMIT 50
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(MetricsOverview { servers, services }))
}

fn alert_target(kind: &str) -> &'static str {
    match kind {
        "event" => "events",
        k if k.starts_with("server_") => "server",
        _ => "service",
    }
}

async fn alert_kinds(_auth: AuthUser) -> Json<Vec<AlertKindView>> {
    Json(
        ALERT_KINDS
            .iter()
            .map(|(kind, description)| AlertKindView {
                kind: (*kind).to_owned(),
                description: (*description).to_owned(),
                needs_threshold: alert_needs_threshold(kind),
                target: alert_target(kind).to_owned(),
            })
            .collect(),
    )
}

async fn list_alerts(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<AlertView>>, ApiError> {
    let rows = sqlx::query_as!(
        AlertView,
        r#"
        SELECT id, name, kind, target_spec, threshold, duration_secs, channel_ids, enabled,
               state, state_since, last_value, last_fired_at, created_at
        FROM alerts ORDER BY name
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(rows))
}

async fn validate_alert(state: &AppState, body: &SaveAlert) -> Result<serde_json::Value, ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(bad("invalid_name", "name is 1 to 100 characters"));
    }
    validate_alert_kind(&body.kind).map_err(core_error)?;
    if alert_needs_threshold(&body.kind) {
        match body.threshold {
            Some(value) if value.is_finite() && value >= 0.0 => {}
            _ => {
                return Err(bad(
                    "invalid_threshold",
                    "this alert needs a threshold of 0 or more",
                ))
            }
        }
        let percent = matches!(
            body.kind.as_str(),
            "server_cpu" | "server_memory" | "server_disk" | "service_cpu"
        );
        if percent && body.threshold.is_some_and(|t| t > 100.0) {
            return Err(bad("invalid_threshold", "percent thresholds are 0 to 100"));
        }
    }
    if !(0..=86_400).contains(&body.duration_secs.unwrap_or(0)) {
        return Err(bad("invalid_duration", "duration is 0 to 86400 seconds"));
    }
    let mut spec = serde_json::Map::new();
    match alert_target(&body.kind) {
        "events" => {
            let events: Vec<String> = body
                .target_spec
                .get("events")
                .and_then(|v| serde_json::from_value(v.clone()).ok())
                .unwrap_or_else(|| vec!["*".to_owned()]);
            if events.is_empty()
                || events.len() > 20
                || events.iter().any(|e| {
                    e.is_empty()
                        || e.len() > 60
                        || !e
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c == '.' || c == '_' || c == '*')
                })
            {
                return Err(bad(
                    "invalid_events",
                    "events are patterns like backup.* or deploy.failed",
                ));
            }
            let min = body
                .target_spec
                .get("min_severity")
                .and_then(|v| v.as_str())
                .unwrap_or("info");
            if !["info", "warning", "critical"].contains(&min) {
                return Err(bad(
                    "invalid_severity",
                    "min_severity is info, warning, or critical",
                ));
            }
            spec.insert("events".into(), json!(events));
            spec.insert("min_severity".into(), json!(min));
        }
        scope => {
            let key = if scope == "server" {
                "server_id"
            } else {
                "service_id"
            };
            let id = body
                .target_spec
                .get(key)
                .and_then(|v| v.as_str())
                .and_then(|v| Uuid::parse_str(v).ok())
                .ok_or_else(|| bad("invalid_target", format!("target_spec.{key} is required")))?;
            let exists = if scope == "server" {
                sqlx::query_scalar!("SELECT count(*) AS \"n!\" FROM servers WHERE id = $1", id)
                    .fetch_one(&state.pool)
                    .await
            } else {
                sqlx::query_scalar!("SELECT count(*) AS \"n!\" FROM services WHERE id = $1", id)
                    .fetch_one(&state.pool)
                    .await
            }
            .map_err(ApiError::database)?;
            if exists == 0 {
                return Err(bad("invalid_target", format!("{scope} not found")));
            }
            spec.insert(key.into(), json!(id));
        }
    }
    let known = sqlx::query_scalar!(
        "SELECT count(*) AS \"n!\" FROM notification_channels WHERE id = ANY($1)",
        &body.channel_ids
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let mut unique = body.channel_ids.clone();
    unique.sort();
    unique.dedup();
    if known as usize != unique.len() {
        return Err(bad("invalid_channels", "unknown notification channel"));
    }
    Ok(serde_json::Value::Object(spec))
}

async fn create_alert(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SaveAlert>,
) -> Result<(StatusCode, Json<AlertView>), ApiError> {
    auth.require_write()?;
    let spec = validate_alert(&state, &body).await?;
    let row = sqlx::query_as!(
        AlertView,
        r#"
        INSERT INTO alerts (name, kind, target_spec, threshold, duration_secs, channel_ids, enabled)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, name, kind, target_spec, threshold, duration_secs, channel_ids, enabled,
                  state, state_since, last_value, last_fired_at, created_at
        "#,
        body.name.trim(),
        body.kind,
        spec,
        body.threshold,
        body.duration_secs.unwrap_or(0),
        &body.channel_ids,
        body.enabled.unwrap_or(true),
    )
    .fetch_one(&state.pool)
    .await
    .map_err(|error| {
        if is_unique(&error) {
            conflict("name_taken", "an alert with this name exists")
        } else {
            ApiError::database(error)
        }
    })?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "alert.create",
        "alert",
        Some(&row.id.to_string()),
        json!({ "name": row.name, "kind": row.kind }),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(row)))
}

async fn update_alert(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveAlert>,
) -> Result<Json<AlertView>, ApiError> {
    auth.require_write()?;
    let spec = validate_alert(&state, &body).await?;
    let row = sqlx::query_as!(
        AlertView,
        r#"
        UPDATE alerts
        SET name = $2, kind = $3, target_spec = $4, threshold = $5, duration_secs = $6,
            channel_ids = $7, enabled = $8, updated_at = now(),
            state = CASE WHEN kind = $3 AND target_spec = $4 THEN state ELSE 'ok' END,
            state_since = CASE WHEN kind = $3 AND target_spec = $4 THEN state_since ELSE now() END
        WHERE id = $1
        RETURNING id, name, kind, target_spec, threshold, duration_secs, channel_ids, enabled,
                  state, state_since, last_value, last_fired_at, created_at
        "#,
        id,
        body.name.trim(),
        body.kind,
        spec,
        body.threshold,
        body.duration_secs.unwrap_or(0),
        &body.channel_ids,
        body.enabled.unwrap_or(true),
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| {
        if is_unique(&error) {
            conflict("name_taken", "an alert with this name exists")
        } else {
            ApiError::database(error)
        }
    })?
    .ok_or_else(|| not_found("alert not found"))?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "alert.update",
        "alert",
        Some(&id.to_string()),
        json!({ "name": row.name }),
        None,
    )
    .await;
    Ok(Json(row))
}

async fn delete_alert(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let deleted = sqlx::query!("DELETE FROM alerts WHERE id = $1", id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if deleted.rows_affected() == 0 {
        return Err(not_found("alert not found"));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "alert.delete",
        "alert",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

struct ChannelRow {
    id: Uuid,
    name: String,
    kind: String,
    config_enc: Vec<u8>,
    enabled: bool,
    created_at: DateTime<Utc>,
}

fn channel_view(state: &AppState, row: ChannelRow) -> ChannelView {
    let summary = ChannelConfig::open(&state.cipher, &row.config_enc)
        .map(|config| config.summary())
        .unwrap_or_else(|_| "config unreadable".into());
    ChannelView {
        id: row.id,
        name: row.name,
        kind: row.kind,
        summary,
        enabled: row.enabled,
        created_at: row.created_at,
    }
}

async fn load_channel(state: &AppState, id: Uuid) -> Result<ChannelRow, ApiError> {
    sqlx::query_as!(
        ChannelRow,
        "SELECT id, name, kind, config_enc, enabled, created_at FROM notification_channels WHERE id = $1",
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("notification channel not found"))
}

async fn list_channels(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<ChannelView>>, ApiError> {
    auth.require_admin()?;
    let rows = sqlx::query_as!(
        ChannelRow,
        "SELECT id, name, kind, config_enc, enabled, created_at FROM notification_channels ORDER BY name"
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| channel_view(&state, row))
            .collect(),
    ))
}

fn parse_channel(body: &SaveChannel) -> Result<ChannelConfig, ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(bad("invalid_name", "name is 1 to 100 characters"));
    }
    serde_json::from_value::<ChannelConfig>(body.config.clone()).map_err(|_| {
        bad(
            "invalid_config",
            "config must have a kind (telegram, discord, slack, webhook, smtp) and its fields",
        )
    })
}

async fn create_channel(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SaveChannel>,
) -> Result<(StatusCode, Json<ChannelView>), ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let config = parse_channel(&body)?;
    config.validate().map_err(core_error)?;
    let sealed = config.seal(&state.cipher).map_err(core_error)?;
    let row = sqlx::query_as!(
        ChannelRow,
        r#"
        INSERT INTO notification_channels (name, kind, config_enc, enabled)
        VALUES ($1, $2, $3, $4)
        RETURNING id, name, kind, config_enc, enabled, created_at
        "#,
        body.name.trim(),
        config.kind(),
        sealed,
        body.enabled.unwrap_or(true),
    )
    .fetch_one(&state.pool)
    .await
    .map_err(|error| {
        if is_unique(&error) {
            conflict("name_taken", "a channel with this name exists")
        } else {
            ApiError::database(error)
        }
    })?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "notification_channel.create",
        "notification_channel",
        Some(&row.id.to_string()),
        json!({ "name": row.name, "kind": row.kind }),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(channel_view(&state, row))))
}

async fn update_channel(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveChannel>,
) -> Result<Json<ChannelView>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let existing = load_channel(&state, id).await?;
    let previous = ChannelConfig::open(&state.cipher, &existing.config_enc).map_err(core_error)?;
    let config = parse_channel(&body)?.merge_secrets(&previous);
    config.validate().map_err(core_error)?;
    let sealed = config.seal(&state.cipher).map_err(core_error)?;
    let row = sqlx::query_as!(
        ChannelRow,
        r#"
        UPDATE notification_channels
        SET name = $2, kind = $3, config_enc = $4, enabled = $5, updated_at = now()
        WHERE id = $1
        RETURNING id, name, kind, config_enc, enabled, created_at
        "#,
        id,
        body.name.trim(),
        config.kind(),
        sealed,
        body.enabled.unwrap_or(existing.enabled),
    )
    .fetch_one(&state.pool)
    .await
    .map_err(|error| {
        if is_unique(&error) {
            conflict("name_taken", "a channel with this name exists")
        } else {
            ApiError::database(error)
        }
    })?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "notification_channel.update",
        "notification_channel",
        Some(&id.to_string()),
        json!({ "name": row.name }),
        None,
    )
    .await;
    Ok(Json(channel_view(&state, row)))
}

async fn delete_channel(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    let deleted = sqlx::query!("DELETE FROM notification_channels WHERE id = $1", id)
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database)?;
    if deleted.rows_affected() == 0 {
        return Err(not_found("notification channel not found"));
    }
    sqlx::query!(
        "UPDATE alerts SET channel_ids = array_remove(channel_ids, $1) WHERE $1 = ANY(channel_ids)",
        id
    )
    .execute(&mut *tx)
    .await
    .map_err(ApiError::database)?;
    tx.commit().await.map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "notification_channel.delete",
        "notification_channel",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn test_channel(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<crate::routes::backups::TestResult>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let row = load_channel(&state, id).await?;
    let config = ChannelConfig::open(&state.cipher, &row.config_enc).map_err(core_error)?;
    let notice = Notice {
        id: 0,
        kind: "test".into(),
        severity: "info".into(),
        title: format!("Test notification for {}", row.name),
        message: "If you can read this, Rudder can reach this channel.".into(),
        server_id: None,
        service_id: None,
        created_at: Utc::now(),
    };
    let result = notify::send(&config, &notice).await;
    Ok(Json(crate::routes::backups::TestResult {
        ok: result.is_ok(),
        message: match result {
            Ok(()) => "delivered".into(),
            Err(error) => error.to_string(),
        },
    }))
}

async fn prune(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<PruneResult>, ApiError> {
    auth.require_write()?;
    let driver = driver_for(&state, id).await?;
    let reclaimed_bytes = driver.prune().await.map_err(core_error)?;
    sqlx::query!(
        "UPDATE servers SET last_pruned_at = now() WHERE id = $1",
        id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "server.prune",
        "server",
        Some(&id.to_string()),
        json!({ "reclaimed_bytes": reclaimed_bytes }),
        None,
    )
    .await;
    Ok(Json(PruneResult { reclaimed_bytes }))
}
