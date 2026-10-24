//! OAuth 2.1 endpoints and the MCP JSON-RPC endpoint.
//!
//! Agents discover the login server, send the person to the browser to approve
//! a template, then exchange the code for an access token and a refresh token.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tower::ServiceExt;

use crate::audit;
use crate::auth::{self, AuthUser};
use crate::error::ApiError;
use crate::state::AppState;

use super::tokens::{self, ExchangeError};
use super::Tool;

const PROTOCOL: &str = "2025-03-26";
const MAX_BODY: usize = 1024 * 1024;
const MAX_TOOL_RESPONSE: usize = 256 * 1024;

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server),
        )
        .route("/oauth/register", post(register))
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .route("/mcp", post(rpc).get(mcp_unused).delete(mcp_end))
}

fn no_store(response: Response) -> Response {
    let mut response = response;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

/// The address agents should use. The panel domain saved under **Certificates**
/// wins, so changing it there also moves the sign-in address. Otherwise an
/// `https` public address wins. A plain `http` one (the install default before
/// a panel domain exists) gives way to the HTTPS address the request actually
/// came in on, because agents in the cloud reject `http` endpoints and cannot
/// reach the install's IP address.
async fn origin(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    if let Some(url) = panel_origin(state).await {
        return Ok(url);
    }
    let configured = state
        .config
        .public_url
        .as_deref()
        .map(|url| url.trim_end_matches('/').to_owned());
    if let Some(url) = &configured {
        if url.starts_with("https://") {
            return Ok(url.clone());
        }
        return match request_origin(state, headers) {
            Ok(seen) if seen.starts_with("https://") => Ok(seen),
            _ => Ok(url.clone()),
        };
    }
    request_origin(state, headers)
}

async fn panel_origin(state: &AppState) -> Option<String> {
    let row: Option<(Option<String>, String)> =
        sqlx::query_as("SELECT panel_domain, panel_ssl_mode FROM panel_settings WHERE id = TRUE")
            .fetch_optional(&state.pool)
            .await
            .ok()?;
    let (domain, mode) = row?;
    let domain = domain.filter(|domain| !domain.is_empty())?;
    let scheme = if mode == "none" { "http" } else { "https" };
    Some(format!("{scheme}://{domain}"))
}

fn request_origin(state: &AppState, headers: &HeaderMap) -> Result<String, ApiError> {
    let host = headers
        .get(if state.config.trust_proxy {
            "x-forwarded-host"
        } else {
            "host"
        })
        .or_else(|| headers.get(header::HOST))
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .split(',')
        .next()
        .unwrap_or("")
        .trim();
    if host.is_empty()
        || host.contains('/')
        || host.contains(' ')
        || host.contains('@')
        || host.len() > 255
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_host",
            "set RUDDER_PUBLIC_URL so agents know the panel address",
        ));
    }
    let proto = if state.config.trust_proxy {
        headers
            .get("x-forwarded-proto")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(',').next())
            .map(str::trim)
            .filter(|value| *value == "http" || *value == "https")
            .unwrap_or(if state.config.cookie_secure {
                "https"
            } else {
                "http"
            })
    } else if state.config.cookie_secure {
        "https"
    } else {
        "http"
    };
    Ok(format!("{proto}://{host}"))
}

fn scopes_supported() -> Vec<&'static str> {
    rudder_core::MCP_SCOPES
        .iter()
        .map(|scope| scope.id)
        .collect()
}

async fn protected_resource(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let origin = origin(&state, &headers).await?;
    Ok(no_store(
        Json(json!({
            "resource": format!("{origin}/mcp"),
            "authorization_servers": [origin],
            "scopes_supported": scopes_supported(),
            "bearer_methods_supported": ["header"],
            "resource_name": "Rudder",
        }))
        .into_response(),
    ))
}

async fn authorization_server(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let origin = origin(&state, &headers).await?;
    Ok(no_store(
        Json(json!({
            "issuer": origin,
            "authorization_endpoint": format!("{origin}/oauth/authorize"),
            "token_endpoint": format!("{origin}/oauth/token"),
            "registration_endpoint": format!("{origin}/oauth/register"),
            "revocation_endpoint": format!("{origin}/oauth/revoke"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": scopes_supported(),
        }))
        .into_response(),
    ))
}

#[derive(Deserialize)]
struct RegisterRequest {
    client_name: String,
    redirect_uris: Vec<String>,
    #[serde(default)]
    grant_types: Vec<String>,
    #[serde(default)]
    response_types: Vec<String>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
}

async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth::OptionalPeer(peer): auth::OptionalPeer,
    Json(body): Json<RegisterRequest>,
) -> Result<Response, ApiError> {
    if rate_limited(&state.oauth_limiter, &client_ip(&state, &headers, peer)) {
        return Err(too_many());
    }
    let name = clean_name(&body.client_name).ok_or_else(|| {
        ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "client name must be 1 to 80 characters",
        )
    })?;
    if body.redirect_uris.is_empty() || body.redirect_uris.len() > 8 {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_redirect",
            "provide 1 to 8 redirect URLs",
        ));
    }
    if body.redirect_uris.iter().any(|uri| !valid_redirect(uri)) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_redirect",
            "redirect URLs must be https, a loopback http URL, or a private app scheme",
        ));
    }
    if body
        .token_endpoint_auth_method
        .as_deref()
        .is_some_and(|method| method != "none")
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "this server only supports public clients (token_endpoint_auth_method none)",
        ));
    }
    if body
        .grant_types
        .iter()
        .any(|grant| grant != "authorization_code" && grant != "refresh_token")
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "supported grants are authorization_code and refresh_token",
        ));
    }
    if body.response_types.iter().any(|kind| kind != "code") {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "the only response type is code",
        ));
    }
    let count = sqlx::query_scalar!(r#"SELECT count(*) AS "count!" FROM oauth_clients"#)
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if count >= 200 {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_clients",
            "too many agent clients are registered; unused ones are removed after 7 days",
        ));
    }
    let client_id = auth::random_token("rdr_cli");
    sqlx::query!(
        r#"
        INSERT INTO oauth_clients (client_id, client_name, redirect_uris)
        VALUES ($1, $2, $3)
        "#,
        client_id,
        name,
        &body.redirect_uris,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(no_store(
        (
            StatusCode::CREATED,
            Json(json!({
                "client_id": client_id,
                "client_name": name,
                "redirect_uris": body.redirect_uris,
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            })),
        )
            .into_response(),
    ))
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    code_challenge: String,
    code_challenge_method: String,
    state: String,
    #[serde(default)]
    resource: Option<String>,
}

async fn authorize(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<AuthorizeQuery>,
    request: Request,
) -> Result<Response, ApiError> {
    validate_login(
        &query.response_type,
        &query.code_challenge_method,
        &query.code_challenge,
        &query.state,
        query.resource.as_deref(),
    )?;
    load_client_redirect(&state, &query.client_id, &query.redirect_uri).await?;
    let raw = request.uri().query().unwrap_or("");
    if auth::session_user(&state, &headers).await.is_none() {
        let back = format!("/oauth/authorize?{raw}");
        let next = percent(&back);
        return Ok(Redirect::temporary(&format!("/login?next={next}")).into_response());
    }
    Ok(Redirect::temporary(&format!("/mcp/consent?{raw}")).into_response())
}

pub(crate) fn validate_login(
    response_type: &str,
    method: &str,
    challenge: &str,
    state: &str,
    resource: Option<&str>,
) -> Result<(), ApiError> {
    if response_type != "code" {
        return Err(bad("response_type must be code"));
    }
    if method != "S256" {
        return Err(bad("code_challenge_method must be S256"));
    }
    if !(43..=128).contains(&challenge.len())
        || !challenge
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(bad("code_challenge must be the S256 value"));
    }
    if state.is_empty() || state.len() > 512 {
        return Err(bad("state is required"));
    }
    if resource.is_some_and(|resource| !resource.is_empty() && !resource.ends_with("/mcp")) {
        return Err(bad("resource must be the panel MCP address"));
    }
    Ok(())
}

#[derive(Deserialize)]
struct TokenForm {
    grant_type: String,
    #[serde(default)]
    code: String,
    #[serde(default)]
    redirect_uri: String,
    #[serde(default)]
    client_id: String,
    #[serde(default)]
    code_verifier: String,
    #[serde(default)]
    refresh_token: String,
}

async fn token(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth::OptionalPeer(peer): auth::OptionalPeer,
    body: axum::body::Bytes,
) -> Response {
    if rate_limited(&state.oauth_limiter, &client_ip(&state, &headers, peer)) {
        return oauth_error(
            StatusCode::TOO_MANY_REQUESTS,
            "temporarily_unavailable",
            "too many token requests",
        );
    }
    if body.len() > 64 * 1024 {
        return oauth_error(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "body is too large",
        );
    }
    let content = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let form: TokenForm = if content.starts_with("application/json") {
        match serde_json::from_slice::<TokenForm>(&body) {
            Ok(form) => form,
            Err(_) => {
                return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "invalid JSON")
            }
        }
    } else {
        match serde_urlencoded::from_bytes(&body) {
            Ok(form) => form,
            Err(_) => {
                return oauth_error(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "invalid form body",
                )
            }
        }
    };
    let issued = match form.grant_type.as_str() {
        "authorization_code" => {
            tokens::exchange_code(
                &state,
                &form.client_id,
                &form.code,
                &form.redirect_uri,
                &form.code_verifier,
            )
            .await
        }
        "refresh_token" => {
            tokens::exchange_refresh(&state, &form.client_id, &form.refresh_token).await
        }
        _ => {
            return oauth_error(
                StatusCode::BAD_REQUEST,
                "unsupported_grant_type",
                "supported grants are authorization_code and refresh_token",
            )
        }
    };
    match issued {
        Ok(issued) => no_store(
            Json(json!({
                "access_token": issued.access_token,
                "token_type": "Bearer",
                "expires_in": issued.expires_in,
                "refresh_token": issued.refresh_token,
                "scope": issued.scope,
            }))
            .into_response(),
        ),
        Err(ExchangeError::Invalid(message)) => {
            oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", message)
        }
        Err(ExchangeError::Server) => oauth_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "server_error",
            "could not issue a token",
        ),
    }
}

#[derive(Deserialize)]
struct RevokeForm {
    token: String,
    #[serde(default)]
    client_id: String,
}

async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let content = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let form: RevokeForm = if content.starts_with("application/json") {
        serde_json::from_slice(&body).unwrap_or(RevokeForm {
            token: String::new(),
            client_id: String::new(),
        })
    } else {
        serde_urlencoded::from_bytes(&body).unwrap_or(RevokeForm {
            token: String::new(),
            client_id: String::new(),
        })
    };
    if !form.token.is_empty() {
        let _ = tokens::revoke(&state, &form.token, &form.client_id).await;
    }
    no_store(StatusCode::OK.into_response())
}

async fn mcp_unused() -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(header::ALLOW, "POST, DELETE".parse().unwrap());
    no_store(response)
}

async fn mcp_end() -> Response {
    no_store(StatusCode::NO_CONTENT.into_response())
}

async fn rpc(
    State(state): State<AppState>,
    headers: HeaderMap,
    auth::OptionalPeer(peer): auth::OptionalPeer,
    body: axum::body::Bytes,
) -> Response {
    let origin = origin(&state, &headers).await.unwrap_or_default();
    if !origin_ok(&headers, &origin) {
        return no_store(
            (
                StatusCode::FORBIDDEN,
                Json(json!({"error": "cross-origin MCP requests are not allowed"})),
            )
                .into_response(),
        );
    }
    if rate_limited(&state.mcp_limiter, &client_ip(&state, &headers, peer)) {
        return no_store(
            (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, "5")],
                Json(rpc_error(Value::Null, -32000, "too many agent requests")),
            )
                .into_response(),
        );
    }
    if body.len() > MAX_BODY {
        return no_store(
            (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(rpc_error(Value::Null, -32600, "request is too large")),
            )
                .into_response(),
        );
    }
    let message: Value = match serde_json::from_slice::<Value>(&body) {
        Ok(value) if value.is_object() => value,
        _ => {
            return no_store(
                (
                    StatusCode::BAD_REQUEST,
                    Json(rpc_error(Value::Null, -32700, "expected a JSON-RPC object")),
                )
                    .into_response(),
            )
        }
    };
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if message.get("id").is_none() {
        return no_store(StatusCode::ACCEPTED.into_response());
    }
    let token = bearer(&headers);
    let Some(token) = token else {
        return unauthorized_mcp(&origin, id);
    };
    let user = match tokens::load_access(&state, &token).await {
        Ok(user) => user,
        Err(_) => return unauthorized_mcp(&origin, id),
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let result = match method {
        "initialize" => initialize(&params),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list(&user)),
        "tools/call" => match tools_call(&state, &token, &user, &params).await {
            Ok(result) => Ok(result),
            Err(message) => Err(rpc_error(id.clone(), -32602, message)),
        },
        "resources/list" => Ok(json!({"resources": []})),
        "prompts/list" => Ok(json!({"prompts": []})),
        _ => Err(rpc_error(id.clone(), -32601, "method not found")),
    };
    let body = match result {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(error) => error,
    };
    no_store(Json(body).into_response())
}

fn initialize(params: &Value) -> Result<Value, Value> {
    let requested = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL);
    let version = if requested == PROTOCOL || requested == "2025-06-18" {
        requested
    } else {
        PROTOCOL
    };
    Ok(json!({
        "protocolVersion": version,
        "capabilities": {"tools": {"listChanged": false}},
        "serverInfo": {"name": "rudder", "version": rudder_core::VERSION},
        "instructions": "Call rudder_whoami to see this approval. Other tools are the panel API. Full control can read and change every setting and all data. Pass path parameters, query parameters, and a JSON body.",
    }))
}

fn tools_list(user: &AuthUser) -> Value {
    let mut tools: Vec<Value> = super::tools()
        .into_iter()
        .filter(|tool| user.scopes.iter().any(|scope| scope == &tool.scope))
        .map(tool_json)
        .collect();
    tools.insert(
        0,
        json!({
            "name": "rudder_whoami",
            "description": "Who this agent is, which permission template was approved, and which scopes it has.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        }),
    );
    json!({"tools": tools})
}

fn tool_json(tool: Tool) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();
    let names = path_params(tool.path);
    if !names.is_empty() {
        let mut path_props = serde_json::Map::new();
        for name in &names {
            path_props.insert((*name).to_owned(), json!({"type": "string"}));
        }
        properties.insert(
            "path".into(),
            json!({
                "type": "object",
                "properties": path_props,
                "required": names,
                "additionalProperties": false,
            }),
        );
        required.push("path");
    }
    properties.insert(
        "query".into(),
        json!({
            "type": "object",
            "description": "Query parameters. Values are strings, numbers, or booleans.",
            "additionalProperties": {"type": "string"},
        }),
    );
    if tool.body_schema.is_some() {
        properties.insert(
            "body".into(),
            json!({
                "description": format!("JSON body ({})", tool.body_schema.unwrap_or("object")),
            }),
        );
    }
    json!({
        "name": tool.name,
        "description": tool.description,
        "inputSchema": {
            "type": "object",
            "properties": properties,
            "required": required,
            "additionalProperties": false,
        },
    })
}

async fn tools_call(
    state: &AppState,
    token: &str,
    user: &AuthUser,
    params: &Value,
) -> Result<Value, &'static str> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name == "rudder_whoami" {
        let described = tokens::describe(state, token)
            .await
            .map_err(|_| "could not read this approval")?;
        return Ok(tool_text(StatusCode::OK, &described.to_string(), false));
    }
    let Some(tool) = super::tools().into_iter().find(|tool| tool.name == name) else {
        return Err("unknown tool");
    };
    if !user.scopes.iter().any(|scope| scope == &tool.scope) {
        return Ok(tool_text(
            StatusCode::FORBIDDEN,
            &format!("this approval does not include {}", tool.scope),
            false,
        ));
    }
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let path = fill_path(tool.path, &arguments)?;
    let mut builder = axum::http::Request::builder()
        .method(tool.method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"));
    let body = arguments.get("body").filter(|body| !body.is_null());
    if body.is_some() {
        builder = builder.header(header::CONTENT_TYPE, "application/json");
    }
    let request = builder
        .body(match body {
            Some(body) => Body::from(body.to_string()),
            None => Body::empty(),
        })
        .map_err(|_| "could not build the request")?;
    let response = crate::router(state.clone())
        .oneshot(request)
        .await
        .map_err(|_| "the panel did not answer")?;
    let status = response.status();
    let bytes = match axum::body::to_bytes(response.into_body(), MAX_TOOL_RESPONSE).await {
        Ok(bytes) => (bytes.to_vec(), false),
        Err(_) => (b"response truncated; narrow the query".to_vec(), true),
    };
    let text = String::from_utf8_lossy(&bytes.0);
    Ok(tool_text(
        status,
        &format!("HTTP {}\n{text}", status.as_u16()),
        bytes.1,
    ))
}

fn fill_path(pattern: &str, arguments: &Value) -> Result<String, &'static str> {
    let provided = arguments
        .get("path")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut path = pattern.to_owned();
    for (key, value) in &provided {
        let needle = format!("{{{key}}}");
        if !path.contains(&needle) {
            return Err("unknown path parameter");
        }
        let text = match value {
            Value::String(text) => text.as_str(),
            _ => return Err("path parameters must be strings"),
        };
        if text.is_empty() || text.contains('/') {
            return Err("invalid path parameter");
        }
        let encoded =
            percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC)
                .to_string();
        path = path.replace(&needle, &encoded);
    }
    if path.contains('{') {
        return Err("missing path parameter");
    }
    if let Some(query) = arguments.get("query").and_then(Value::as_object) {
        let mut pairs = Vec::new();
        for (key, value) in query {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                Value::Null => continue,
                _ => return Err("query values must be strings, numbers, or booleans"),
            };
            pairs.push(format!("{}={}", percent(key), percent(&text)));
        }
        if !pairs.is_empty() {
            path.push('?');
            path.push_str(&pairs.join("&"));
        }
    }
    Ok(path)
}

fn path_params(pattern: &str) -> Vec<&str> {
    pattern
        .split('/')
        .filter_map(|segment| {
            segment
                .strip_prefix('{')
                .and_then(|rest| rest.strip_suffix('}'))
        })
        .collect()
}

fn tool_text(status: StatusCode, text: &str, truncated: bool) -> Value {
    json!({
        "content": [{"type": "text", "text": text}],
        "isError": truncated || status.as_u16() >= 400,
    })
}

fn rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message},
    })
}

fn unauthorized_mcp(origin: &str, id: Value) -> Response {
    let metadata = format!("{origin}/.well-known/oauth-protected-resource");
    let mut response = (
        StatusCode::UNAUTHORIZED,
        Json(rpc_error(id, -32001, "sign in required")),
    )
        .into_response();
    if let Ok(value) = format!("Bearer realm=\"rudder\", resource_metadata=\"{metadata}\"").parse()
    {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, value);
    }
    no_store(response)
}

fn oauth_error(status: StatusCode, code: &str, description: &str) -> Response {
    no_store(
        (
            status,
            Json(json!({"error": code, "error_description": description})),
        )
            .into_response(),
    )
}

fn origin_ok(headers: &HeaderMap, expected: &str) -> bool {
    let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
    else {
        return true;
    };
    if expected.is_empty() || origin == "null" {
        return false;
    }
    origin.trim_end_matches('/') == expected.trim_end_matches('/')
}

fn rate_limited(limiter: &crate::state::LoginLimiter, ip: &str) -> bool {
    limiter.check_key(&ip.to_owned()).is_err()
}

fn client_ip(state: &AppState, headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
    auth::client_ip(headers, peer, state.config.trust_proxy)
}

fn too_many() -> ApiError {
    ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "rate_limited",
        "too many requests; try again in a minute",
    )
}

fn bad(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_request", message)
}

fn percent(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn clean_name(name: &str) -> Option<String> {
    let name = name.trim();
    if (1..=80).contains(&name.len()) && !name.chars().any(|c| c.is_control()) {
        Some(name.to_owned())
    } else {
        None
    }
}

pub(crate) fn valid_redirect(uri: &str) -> bool {
    if uri.len() > 500 || uri.contains('#') || uri.contains('\\') || uri.contains('@') {
        return false;
    }
    let Ok(url) = url::Url::parse(uri) else {
        return false;
    };
    match url.scheme() {
        "https" => url.host_str().is_some(),
        "http" => matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]") | Some("::1")
        ),
        "javascript" | "data" | "file" | "vbscript" => false,
        scheme
            if (2..=40).contains(&scheme.len())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-') =>
        {
            true
        }
        _ => false,
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
}

pub async fn load_client_redirect(
    state: &AppState,
    client_id: &str,
    redirect_uri: &str,
) -> Result<(String, String), ApiError> {
    let row = sqlx::query!(
        r#"SELECT client_name, redirect_uris FROM oauth_clients WHERE client_id = $1"#,
        client_id,
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_client",
            "unknown agent client",
        ));
    };
    if !row.redirect_uris.iter().any(|uri| uri == redirect_uri) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_redirect",
            "redirect URL is not registered for this client",
        ));
    }
    Ok((row.client_name, redirect_uri.to_owned()))
}

pub fn redirect_with(redirect_uri: &str, code: &str, state: &str) -> Result<String, ApiError> {
    let mut url = url::Url::parse(redirect_uri).map_err(|_| bad("redirect URL is invalid"))?;
    url.query_pairs_mut()
        .append_pair("code", code)
        .append_pair("state", state);
    Ok(url.to_string())
}

pub async fn record_grant(state: &AppState, user: &AuthUser, client_id: &str, template: &str) {
    audit::record(
        &state.pool,
        Some(user.id),
        "mcp.grant",
        "mcp_grant",
        Some(client_id),
        json!({"template": template}),
        None,
    )
    .await;
}
