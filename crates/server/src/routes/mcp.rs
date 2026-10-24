//! Browser approval and the list of agents allowed to control the panel.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use chrono::{DateTime, Utc};
use rudder_core::{validate_agent_scopes, MCP_SCOPES, MCP_TEMPLATES};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::mcp::{self, http};
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct McpScopeView {
    pub id: String,
    pub group: String,
    pub write: bool,
    pub summary: String,
}

#[derive(Serialize, ToSchema)]
pub struct McpTemplateView {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub scopes: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct McpCatalog {
    pub scopes: Vec<McpScopeView>,
    pub templates: Vec<McpTemplateView>,
}

#[derive(Serialize, ToSchema)]
pub struct McpInfo {
    /// Path to append to the panel address. The browser adds the origin.
    pub path: String,
}

#[derive(Serialize, ToSchema)]
pub struct McpClientView {
    pub client_id: String,
    pub client_name: String,
    pub redirect_uris: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct McpGrantView {
    pub id: Uuid,
    pub name: String,
    pub client_name: String,
    pub template: String,
    pub scopes: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, ToSchema)]
pub struct McpConsentRequest {
    pub client_id: String,
    pub redirect_uri: String,
    pub code_challenge: String,
    pub code_challenge_method: String,
    pub state: String,
    /// `full`, `read-all`, `operator`, `observer`, or `custom`.
    pub template: String,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub resource: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct McpConsentResponse {
    /// Send the browser here. It carries the one-time code.
    pub redirect: String,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/mcp/info", get(info))
        .route("/api/v1/mcp/catalog", get(catalog))
        .route("/api/v1/mcp/clients/{client_id}", get(client))
        .route("/api/v1/mcp/grants", get(grants))
        .route("/api/v1/mcp/grants/{id}", axum::routing::delete(revoke))
        .route("/api/v1/mcp/consent", post(consent))
}

async fn info(_auth: AuthUser) -> Json<McpInfo> {
    Json(McpInfo {
        path: "/mcp".into(),
    })
}

async fn catalog(auth: AuthUser) -> Result<Json<McpCatalog>, ApiError> {
    auth.require_admin()?;
    Ok(Json(McpCatalog {
        scopes: MCP_SCOPES
            .iter()
            .map(|scope| McpScopeView {
                id: scope.id.into(),
                group: scope.group.into(),
                write: scope.write,
                summary: scope.summary.into(),
            })
            .collect(),
        templates: MCP_TEMPLATES
            .iter()
            .map(|template| McpTemplateView {
                id: template.id.into(),
                name: template.name.into(),
                summary: template.summary.into(),
                scopes: rudder_core::scopes_for_template(template.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
            })
            .collect(),
    }))
}

async fn client(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(client_id): Path<String>,
) -> Result<Json<McpClientView>, ApiError> {
    auth.require_admin()?;
    let row = sqlx::query!(
        r#"SELECT client_id, client_name, redirect_uris FROM oauth_clients WHERE client_id = $1"#,
        client_id,
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "unknown agent client",
        ));
    };
    Ok(Json(McpClientView {
        client_id: row.client_id,
        client_name: row.client_name,
        redirect_uris: row.redirect_uris,
    }))
}

async fn grants(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<McpGrantView>>, ApiError> {
    auth.require_admin()?;
    let rows = sqlx::query!(
        r#"
        SELECT g.id, g.name, g.template, g.scopes, g.created_at, g.last_used_at, c.client_name
        FROM mcp_grants g
        JOIN oauth_clients c ON c.client_id = g.client_id
        WHERE g.revoked_at IS NULL
        ORDER BY g.created_at DESC
        LIMIT 200
        "#,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| McpGrantView {
                id: row.id,
                name: row.name,
                client_name: row.client_name,
                template: row.template,
                scopes: row.scopes,
                created_at: row.created_at,
                last_used_at: row.last_used_at,
            })
            .collect(),
    ))
}

async fn revoke(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    if !mcp::tokens::revoke_grant_id(&state, id).await? {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "approval not found",
        ));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "mcp.revoke",
        "mcp_grant",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn consent(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<McpConsentRequest>,
) -> Result<Json<McpConsentResponse>, ApiError> {
    if auth.via_token || auth.via_mcp {
        return Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "approve an agent from a signed-in browser",
        ));
    }
    auth.require_admin()?;
    http::validate_login(
        "code",
        &body.code_challenge_method,
        &body.code_challenge,
        &body.state,
        body.resource.as_deref(),
    )?;
    let (client_name, redirect_uri) =
        http::load_client_redirect(&state, &body.client_id, &body.redirect_uri).await?;
    let scopes = validate_agent_scopes(&body.template, &body.scopes)
        .map_err(|message| ApiError::new(StatusCode::BAD_REQUEST, "invalid_scopes", message))?;
    let name = {
        let trimmed = body.name.trim();
        let chosen = if trimmed.is_empty() {
            client_name.as_str()
        } else {
            trimmed
        };
        if chosen.is_empty() || chosen.len() > 80 || chosen.chars().any(|c| c.is_control()) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid",
                "name must be 1 to 80 characters",
            ));
        }
        chosen.to_owned()
    };
    let code = mcp::tokens::store_code(
        &state,
        auth.id,
        &body.client_id,
        &name,
        &body.template,
        &scopes,
        &redirect_uri,
        &body.code_challenge,
    )
    .await?;
    http::record_grant(&state, &auth, &body.client_id, &body.template).await;
    let redirect = http::redirect_with(&redirect_uri, &code, &body.state)?;
    Ok(Json(McpConsentResponse { redirect }))
}
