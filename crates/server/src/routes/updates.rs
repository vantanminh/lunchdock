//! Self-update endpoints under `/api/v1/system/updates`. Admin only.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::update::{self, ApplyError, ComponentStatus, UpdateCheck, UpdateHistoryEntry};

use super::catalog::{bad, conflict, core_error};

#[derive(Serialize, ToSchema)]
pub struct UpdateOverview {
    pub current_version: String,
    /// `stable` or `prerelease`.
    pub channel: String,
    pub auto_update: bool,
    /// `HH:MM-HH:MM` in UTC; automatic updates wait for it. Empty means any time.
    pub maintenance_window: Option<String>,
    pub repository: String,
    /// A release signing key is configured, so verified releases can be applied.
    pub signing_key: bool,
    /// Why "Update now" is unavailable, if it is.
    pub apply_blocker: Option<String>,
    /// Cache image for this panel version, from its signed release.
    pub target_cache_image: Option<String>,
    pub last_check: Option<UpdateCheck>,
    pub components: Vec<ComponentStatus>,
    pub history: Vec<UpdateHistoryEntry>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateSettings {
    /// `stable` or `prerelease`.
    pub channel: String,
    pub auto_update: bool,
    pub maintenance_window: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct UpdateStarted {
    pub history_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct ComponentsStarted {
    /// Agents and caches queued for an update.
    pub queued: usize,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/system/updates", get(overview))
        .route("/api/v1/system/updates/check", post(check))
        .route("/api/v1/system/updates/settings", put(settings))
        .route("/api/v1/system/updates/apply", post(apply))
        .route("/api/v1/system/updates/components", post(components))
}

fn apply_error(error: ApplyError) -> ApiError {
    match error {
        ApplyError::Conflict(message) => conflict("update_unavailable", message),
        ApplyError::Failed(error) => core_error(error),
    }
}

async fn overview(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<UpdateOverview>, ApiError> {
    auth.require_admin()?;
    let settings = sqlx::query!(
        r#"
        SELECT github_repo, update_channel, auto_update, maintenance_window
        FROM panel_settings WHERE id = TRUE
        "#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let last_check = update::last_public_check(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let components = update::components(&state).await.map_err(core_error)?;
    let history = update::history(&state.pool, 30)
        .await
        .map_err(ApiError::database)?;
    Ok(Json(UpdateOverview {
        current_version: rudder_core::VERSION.into(),
        channel: settings.update_channel,
        auto_update: settings.auto_update,
        maintenance_window: settings.maintenance_window,
        repository: settings.github_repo,
        signing_key: state.config.update_public_key.is_some(),
        apply_blocker: update::apply_blocker(&state).await,
        target_cache_image: update::target_cache_image(&state.pool)
            .await
            .map_err(ApiError::database)?,
        last_check,
        components,
        history,
    }))
}

async fn check(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<UpdateCheck>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let check = update::check(&state).await.map_err(core_error)?;
    Ok(Json(update::UpdateCheck {
        manifest: None,
        ..check
    }))
}

async fn settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<UpdateSettings>,
) -> Result<Json<UpdateOverview>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let channel = rudder_core::UpdateChannel::parse(&body.channel).map_err(core_error)?;
    let window = body
        .maintenance_window
        .map(|w| w.trim().to_owned())
        .filter(|w| !w.is_empty());
    if let Some(window) = &window {
        rudder_core::parse_maintenance_window(window)
            .map_err(|error| bad("invalid_window", error.to_string()))?;
    }
    sqlx::query!(
        r#"
        UPDATE panel_settings
        SET update_channel = $1, auto_update = $2, maintenance_window = $3, updated_at = now()
        WHERE id = TRUE
        "#,
        channel.as_str(),
        body.auto_update,
        window,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "panel.update_settings",
        "panel",
        None,
        json!({ "channel": channel.as_str(), "auto_update": body.auto_update, "maintenance_window": window }),
        None,
    )
    .await;
    overview(State(state), auth).await
}

async fn apply(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<(StatusCode, Json<UpdateStarted>), ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let id = update::apply_panel(&state, Some(auth.id))
        .await
        .map_err(apply_error)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "panel.update",
        "update",
        Some(&id.to_string()),
        json!({ "from_version": rudder_core::VERSION }),
        None,
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(UpdateStarted { history_id: id })))
}

async fn components(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<(StatusCode, Json<ComponentsStarted>), ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let queued = update::update_components(&state, Some(auth.id))
        .await
        .map_err(apply_error)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "panel.update_components",
        "update",
        None,
        json!({ "queued": queued, "to_version": rudder_core::VERSION }),
        None,
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(ComponentsStarted { queued })))
}
