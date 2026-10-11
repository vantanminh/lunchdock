use axum::extract::State;
use axum::routing::get;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub product: &'static str,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/api/health", get(health))
}

async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, ApiError> {
    crate::db::database_reachable(&state.pool)
        .await
        .map_err(ApiError::database)?;
    Ok(Json(HealthResponse {
        status: "ok",
        version: rudder_core::VERSION,
        product: rudder_core::PRODUCT,
    }))
}
