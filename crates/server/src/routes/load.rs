//! Load guard endpoints: what the guard sees on the panel host, its settings,
//! and the rescue actions an admin needs when the server is overloaded.

use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post, put};
use axum::Json;
use chrono::{DateTime, Utc};
use rudder_core::GuardLimits;
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;

use crate::audit;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::events::{self, Event};
use crate::guard::{self, GuardSettings, GuardView, ProtectionView};
use crate::state::AppState;

use super::catalog::{bad, core_error};

/// Load of the panel host and what the guard does about it.
#[derive(Serialize, ToSchema)]
pub struct LoadView {
    pub guard: GuardView,
    pub builds: BuildLoad,
    /// How the panel, its database and Traefik are protected from apps.
    pub protection: Vec<ProtectionView>,
}

#[derive(Serialize, ToSchema)]
pub struct BuildLoad {
    pub queued: i64,
    pub running: i64,
    /// Compose stack actions running now.
    pub stack_actions: i64,
    /// Of those, the ones that build images on the panel host.
    pub stack_builds: i64,
    /// Why queued builds wait: `paused`, `busy`, or `critical`; `None` when they may start.
    pub hold: Option<String>,
    pub oldest_queued_at: Option<DateTime<Utc>>,
    /// Builds that may run at the same time on one server.
    pub concurrency: i32,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateGuard {
    /// Hold and stop builds when the panel host is overloaded.
    pub enabled: bool,
    /// 5 to 50: new builds wait while less memory than this is free.
    pub memory_reserve_percent: u8,
    /// 50 to 100: new builds wait while CPU stays above this.
    pub cpu_percent: u8,
    /// 1 to 8 builds at the same time on one server.
    pub build_concurrency: Option<i32>,
}

#[derive(Deserialize, ToSchema)]
pub struct PauseBuilds {
    pub paused: bool,
}

#[derive(Serialize, ToSchema)]
pub struct BuildsCancelled {
    /// Queued builds that will not run.
    pub queued: i64,
    /// Running builds and stack builds that were asked to stop.
    pub running: i64,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/load", get(overview))
        .route("/api/v1/load/settings", put(update_settings))
        .route("/api/v1/load/pause", post(pause))
        .route("/api/v1/load/cancel-builds", post(cancel_builds))
}

async fn overview(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<LoadView>, ApiError> {
    load_view(&state).await.map(Json)
}

async fn load_view(state: &AppState) -> Result<LoadView, ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT
            COUNT(*) FILTER (WHERE status = 'queued' AND NOT cancel_requested) AS "queued!",
            COUNT(*) FILTER (WHERE status = 'running') AS "running!",
            MIN(created_at) FILTER (WHERE status = 'queued' AND NOT cancel_requested) AS oldest,
            (SELECT build_concurrency_default FROM panel_settings WHERE id = TRUE) AS "concurrency!"
        FROM build_queue
        WHERE status IN ('queued', 'running')
        "#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let waited = row
        .oldest
        .and_then(|at| (Utc::now() - at).to_std().ok())
        .unwrap_or(Duration::ZERO);
    let (stack_actions, stack_builds) = state.stack_runs.counts();
    Ok(LoadView {
        guard: state.guard.view(),
        builds: BuildLoad {
            queued: row.queued,
            running: row.running,
            stack_actions: stack_actions as i64,
            stack_builds: stack_builds as i64,
            hold: state.guard.hold(waited).map(str::to_owned),
            oldest_queued_at: row.oldest,
            concurrency: row.concurrency,
        },
        protection: state.guard.protection(),
    })
}

async fn update_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<UpdateGuard>,
) -> Result<Json<LoadView>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    GuardLimits {
        memory_reserve_percent: body.memory_reserve_percent,
        cpu_percent: body.cpu_percent,
    }
    .validate()
    .map_err(core_error)?;
    if let Some(concurrency) = body.build_concurrency {
        if !(1..=8).contains(&concurrency) {
            return Err(bad(
                "invalid_concurrency",
                "builds at the same time must be between 1 and 8",
            ));
        }
        sqlx::query!(
            r#"UPDATE panel_settings SET build_concurrency_default = $1 WHERE id = TRUE"#,
            concurrency
        )
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    }
    let settings = GuardSettings {
        enabled: body.enabled,
        builds_paused: state.guard.settings().builds_paused,
        memory_reserve_percent: body.memory_reserve_percent,
        cpu_percent: body.cpu_percent,
    };
    guard::save_settings(&state.pool, settings)
        .await
        .map_err(ApiError::database)?;
    state.guard.set_settings(settings);
    audit::record(
        &state.pool,
        Some(auth.id),
        "load.settings",
        "panel",
        None,
        json!({
            "enabled": settings.enabled,
            "memory_reserve_percent": settings.memory_reserve_percent,
            "cpu_percent": settings.cpu_percent,
            "build_concurrency": body.build_concurrency,
        }),
        None,
    )
    .await;
    load_view(&state).await.map(Json)
}

async fn pause(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<PauseBuilds>,
) -> Result<Json<LoadView>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let settings = GuardSettings {
        builds_paused: body.paused,
        ..state.guard.settings()
    };
    guard::save_settings(&state.pool, settings)
        .await
        .map_err(ApiError::database)?;
    state.guard.set_settings(settings);
    let (action, event) = if body.paused {
        (
            "load.pause_builds",
            Event::warning("guard.paused", "Builds paused")
                .message(format!("{} paused new builds.", auth.email)),
        )
    } else {
        (
            "load.resume_builds",
            Event::info("guard.resumed", "Builds resumed")
                .message(format!("{} resumed builds.", auth.email)),
        )
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        action,
        "panel",
        None,
        json!({}),
        None,
    )
    .await;
    events::record(&state.pool, event).await;
    load_view(&state).await.map(Json)
}

async fn cancel_builds(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<BuildsCancelled>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let reason = format!("cancelled by {} from the load panel", auth.email);
    let deployments = sqlx::query_scalar!(
        r#"
        UPDATE build_queue
        SET status = 'cancelled', cancel_requested = TRUE, finished_at = now()
        WHERE status = 'queued'
        RETURNING deployment_id
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    sqlx::query!(
        r#"
        UPDATE deployments
        SET status = 'cancelled', error = $2, finished_at = now()
        WHERE id = ANY($1) AND status = 'queued'
        "#,
        &deployments,
        reason
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let running = guard::stop_builds(&state, &reason).await;
    let cancelled = BuildsCancelled {
        queued: deployments.len() as i64,
        running: running as i64,
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "load.cancel_builds",
        "panel",
        None,
        json!({ "queued": cancelled.queued, "running": cancelled.running }),
        None,
    )
    .await;
    if cancelled.queued + cancelled.running > 0 {
        events::record(
            &state.pool,
            Event::warning("guard.builds_cancelled", "Builds cancelled").message(format!(
                "{} cancelled {} queued and {} running build(s).",
                auth.email, cancelled.queued, cancelled.running
            )),
        )
        .await;
    }
    Ok(Json(cancelled))
}
