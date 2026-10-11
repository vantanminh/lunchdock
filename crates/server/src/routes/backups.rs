//! S3 destinations, backup jobs, runs, downloads, and restores.

use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::Response;
use axum::routing::{get, post};
use axum::Json;
use chrono::{DateTime, Utc};
use futures_util::StreamExt;
use rudder_core::{
    validate_host_path, validate_prefix, validate_schedule, validate_tool_options, ArchiveSource,
    Compression, Consistency, RetentionSpec, ToolOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::backup::{self, RestoreRequest};
use crate::error::ApiError;
use crate::s3;
use crate::state::AppState;

use super::catalog::{bad, conflict, core_error, is_unique, not_found};

#[derive(Serialize, ToSchema)]
pub struct DestinationView {
    pub id: Uuid,
    pub name: String,
    pub endpoint: Option<String>,
    pub region: String,
    pub bucket: String,
    pub path_style: bool,
    /// Last four characters of the access key id.
    pub access_key_hint: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveDestination {
    pub name: String,
    pub endpoint: Option<String>,
    pub region: Option<String>,
    pub bucket: String,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub path_style: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct TestResult {
    pub ok: bool,
    pub message: String,
}

#[derive(Serialize, ToSchema)]
pub struct BackupJobView {
    pub id: Uuid,
    pub name: String,
    pub server_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub target_kind: String,
    pub target_spec: Value,
    pub schedule_cron: Option<String>,
    pub timezone: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub destination_ids: Vec<Uuid>,
    pub path_prefix: String,
    pub filename_template: String,
    pub retention: Value,
    pub compression: String,
    pub compression_level: i32,
    pub encrypted: bool,
    pub tool_options: Value,
    pub consistency: Value,
    pub timeout_secs: i32,
    pub retries: i32,
    pub bandwidth_limit_bps: Option<i64>,
    pub verify_after_upload: bool,
    pub notify_on_success: bool,
    pub enabled: bool,
    pub last_status: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema, Default)]
pub struct SaveBackupJob {
    pub name: Option<String>,
    /// database, volume, bind, or path.
    pub target_kind: Option<String>,
    pub server_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    /// `{"volume": "name"}`, `{"source": "/bind/source"}`, or `{"path": "/host/path"}`.
    pub target_spec: Option<Value>,
    /// Five-field cron or a preset such as `@daily`. Empty string clears it.
    pub schedule_cron: Option<String>,
    pub timezone: Option<String>,
    pub destination_ids: Option<Vec<Uuid>>,
    pub path_prefix: Option<String>,
    pub filename_template: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub retention: Option<RetentionSpec>,
    pub compression: Option<String>,
    pub compression_level: Option<i32>,
    /// Client-side encryption passphrase. Empty string removes encryption.
    pub encryption_passphrase: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub tool_options: Option<ToolOptions>,
    #[schema(value_type = Option<Object>)]
    pub consistency: Option<Consistency>,
    pub timeout_secs: Option<i32>,
    pub retries: Option<i32>,
    pub bandwidth_limit_bps: Option<i64>,
    pub verify_after_upload: Option<bool>,
    pub notify_on_success: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct BackupRunView {
    pub id: Uuid,
    pub job_id: Uuid,
    pub kind: String,
    pub trigger: String,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub duration_ms: Option<i64>,
    pub size_bytes: Option<i64>,
    pub checksum: Option<String>,
    pub object_keys: Vec<String>,
    pub destination_ids: Vec<Uuid>,
    pub meta: Value,
    pub logs: String,
    pub error: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RunAccepted {
    pub run_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct EventView {
    pub id: i64,
    pub kind: String,
    pub severity: String,
    pub server_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub title: String,
    pub message: String,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize)]
pub struct JobQuery {
    pub service_id: Option<Uuid>,
}

#[derive(Deserialize)]
pub struct LimitQuery {
    pub limit: Option<i64>,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/s3-destinations",
            get(list_destinations).post(create_destination),
        )
        .route(
            "/api/v1/s3-destinations/{id}",
            axum::routing::put(update_destination).delete(delete_destination),
        )
        .route("/api/v1/s3-destinations/{id}/test", post(test_destination))
        .route("/api/v1/backup-jobs", get(list_jobs).post(create_job))
        .route(
            "/api/v1/backup-jobs/{id}",
            get(get_job).patch(update_job).delete(delete_job),
        )
        .route("/api/v1/backup-jobs/{id}/run", post(run_job))
        .route("/api/v1/backup-jobs/{id}/runs", get(list_runs))
        .route("/api/v1/backup-runs/{id}", get(get_run))
        .route("/api/v1/backup-runs/{id}/download", get(download_run))
        .route("/api/v1/backup-runs/{id}/restore", post(restore_run))
        .route("/api/v1/events", get(list_events))
        .route("/api/v1/backup-presets", get(presets))
}

async fn presets(_auth: AuthUser) -> Json<Value> {
    Json(json!({
        "schedules": rudder_core::PRESETS
            .iter()
            .map(|(name, cron)| json!({ "name": name, "cron": cron }))
            .collect::<Vec<_>>(),
        "filename_tokens": ["{name}", "{service}", "{engine}", "{timestamp}", "{date}", "{unix}"],
    }))
}

struct DestinationRow {
    id: Uuid,
    name: String,
    endpoint: Option<String>,
    region: String,
    bucket: String,
    path_style: bool,
    access_key_id_enc: Vec<u8>,
    created_at: DateTime<Utc>,
}

fn destination_view(state: &AppState, row: DestinationRow) -> DestinationView {
    let hint = state
        .cipher
        .decrypt(&row.access_key_id_enc)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .map(|key| {
            let tail: String = key.chars().rev().take(4).collect();
            format!("…{}", tail.chars().rev().collect::<String>())
        })
        .unwrap_or_default();
    DestinationView {
        id: row.id,
        name: row.name,
        endpoint: row.endpoint,
        region: row.region,
        bucket: row.bucket,
        path_style: row.path_style,
        access_key_hint: hint,
        created_at: row.created_at,
    }
}

async fn list_destinations(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<DestinationView>>, ApiError> {
    let rows = sqlx::query_as!(
        DestinationRow,
        r#"
        SELECT id, name, endpoint, region, bucket, path_style, access_key_id_enc, created_at
        FROM s3_destinations ORDER BY name
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| destination_view(&state, row))
            .collect(),
    ))
}

fn check_destination(body: &SaveDestination, creating: bool) -> Result<(), ApiError> {
    rudder_core::validate_name(&body.name).map_err(core_error)?;
    if let Some(endpoint) = body.endpoint.as_deref().filter(|e| !e.is_empty()) {
        s3::validate_endpoint(endpoint).map_err(core_error)?;
    }
    if creating
        && (body.access_key_id.as_deref().unwrap_or("").is_empty()
            || body.secret_access_key.as_deref().unwrap_or("").is_empty())
    {
        return Err(bad("invalid", "access key id and secret are required"));
    }
    s3::build_store(&s3::StoreConfig {
        endpoint: body.endpoint.as_deref(),
        region: body.region.as_deref().unwrap_or("us-east-1"),
        bucket: &body.bucket,
        access_key_id: "probe",
        secret_access_key: "probe",
        path_style: body.path_style.unwrap_or(true),
    })
    .map_err(core_error)?;
    Ok(())
}

async fn create_destination(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SaveDestination>,
) -> Result<(StatusCode, Json<DestinationView>), ApiError> {
    auth.require_write()?;
    check_destination(&body, true)?;
    let access = state
        .cipher
        .encrypt(body.access_key_id.as_deref().unwrap_or("").as_bytes())
        .map_err(core_error)?;
    let secret = state
        .cipher
        .encrypt(body.secret_access_key.as_deref().unwrap_or("").as_bytes())
        .map_err(core_error)?;
    let inserted = sqlx::query_as!(
        DestinationRow,
        r#"
        INSERT INTO s3_destinations (name, endpoint, region, bucket, access_key_id_enc,
                                     secret_access_key_enc, path_style)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, name, endpoint, region, bucket, path_style, access_key_id_enc, created_at
        "#,
        body.name,
        body.endpoint.filter(|e| !e.is_empty()),
        body.region.unwrap_or_else(|| "us-east-1".into()),
        body.bucket,
        access,
        secret,
        body.path_style.unwrap_or(true),
    )
    .fetch_one(&state.pool)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict(
                "name_taken",
                "a destination with that name exists",
            ))
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "s3_destination.create",
        "s3_destination",
        Some(&row.id.to_string()),
        json!({ "name": row.name, "bucket": row.bucket }),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(destination_view(&state, row))))
}

async fn update_destination(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveDestination>,
) -> Result<Json<DestinationView>, ApiError> {
    auth.require_write()?;
    check_destination(&body, false)?;
    let access = match body.access_key_id.as_deref().filter(|v| !v.is_empty()) {
        Some(value) => Some(state.cipher.encrypt(value.as_bytes()).map_err(core_error)?),
        None => None,
    };
    let secret = match body.secret_access_key.as_deref().filter(|v| !v.is_empty()) {
        Some(value) => Some(state.cipher.encrypt(value.as_bytes()).map_err(core_error)?),
        None => None,
    };
    let row = sqlx::query_as!(
        DestinationRow,
        r#"
        UPDATE s3_destinations
        SET name = $2, endpoint = $3, region = $4, bucket = $5,
            access_key_id_enc = COALESCE($6, access_key_id_enc),
            secret_access_key_enc = COALESCE($7, secret_access_key_enc),
            path_style = $8, updated_at = now()
        WHERE id = $1
        RETURNING id, name, endpoint, region, bucket, path_style, access_key_id_enc, created_at
        "#,
        id,
        body.name,
        body.endpoint.filter(|e| !e.is_empty()),
        body.region.unwrap_or_else(|| "us-east-1".into()),
        body.bucket,
        access,
        secret,
        body.path_style.unwrap_or(true),
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("S3 destination not found"))?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "s3_destination.update",
        "s3_destination",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(Json(destination_view(&state, row)))
}

async fn delete_destination(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let used = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM backup_jobs WHERE $1 = ANY(destination_ids)"#,
        id
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if used > 0 {
        return Err(conflict(
            "in_use",
            "remove this destination from backup jobs first",
        ));
    }
    let result = sqlx::query!(r#"DELETE FROM s3_destinations WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if result.rows_affected() == 0 {
        return Err(not_found("S3 destination not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn test_destination(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<TestResult>, ApiError> {
    auth.require_write()?;
    let destination = s3::load_destination(&state, id).await?;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        s3::test_connection(destination.store.as_ref()),
    )
    .await
    .unwrap_or_else(|_| Err("timed out after 30 seconds".into()));
    Ok(Json(match result {
        Ok(()) => TestResult {
            ok: true,
            message: "write, read, and delete succeeded".into(),
        },
        Err(message) => TestResult { ok: false, message },
    }))
}

struct JobRow {
    id: Uuid,
    name: String,
    server_id: Option<Uuid>,
    service_id: Option<Uuid>,
    target_kind: String,
    target_spec: Value,
    schedule_cron: Option<String>,
    timezone: String,
    destination_ids: Vec<Uuid>,
    path_prefix: String,
    filename_template: String,
    retention_spec: Value,
    compression: String,
    compression_level: i32,
    encrypted: bool,
    tool_options: Value,
    consistency_spec: Value,
    timeout_secs: i32,
    retries: i32,
    bandwidth_limit_bps: Option<i64>,
    verify_after_upload: bool,
    notify_on_success: bool,
    enabled: bool,
    created_at: DateTime<Utc>,
    last_status: Option<String>,
    last_run_at: Option<DateTime<Utc>>,
}

fn job_view(row: JobRow) -> BackupJobView {
    let anchor = row.last_run_at.unwrap_or(row.created_at);
    BackupJobView {
        next_run_at: if row.enabled {
            backup::next_run(row.schedule_cron.as_deref(), &row.timezone, anchor)
        } else {
            None
        },
        id: row.id,
        name: row.name,
        server_id: row.server_id,
        service_id: row.service_id,
        target_kind: row.target_kind,
        target_spec: row.target_spec,
        schedule_cron: row.schedule_cron,
        timezone: row.timezone,
        destination_ids: row.destination_ids,
        path_prefix: row.path_prefix,
        filename_template: row.filename_template,
        retention: row.retention_spec,
        compression: row.compression,
        compression_level: row.compression_level,
        encrypted: row.encrypted,
        tool_options: row.tool_options,
        consistency: row.consistency_spec,
        timeout_secs: row.timeout_secs,
        retries: row.retries,
        bandwidth_limit_bps: row.bandwidth_limit_bps,
        verify_after_upload: row.verify_after_upload,
        notify_on_success: row.notify_on_success,
        enabled: row.enabled,
        last_status: row.last_status,
        last_run_at: row.last_run_at,
        created_at: row.created_at,
    }
}

async fn load_jobs(
    state: &AppState,
    id: Option<Uuid>,
    service_id: Option<Uuid>,
) -> Result<Vec<JobRow>, ApiError> {
    sqlx::query_as!(
        JobRow,
        r#"
        SELECT j.id, j.name, j.server_id, j.service_id, j.target_kind, j.target_spec,
               j.schedule_cron, j.timezone, j.destination_ids, j.path_prefix,
               j.filename_template, j.retention_spec, j.compression, j.compression_level,
               (j.encryption_passphrase_enc IS NOT NULL) AS "encrypted!", j.tool_options,
               j.consistency_spec, j.timeout_secs, j.retries, j.bandwidth_limit_bps,
               j.verify_after_upload, j.notify_on_success, j.enabled, j.created_at,
               last.status AS "last_status?", last.created_at AS "last_run_at?"
        FROM backup_jobs j
        LEFT JOIN LATERAL (
            SELECT r.status, r.created_at FROM backup_runs r
            WHERE r.job_id = j.id AND r.kind = 'backup'
            ORDER BY r.created_at DESC LIMIT 1
        ) last ON TRUE
        WHERE ($1::uuid IS NULL OR j.id = $1) AND ($2::uuid IS NULL OR j.service_id = $2)
        ORDER BY j.name
        "#,
        id,
        service_id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)
}

async fn list_jobs(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(query): Query<JobQuery>,
) -> Result<Json<Vec<BackupJobView>>, ApiError> {
    let rows = load_jobs(&state, None, query.service_id).await?;
    Ok(Json(rows.into_iter().map(job_view).collect()))
}

async fn get_job(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<BackupJobView>, ApiError> {
    let row = load_jobs(&state, Some(id), None)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| not_found("backup job not found"))?;
    Ok(Json(job_view(row)))
}

/// Merged and validated job fields, ready to write.
struct JobFields {
    name: String,
    target_kind: String,
    server_id: Option<Uuid>,
    service_id: Option<Uuid>,
    target_spec: Value,
    schedule_cron: Option<String>,
    timezone: String,
    destination_ids: Vec<Uuid>,
    path_prefix: String,
    filename_template: String,
    retention: RetentionSpec,
    compression: Compression,
    compression_level: i32,
    tool_options: ToolOptions,
    consistency: Consistency,
    timeout_secs: i32,
    retries: i32,
    bandwidth_limit_bps: Option<i64>,
    verify_after_upload: bool,
    notify_on_success: bool,
    enabled: bool,
}

async fn validate_fields(state: &AppState, fields: &JobFields) -> Result<(), ApiError> {
    if fields.name.trim().is_empty() || fields.name.len() > 100 {
        return Err(bad("invalid", "job name must be 1 to 100 characters"));
    }
    if !matches!(
        fields.target_kind.as_str(),
        "database" | "volume" | "bind" | "path" | "panel_db" | "panel_settings"
    ) {
        return Err(bad(
            "invalid",
            "target_kind must be database, volume, bind, path, panel_db, or panel_settings",
        ));
    }
    if let Some(cron) = &fields.schedule_cron {
        validate_schedule(cron, &fields.timezone).map_err(core_error)?;
    } else {
        rudder_core::parse_timezone(&fields.timezone).map_err(core_error)?;
    }
    if fields.destination_ids.is_empty() {
        return Err(bad("invalid", "choose at least one S3 destination"));
    }
    let known = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM s3_destinations WHERE id = ANY($1)"#,
        &fields.destination_ids
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if known != fields.destination_ids.len() as i64 {
        return Err(not_found("S3 destination not found"));
    }
    validate_prefix(&fields.path_prefix).map_err(core_error)?;
    rudder_core::object_key(
        &fields.path_prefix,
        &fields.filename_template,
        &rudder_core::KeyVars {
            name: "job",
            service: "service",
            engine: "engine",
            unix: 0,
        },
        "",
    )
    .map_err(core_error)?;
    fields
        .compression
        .validate_level(fields.compression_level)
        .map_err(core_error)?;
    validate_tool_options(&fields.tool_options).map_err(core_error)?;
    fields.consistency.validate().map_err(core_error)?;
    if !(10..=86_400).contains(&fields.timeout_secs) {
        return Err(bad("invalid", "timeout must be 10 to 86400 seconds"));
    }
    if !(0..=10).contains(&fields.retries) {
        return Err(bad("invalid", "retries must be 0 to 10"));
    }
    if fields.bandwidth_limit_bps.is_some_and(|bps| bps < 1024) {
        return Err(bad(
            "invalid",
            "bandwidth limit must be at least 1024 bytes/s",
        ));
    }
    let service_kind = match fields.service_id {
        Some(id) => Some(
            sqlx::query_scalar!(r#"SELECT kind FROM services WHERE id = $1"#, id)
                .fetch_optional(&state.pool)
                .await
                .map_err(ApiError::database)?
                .ok_or_else(|| not_found("service not found"))?,
        ),
        None => None,
    };
    let spec_text = |key: &str| fields.target_spec.get(key).and_then(Value::as_str);
    match fields.target_kind.as_str() {
        "database" => {
            if service_kind.as_deref() != Some("database") {
                return Err(bad("invalid", "database jobs need a database service"));
            }
            if fields.consistency.stop_service {
                return Err(bad(
                    "invalid",
                    "dump tools run inside the database, so it cannot be stopped",
                ));
            }
        }
        "volume" => {
            match spec_text("volume") {
                Some(name) => ArchiveSource::Volume { name: name.into() }
                    .validate()
                    .map_err(core_error)?,
                None if service_kind.as_deref() == Some("database") => {}
                None => return Err(bad("invalid", "set target_spec.volume")),
            }
            if fields.service_id.is_none() && fields.server_id.is_none() {
                return Err(bad("invalid", "volume jobs need a server or a service"));
            }
        }
        "bind" => {
            if fields.service_id.is_none() {
                return Err(bad("invalid", "bind jobs need a service"));
            }
            validate_host_path(spec_text("source").unwrap_or("")).map_err(core_error)?;
        }
        "path" => {
            if fields.server_id.is_none() {
                return Err(bad("invalid", "path jobs need a server"));
            }
            validate_host_path(spec_text("path").unwrap_or("")).map_err(core_error)?;
        }
        "panel_db" | "panel_settings" => {
            if fields.server_id.is_some() || fields.service_id.is_some() {
                return Err(bad(
                    "invalid",
                    "panel backups belong to the panel, not to a server or service",
                ));
            }
            if fields.consistency.stop_service
                || !fields.consistency.pre_hook.is_empty()
                || !fields.consistency.post_hook.is_empty()
            {
                return Err(bad(
                    "invalid",
                    "panel backups have no service to stop or hook",
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

/// Panel database backups carry the master key, so they must stay encrypted.
fn check_panel_passphrase(
    target_kind: &str,
    new_value: Option<&str>,
    already_encrypted: bool,
) -> Result<(), ApiError> {
    if target_kind != "panel_db" {
        return Ok(());
    }
    match new_value {
        Some("") => Err(bad(
            "invalid",
            "panel database backups must stay encrypted",
        )),
        Some(value) => crate::panel::validate_passphrase(value).map_err(core_error),
        None if already_encrypted => Ok(()),
        None => Err(bad(
            "invalid",
            "panel database backups include the master key; set an encryption passphrase of at least 12 characters",
        )),
    }
}

fn merge(existing: Option<&BackupJobView>, body: &SaveBackupJob) -> Result<JobFields, ApiError> {
    let pick_str = |new: &Option<String>, old: Option<&str>, default: &str| {
        new.clone()
            .or_else(|| old.map(str::to_owned))
            .unwrap_or_else(|| default.to_owned())
    };
    let schedule_cron = match &body.schedule_cron {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => Some(value.trim().to_owned()),
        None => existing.and_then(|job| job.schedule_cron.clone()),
    };
    let compression = Compression::parse(&pick_str(
        &body.compression,
        existing.map(|job| job.compression.as_str()),
        "zstd",
    ))
    .map_err(core_error)?;
    let parse = |value: Option<&Value>| value.cloned().unwrap_or_else(|| json!({}));
    Ok(JobFields {
        name: pick_str(&body.name, existing.map(|job| job.name.as_str()), ""),
        target_kind: pick_str(
            &body.target_kind,
            existing.map(|job| job.target_kind.as_str()),
            "",
        ),
        server_id: body.server_id.or(existing.and_then(|job| job.server_id)),
        service_id: body.service_id.or(existing.and_then(|job| job.service_id)),
        target_spec: body
            .target_spec
            .clone()
            .unwrap_or_else(|| parse(existing.map(|job| &job.target_spec))),
        schedule_cron,
        timezone: pick_str(
            &body.timezone,
            existing.map(|job| job.timezone.as_str()),
            "UTC",
        ),
        destination_ids: body
            .destination_ids
            .clone()
            .or_else(|| existing.map(|job| job.destination_ids.clone()))
            .unwrap_or_default(),
        path_prefix: pick_str(
            &body.path_prefix,
            existing.map(|job| job.path_prefix.as_str()),
            "rudder",
        ),
        filename_template: pick_str(
            &body.filename_template,
            existing.map(|job| job.filename_template.as_str()),
            "{name}-{timestamp}",
        ),
        retention: match body.retention {
            Some(retention) => retention,
            None => existing
                .and_then(|job| serde_json::from_value(job.retention.clone()).ok())
                .unwrap_or_default(),
        },
        compression,
        compression_level: body
            .compression_level
            .or(existing.map(|job| job.compression_level))
            .unwrap_or(3),
        tool_options: match &body.tool_options {
            Some(options) => options.clone(),
            None => existing
                .and_then(|job| serde_json::from_value(job.tool_options.clone()).ok())
                .unwrap_or_default(),
        },
        consistency: match &body.consistency {
            Some(consistency) => consistency.clone(),
            None => existing
                .and_then(|job| serde_json::from_value(job.consistency.clone()).ok())
                .unwrap_or_default(),
        },
        timeout_secs: body
            .timeout_secs
            .or(existing.map(|job| job.timeout_secs))
            .unwrap_or(3600),
        retries: body
            .retries
            .or(existing.map(|job| job.retries))
            .unwrap_or(2),
        bandwidth_limit_bps: body.bandwidth_limit_bps.filter(|bps| *bps > 0).or(
            if body.bandwidth_limit_bps == Some(0) {
                None
            } else {
                existing.and_then(|job| job.bandwidth_limit_bps)
            },
        ),
        verify_after_upload: body
            .verify_after_upload
            .or(existing.map(|job| job.verify_after_upload))
            .unwrap_or(false),
        notify_on_success: body
            .notify_on_success
            .or(existing.map(|job| job.notify_on_success))
            .unwrap_or(false),
        enabled: body
            .enabled
            .or(existing.map(|job| job.enabled))
            .unwrap_or(true),
    })
}

fn encrypt_passphrase(state: &AppState, value: &str) -> Result<Vec<u8>, ApiError> {
    if value.len() < 8 || value.len() > 1024 {
        return Err(bad(
            "invalid",
            "encryption passphrase must be 8 to 1024 characters",
        ));
    }
    state.cipher.encrypt(value.as_bytes()).map_err(core_error)
}

async fn create_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SaveBackupJob>,
) -> Result<(StatusCode, Json<BackupJobView>), ApiError> {
    auth.require_write()?;
    let fields = merge(None, &body)?;
    validate_fields(&state, &fields).await?;
    check_panel_passphrase(
        &fields.target_kind,
        body.encryption_passphrase
            .as_deref()
            .filter(|value| !value.is_empty()),
        false,
    )?;
    let passphrase = match body
        .encryption_passphrase
        .as_deref()
        .filter(|p| !p.is_empty())
    {
        Some(value) => Some(encrypt_passphrase(&state, value)?),
        None => None,
    };
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO backup_jobs (
            name, server_id, service_id, target_kind, target_spec, schedule_cron, timezone,
            destination_ids, path_prefix, filename_template, retention_spec, compression,
            compression_level, encryption_passphrase_enc, tool_options, consistency_spec,
            timeout_secs, retries, bandwidth_limit_bps, verify_after_upload, notify_on_success,
            enabled
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17,
                $18, $19, $20, $21, $22)
        RETURNING id
        "#,
        fields.name,
        fields.server_id,
        fields.service_id,
        fields.target_kind,
        fields.target_spec,
        fields.schedule_cron,
        fields.timezone,
        &fields.destination_ids,
        fields.path_prefix,
        fields.filename_template,
        serde_json::to_value(fields.retention).unwrap_or_else(|_| json!({})),
        fields.compression.as_str(),
        fields.compression_level,
        passphrase,
        serde_json::to_value(&fields.tool_options).unwrap_or_else(|_| json!({})),
        serde_json::to_value(&fields.consistency).unwrap_or_else(|_| json!({})),
        fields.timeout_secs,
        fields.retries,
        fields.bandwidth_limit_bps,
        fields.verify_after_upload,
        fields.notify_on_success,
        fields.enabled,
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "backup_job.create",
        "backup_job",
        Some(&id.to_string()),
        json!({ "target_kind": fields.target_kind, "encrypted": passphrase_set(&body) }),
        None,
    )
    .await;
    let row = load_jobs(&state, Some(id), None)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| not_found("backup job not found"))?;
    Ok((StatusCode::CREATED, Json(job_view(row))))
}

fn passphrase_set(body: &SaveBackupJob) -> bool {
    body.encryption_passphrase
        .as_deref()
        .is_some_and(|p| !p.is_empty())
}

async fn update_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveBackupJob>,
) -> Result<Json<BackupJobView>, ApiError> {
    auth.require_write()?;
    let existing = load_jobs(&state, Some(id), None)
        .await?
        .into_iter()
        .next()
        .map(job_view)
        .ok_or_else(|| not_found("backup job not found"))?;
    let fields = merge(Some(&existing), &body)?;
    validate_fields(&state, &fields).await?;
    check_panel_passphrase(
        &fields.target_kind,
        body.encryption_passphrase.as_deref(),
        existing.encrypted,
    )?;
    let (set_passphrase, passphrase) = match body.encryption_passphrase.as_deref() {
        None => (false, None),
        Some("") => (true, None),
        Some(value) => (true, Some(encrypt_passphrase(&state, value)?)),
    };
    sqlx::query!(
        r#"
        UPDATE backup_jobs SET
            name = $2, server_id = $3, service_id = $4, target_kind = $5, target_spec = $6,
            schedule_cron = $7, timezone = $8, destination_ids = $9, path_prefix = $10,
            filename_template = $11, retention_spec = $12, compression = $13,
            compression_level = $14,
            encryption_passphrase_enc = CASE WHEN $15 THEN $16 ELSE encryption_passphrase_enc END,
            tool_options = $17, consistency_spec = $18, timeout_secs = $19, retries = $20,
            bandwidth_limit_bps = $21, verify_after_upload = $22, notify_on_success = $23,
            enabled = $24, updated_at = now()
        WHERE id = $1
        "#,
        id,
        fields.name,
        fields.server_id,
        fields.service_id,
        fields.target_kind,
        fields.target_spec,
        fields.schedule_cron,
        fields.timezone,
        &fields.destination_ids,
        fields.path_prefix,
        fields.filename_template,
        serde_json::to_value(fields.retention).unwrap_or_else(|_| json!({})),
        fields.compression.as_str(),
        fields.compression_level,
        set_passphrase,
        passphrase,
        serde_json::to_value(&fields.tool_options).unwrap_or_else(|_| json!({})),
        serde_json::to_value(&fields.consistency).unwrap_or_else(|_| json!({})),
        fields.timeout_secs,
        fields.retries,
        fields.bandwidth_limit_bps,
        fields.verify_after_upload,
        fields.notify_on_success,
        fields.enabled,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "backup_job.update",
        "backup_job",
        Some(&id.to_string()),
        json!({ "passphrase_changed": set_passphrase }),
        None,
    )
    .await;
    let row = load_jobs(&state, Some(id), None)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| not_found("backup job not found"))?;
    Ok(Json(job_view(row)))
}

async fn delete_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let result = sqlx::query!(r#"DELETE FROM backup_jobs WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if result.rows_affected() == 0 {
        return Err(not_found("backup job not found"));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "backup_job.delete",
        "backup_job",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn run_job(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<RunAccepted>), ApiError> {
    auth.require_write()?;
    let run_id = backup::start_run(&state, id, "manual").await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "backup_job.run",
        "backup_job",
        Some(&id.to_string()),
        json!({ "run_id": run_id }),
        None,
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(RunAccepted { run_id })))
}

struct RunRow {
    id: Uuid,
    job_id: Uuid,
    kind: String,
    trigger: String,
    status: String,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    size_bytes: Option<i64>,
    checksum: Option<String>,
    object_keys: Vec<String>,
    destination_ids: Vec<Uuid>,
    meta: Value,
    logs: String,
    error: Option<String>,
}

fn run_view(row: RunRow) -> BackupRunView {
    BackupRunView {
        duration_ms: row
            .finished_at
            .map(|finished| (finished - row.started_at).num_milliseconds()),
        id: row.id,
        job_id: row.job_id,
        kind: row.kind,
        trigger: row.trigger,
        status: row.status,
        started_at: row.started_at,
        finished_at: row.finished_at,
        size_bytes: row.size_bytes,
        checksum: row.checksum,
        object_keys: row.object_keys,
        destination_ids: row.destination_ids,
        meta: row.meta,
        logs: row.logs,
        error: row.error,
    }
}

async fn list_runs(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<Vec<BackupRunView>>, ApiError> {
    let rows = sqlx::query_as!(
        RunRow,
        r#"
        SELECT id, job_id, kind, trigger, status, started_at, finished_at, size_bytes, checksum,
               object_keys, destination_ids, meta, logs, error
        FROM backup_runs
        WHERE job_id = $1
        ORDER BY created_at DESC
        LIMIT $2
        "#,
        id,
        query.limit.unwrap_or(50).clamp(1, 500)
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(rows.into_iter().map(run_view).collect()))
}

async fn get_run(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<BackupRunView>, ApiError> {
    let row = sqlx::query_as!(
        RunRow,
        r#"
        SELECT id, job_id, kind, trigger, status, started_at, finished_at, size_bytes, checksum,
               object_keys, destination_ids, meta, logs, error
        FROM backup_runs WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("backup run not found"))?;
    Ok(Json(run_view(row)))
}

/// Stream the stored object (compressed and, if configured, encrypted).
async fn download_run(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Response, ApiError> {
    auth.require_admin()?;
    let source = backup::load_source_run(&state, id).await?;
    let destination = backup::first_destination(&state, &source.destination_ids).await?;
    let stream = s3::download(destination.store.clone(), &source.key)
        .await
        .map_err(core_error)?;
    let filename = source.key.rsplit('/').next().unwrap_or("backup").to_owned();
    audit::record(
        &state.pool,
        Some(auth.id),
        "backup_run.download",
        "backup_run",
        Some(&id.to_string()),
        json!({ "key": source.key }),
        None,
    )
    .await;
    let body = Body::from_stream(stream.map(|item| item.map_err(std::io::Error::other)));
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(body)
        .map_err(|_| bad("invalid", "could not build the download"))
}

async fn restore_run(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RestoreRequest>,
) -> Result<(StatusCode, Json<RunAccepted>), ApiError> {
    auth.require_write()?;
    let target = json!({
        "service_id": body.target_service_id,
        "volume": body.target_volume,
        "path": body.target_path,
    });
    let run_id = backup::start_restore(&state, id, body).await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "backup_run.restore",
        "backup_run",
        Some(&id.to_string()),
        json!({ "restore_run_id": run_id, "target": target }),
        None,
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(RunAccepted { run_id })))
}

async fn list_events(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(query): Query<LimitQuery>,
) -> Result<Json<Vec<EventView>>, ApiError> {
    let rows = sqlx::query_as!(
        EventView,
        r#"
        SELECT id, kind, severity, server_id, service_id, title, message, payload, created_at,
               delivered_at
        FROM system_events
        ORDER BY created_at DESC
        LIMIT $1
        "#,
        query.limit.unwrap_or(100).clamp(1, 1000)
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(rows))
}
