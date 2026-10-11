//! Pick the [`NodeDriver`] for a server row.

use std::sync::Arc;

use rudder_core::NodeDriver;
use uuid::Uuid;

use crate::error::ApiError;
use crate::routes::catalog::{conflict, not_found};
use crate::state::AppState;

pub async fn server_kind(state: &AppState, server_id: Uuid) -> Result<String, ApiError> {
    sqlx::query_scalar!(r#"SELECT kind FROM servers WHERE id = $1"#, server_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| not_found("server not found"))
}

/// Local servers use the Docker socket. Remote servers use their agent session.
pub async fn driver_for(
    state: &AppState,
    server_id: Uuid,
) -> Result<Arc<dyn NodeDriver>, ApiError> {
    let kind = server_kind(state, server_id).await?;
    if kind == "local" {
        return Ok(state.local_driver.clone());
    }
    state.agents.driver(server_id).ok_or_else(|| {
        conflict(
            "remote_unavailable",
            "the agent for this server is not connected",
        )
    })
}
