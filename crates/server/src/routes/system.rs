//! Panel disaster recovery endpoints: version info, settings export, and panel backups.

use axum::body::Body;
use axum::extract::State;
use axum::http::header;
use axum::response::Response;
use axum::routing::{get, post};
use axum::Json;
use chrono::Utc;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::audit;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::panel;
use crate::state::AppState;

use super::catalog::{bad, core_error};

#[derive(Serialize, ToSchema)]
pub struct SystemInfo {
    pub version: String,
    /// Highest applied schema migration.
    pub schema: i64,
    /// Highest migration this binary carries.
    pub known_schema: i64,
    /// `pg_dump --version`, or `None` when it cannot run.
    pub pg_dump: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct PanelBackupRequest {
    /// At least 12 characters. Needed to restore; it is not stored.
    pub passphrase: String,
    /// Also include log lines, metric samples, and sessions.
    #[serde(default)]
    pub include_history: bool,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/system", get(info))
        .route("/api/v1/system/settings-export", get(settings_export))
        .route("/api/v1/system/backup", post(backup))
}

async fn info(State(state): State<AppState>, auth: AuthUser) -> Result<Json<SystemInfo>, ApiError> {
    auth.require_admin()?;
    let schema = sqlx::query_scalar!(
        r#"SELECT COALESCE(MAX(version), 0) AS "version!" FROM _sqlx_migrations WHERE success"#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let pg_dump = tokio::process::Command::new(&state.config.pg_dump)
        .arg("--version")
        .output()
        .await
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    Ok(Json(SystemInfo {
        version: rudder_core::VERSION.into(),
        schema,
        known_schema: crate::db::known_migration(),
        pg_dump,
    }))
}

async fn settings_export(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Response, ApiError> {
    auth.require_admin()?;
    let export = panel::settings_export(&state.pool).await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "panel.settings_export",
        "panel",
        None,
        json!({}),
        None,
    )
    .await;
    let body = serde_json::to_vec_pretty(&export)
        .map_err(|_| bad("invalid", "could not serialize the export"))?;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"rudder-settings-{}.json\"",
                Utc::now().format("%Y%m%d-%H%M%S")
            ),
        )
        .body(Body::from(body))
        .map_err(|_| bad("invalid", "could not build the download"))
}

async fn backup(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<PanelBackupRequest>,
) -> Result<Response, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    panel::validate_passphrase(&body.passphrase).map_err(core_error)?;
    let archive = panel::archive(&state.pool, &state.config, body.include_history)
        .await
        .map_err(core_error)?;
    let stream = panel::seal(archive, &body.passphrase).map_err(core_error)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "panel.backup_download",
        "panel",
        None,
        json!({ "include_history": body.include_history }),
        None,
    )
    .await;
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", panel::file_name(Utc::now())),
        )
        .body(Body::from_stream(
            stream.map(|item| item.map_err(std::io::Error::other)),
        ))
        .map_err(|_| bad("invalid", "could not build the download"))
}
