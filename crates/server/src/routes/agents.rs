//! Agent enrollment, join tokens, and the agent installer.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::agent::{self, pki, EnrollError, AGENT_PORT};
use crate::audit;
use crate::auth::{self, AuthUser};
use crate::error::ApiError;
use crate::routes::catalog::{bad, conflict, not_found};
use crate::state::AppState;

const JOIN_TOKEN_HOURS: i64 = 1;
const MAX_CSR_BYTES: usize = 8 * 1024;
const INSTALL_SCRIPT: &str = include_str!("../../../../deploy/install-agent.sh");

#[derive(Deserialize, ToSchema)]
pub struct EnrollRequest {
    pub join_token: String,
    /// PKCS#10 request for the agent's own key, PEM.
    pub csr_pem: String,
    #[serde(default)]
    pub hostname: String,
}

#[derive(Serialize, ToSchema)]
pub struct EnrollResponse {
    pub server_id: Uuid,
    pub client_cert_pem: String,
    pub ca_pem: String,
    /// Where the agent dials its gRPC session.
    pub grpc_url: String,
    /// TLS server name to verify on that connection.
    pub tls_name: String,
}

#[derive(Serialize, ToSchema)]
pub struct JoinToken {
    pub join_token: String,
    pub join_command: String,
    pub expires_at: DateTime<Utc>,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/agent/enroll", post(enroll))
        .route("/api/v1/servers/{id}/join-token", post(rotate_join_token))
        .route("/install-agent.sh", get(install_script))
        .route("/downloads/{file}", get(download))
}

/// Base URL of the panel as the browser reached it, for commands shown to users.
pub fn panel_url(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(url) = &state.config.public_url {
        return url.trim_end_matches('/').to_owned();
    }
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .filter(|h| valid_host(h))
        .unwrap_or("localhost:3000");
    let scheme = if state.config.trust_proxy {
        headers
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .filter(|p| *p == "https" || *p == "http")
            .unwrap_or("http")
    } else {
        "http"
    };
    format!("{scheme}://{host}")
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 260
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
}

/// The one-line installer shown after adding a server. The token is the only secret.
pub fn join_command(state: &AppState, headers: &HeaderMap, token: &str) -> String {
    let panel = panel_url(state, headers);
    format!("curl -fsSL {panel}/install-agent.sh | sudo sh -s -- --server {panel} --token {token}")
}

fn grpc_url(state: &AppState, headers: &HeaderMap) -> String {
    if let Some(url) = &state.config.agent_url {
        return url.clone();
    }
    let panel = panel_url(state, headers);
    let authority = panel
        .split_once("://")
        .map_or(panel.as_str(), |(_, rest)| rest)
        .split('/')
        .next()
        .unwrap_or_default();
    let host = authority_host(authority);
    let port = state
        .config
        .agent_bind
        .map_or(AGENT_PORT, |bind| bind.port());
    format!("https://{host}:{port}")
}

/// Host part of `host`, `host:port`, `[v6]`, or `[v6]:port`, brackets kept.
fn authority_host(authority: &str) -> &str {
    if authority.starts_with('[') {
        return authority
            .find(']')
            .map_or(authority, |end| &authority[..=end]);
    }
    authority
        .split_once(':')
        .map_or(authority, |(host, _)| host)
}

async fn enroll(
    State(state): State<AppState>,
    auth::OptionalPeer(peer): auth::OptionalPeer,
    headers: HeaderMap,
    Json(body): Json<EnrollRequest>,
) -> Result<Json<EnrollResponse>, ApiError> {
    let ip = auth::client_ip(&headers, peer, state.config.trust_proxy);
    if state
        .login_limiter
        .check_key(&format!("enroll:{ip}"))
        .is_err()
    {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many enrollment attempts",
        ));
    }
    if body.csr_pem.len() > MAX_CSR_BYTES || body.join_token.len() > 200 {
        return Err(bad("invalid_request", "request is too large"));
    }
    let enrolled = agent::enroll(&state, &body.join_token, &body.csr_pem, &body.hostname)
        .await
        .map_err(|error| match error {
            EnrollError::BadToken => ApiError::new(
                StatusCode::UNAUTHORIZED,
                "invalid_join_token",
                error.to_string(),
            ),
            EnrollError::BadCsr => bad("invalid_csr", error.to_string()),
            EnrollError::Database(error) => ApiError::database(error),
            EnrollError::Pki(error) => {
                tracing::error!(%error, "agent CA unavailable");
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ca_unavailable",
                    "the agent certificate authority is unavailable",
                )
            }
        })?;
    audit::record(
        &state.pool,
        None,
        "server.enroll",
        "server",
        Some(&enrolled.server_id.to_string()),
        json!({ "hostname": body.hostname.chars().take(253).collect::<String>() }),
        Some(&ip),
    )
    .await;
    Ok(Json(EnrollResponse {
        server_id: enrolled.server_id,
        client_cert_pem: enrolled.client_cert_pem,
        ca_pem: enrolled.ca_pem,
        grpc_url: grpc_url(&state, &headers),
        tls_name: pki::PANEL_TLS_NAME.to_owned(),
    }))
}

async fn rotate_join_token(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<JoinToken>, ApiError> {
    auth.require_write()?;
    let kind = sqlx::query_scalar!("SELECT kind FROM servers WHERE id = $1", id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| not_found("server not found"))?;
    if kind != "remote" {
        return Err(conflict(
            "local_server",
            "the local server does not use an agent",
        ));
    }
    let token = auth::random_token("join");
    let expires_at = Utc::now() + Duration::hours(JOIN_TOKEN_HOURS);
    sqlx::query!(
        "UPDATE servers SET join_token_hash = $2, join_token_expires_at = $3, updated_at = now() WHERE id = $1",
        id,
        auth::sha256_hex(&token),
        expires_at
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "server.join_token",
        "server",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(Json(JoinToken {
        join_command: join_command(&state, &headers, &token),
        join_token: token,
        expires_at,
    }))
}

async fn install_script() -> Response {
    (
        [(header::CONTENT_TYPE, "text/x-shellscript; charset=utf-8")],
        INSTALL_SCRIPT,
    )
        .into_response()
}

/// Agent binaries bundled with the panel image, so servers can install without
/// reaching GitHub.
async fn download(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    let allowed = [
        "rudder-agent-x86_64-unknown-linux-musl",
        "rudder-agent-aarch64-unknown-linux-musl",
    ];
    let name = file.strip_suffix(".sha256").unwrap_or(&file);
    if !allowed.contains(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = state.config.dist_dir.join(&file);
    match tokio::fs::File::open(&path).await {
        Ok(handle) => {
            let length = handle.metadata().await.map(|m| m.len()).unwrap_or(0);
            let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(handle));
            (
                [
                    (header::CONTENT_TYPE, "application/octet-stream".to_owned()),
                    (header::CONTENT_LENGTH, length.to_string()),
                ],
                body,
            )
                .into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{authority_host, valid_host};

    #[test]
    fn authority_host_drops_ports() {
        assert_eq!(
            authority_host("panel.example.com:3000"),
            "panel.example.com"
        );
        assert_eq!(authority_host("panel.example.com"), "panel.example.com");
        assert_eq!(authority_host("[2001:db8::1]:3000"), "[2001:db8::1]");
        assert_eq!(authority_host("[2001:db8::1]"), "[2001:db8::1]");
    }

    #[test]
    fn host_headers_are_restricted_to_hostname_characters() {
        assert!(valid_host("panel.example.com:3000"));
        assert!(valid_host("[2001:db8::1]:3000"));
        assert!(!valid_host("evil.com/$(id)"));
        assert!(!valid_host("a b"));
        assert!(!valid_host(""));
    }
}
