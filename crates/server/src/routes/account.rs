//! Setup, sessions, users, API tokens, TOTP, and the audit read API.

use std::net::SocketAddr;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use axum_extra::extract::cookie::CookieJar;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use totp_rs::{Builder, Secret, Totp};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::{self, AuthUser};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct UserView {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub role: String,
    pub totp_enabled: bool,
}

#[derive(Serialize, ToSchema)]
pub struct SessionResponse {
    user: UserView,
    csrf_token: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct SetupRequest {
    email: String,
    password: String,
    display_name: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    email: String,
    password: String,
    totp_code: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct PasswordChange {
    current_password: String,
    new_password: String,
}

#[derive(Deserialize, ToSchema)]
pub struct TotpEnable {
    code: String,
}

#[derive(Deserialize, ToSchema)]
pub struct TotpDisable {
    password: String,
    code: String,
}

#[derive(Serialize, ToSchema)]
pub struct TotpSetupResponse {
    secret: String,
    otpauth_url: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateUser {
    email: String,
    password: String,
    display_name: Option<String>,
    role: String,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateUser {
    role: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateToken {
    name: String,
    scopes: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct TokenView {
    id: Uuid,
    name: String,
    scopes: Vec<String>,
    expires_at: Option<DateTime<Utc>>,
    last_used_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedToken {
    id: Uuid,
    name: String,
    scopes: Vec<String>,
    token: String,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Serialize, ToSchema)]
pub struct AuditView {
    pub id: i64,
    pub actor_user_id: Option<Uuid>,
    pub action: String,
    pub resource_type: String,
    pub resource_id: Option<String>,
    #[schema(value_type = Object)]
    pub metadata: serde_json::Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct SetupStatus {
    needs_setup: bool,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/setup/status", get(setup_status))
        .route("/api/v1/setup", post(setup))
        .route("/api/v1/login", post(login))
        .route("/api/v1/logout", post(logout))
        .route("/api/v1/me", get(me))
        .route("/api/v1/me/password", post(change_password))
        .route("/api/v1/me/totp/setup", post(totp_setup))
        .route("/api/v1/me/totp/enable", post(totp_enable))
        .route("/api/v1/me/totp/disable", post(totp_disable))
        .route("/api/v1/users", get(list_users).post(create_user))
        .route(
            "/api/v1/users/{id}",
            axum::routing::patch(update_user).delete(delete_user),
        )
        .route("/api/v1/tokens", get(list_tokens).post(create_token))
        .route("/api/v1/tokens/{id}", axum::routing::delete(delete_token))
        .route("/api/v1/audit", get(list_audit))
}

fn view_of(user: &AuthUser) -> UserView {
    UserView {
        id: user.id,
        email: user.email.clone(),
        display_name: user.display_name.clone(),
        role: user.role.clone(),
        totp_enabled: user.totp_enabled,
    }
}

fn request_ip(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    peer: Option<SocketAddr>,
) -> String {
    auth::client_ip(headers, peer, state.config.trust_proxy)
}

async fn setup_status(State(state): State<AppState>) -> Result<Json<SetupStatus>, ApiError> {
    let count = user_count(&state).await?;
    Ok(Json(SetupStatus {
        needs_setup: count == 0,
    }))
}

async fn setup(
    State(state): State<AppState>,
    auth::OptionalPeer(peer): auth::OptionalPeer,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    Json(body): Json<SetupRequest>,
) -> Result<(CookieJar, Json<SessionResponse>), ApiError> {
    if user_count(&state).await? > 0 {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "already_setup",
            "an owner already exists",
        ));
    }
    let email = auth::normalize_email(&body.email)?;
    auth::validate_password(&body.password)?;
    let display_name = display_name(body.display_name.as_deref(), &email);
    let password_hash = auth::hash_password(&body.password)?;
    let user = sqlx::query!(
        r#"
        INSERT INTO users (email, display_name, password_hash, role)
        VALUES ($1, $2, $3, 'admin')
        RETURNING id, email, display_name, role, totp_enabled
        "#,
        email,
        display_name,
        password_hash,
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let ip = request_ip(&state, &headers, peer);
    audit::record(
        &state.pool,
        Some(user.id),
        "user.setup",
        "user",
        Some(&user.id.to_string()),
        json!({ "email": user.email }),
        Some(&ip),
    )
    .await;
    let auth_user = AuthUser {
        id: user.id,
        email: user.email,
        display_name: user.display_name,
        role: user.role,
        totp_enabled: user.totp_enabled,
        csrf_token: None,
        scopes: vec!["read".into(), "write".into()],
        via_token: false,
        via_mcp: false,
    };
    issue(&state, &auth_user, &headers, jar, Some(ip)).await
}

async fn login(
    State(state): State<AppState>,
    auth::OptionalPeer(peer): auth::OptionalPeer,
    headers: axum::http::HeaderMap,
    jar: CookieJar,
    Json(body): Json<LoginRequest>,
) -> Result<(CookieJar, Json<SessionResponse>), ApiError> {
    let ip = request_ip(&state, &headers, peer);
    if state.login_limiter.check_key(&ip).is_err() {
        return Err(ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "too many login attempts",
        ));
    }
    let email = auth::normalize_email(&body.email).unwrap_or_default();
    let row = if email.is_empty() {
        None
    } else {
        sqlx::query!(
            r#"
            SELECT id, email, display_name, password_hash, role, totp_enabled, totp_secret
            FROM users
            WHERE email = $1
            "#,
            email,
        )
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?
    };
    let Some(row) = row else {
        auth::dummy_verify(&body.password);
        audit::record(
            &state.pool,
            None,
            "session.login_failed",
            "session",
            None,
            json!({}),
            Some(&ip),
        )
        .await;
        return Err(invalid_credentials());
    };
    if !auth::verify_password(&body.password, &row.password_hash) {
        audit::record(
            &state.pool,
            Some(row.id),
            "session.login_failed",
            "session",
            Some(&row.id.to_string()),
            json!({}),
            Some(&ip),
        )
        .await;
        return Err(invalid_credentials());
    }
    if row.totp_enabled {
        let code = body.totp_code.unwrap_or_default();
        if code.is_empty() {
            return Err(ApiError::new(
                StatusCode::UNAUTHORIZED,
                "totp_required",
                "authenticator code required",
            ));
        }
        let secret = decrypt_secret(&state, row.totp_secret.as_deref())?;
        if !totp_matches(&secret, &row.email, &code)? {
            return Err(invalid_credentials());
        }
    }
    let auth_user = AuthUser {
        id: row.id,
        email: row.email,
        display_name: row.display_name,
        role: row.role,
        totp_enabled: row.totp_enabled,
        csrf_token: None,
        scopes: vec!["read".into(), "write".into()],
        via_token: false,
        via_mcp: false,
    };
    audit::record(
        &state.pool,
        Some(auth_user.id),
        "session.login",
        "session",
        Some(&auth_user.id.to_string()),
        json!({}),
        Some(&ip),
    )
    .await;
    issue(&state, &auth_user, &headers, jar, Some(ip)).await
}

async fn logout(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    auth: AuthUser,
    jar: CookieJar,
) -> Result<(CookieJar, StatusCode), ApiError> {
    if let Some(token) = cookie_token(&headers) {
        let hash = auth::sha256_hex(&token);
        let _ = sqlx::query!("DELETE FROM sessions WHERE token_hash = $1", hash)
            .execute(&state.pool)
            .await;
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "session.logout",
        "session",
        Some(&auth.id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok((
        jar.add(auth::clear_session_cookie(state.config.cookie_secure)),
        StatusCode::NO_CONTENT,
    ))
}

async fn me(auth: AuthUser) -> Json<SessionResponse> {
    Json(SessionResponse {
        csrf_token: auth.csrf_token.clone(),
        user: view_of(&auth),
    })
}

async fn change_password(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<PasswordChange>,
) -> Result<StatusCode, ApiError> {
    auth::validate_password(&body.new_password)?;
    let row = sqlx::query!("SELECT password_hash FROM users WHERE id = $1", auth.id)
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if !auth::verify_password(&body.current_password, &row.password_hash) {
        return Err(invalid_credentials());
    }
    let password_hash = auth::hash_password(&body.new_password)?;
    sqlx::query!(
        "UPDATE users SET password_hash = $1, updated_at = now() WHERE id = $2",
        password_hash,
        auth.id,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let _ = sqlx::query!("DELETE FROM sessions WHERE user_id = $1", auth.id)
        .execute(&state.pool)
        .await;
    audit::record(
        &state.pool,
        Some(auth.id),
        "user.password_change",
        "user",
        Some(&auth.id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn totp_setup(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<TotpSetupResponse>, ApiError> {
    if auth.totp_enabled {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "totp_enabled",
            "authenticator is already enabled",
        ));
    }
    let secret = Secret::generate();
    let bytes = secret.as_bytes().to_vec();
    let totp = build_totp(&bytes, &auth.email)?;
    let encrypted = state.cipher.encrypt(&bytes).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "could not store authenticator secret",
        )
    })?;
    sqlx::query!(
        "UPDATE users SET totp_secret = $1, totp_enabled = FALSE, updated_at = now() WHERE id = $2",
        encrypted.as_slice(),
        auth.id,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "user.totp_setup",
        "user",
        Some(&auth.id.to_string()),
        json!({}),
        None,
    )
    .await;
    let otpauth_url = totp.to_url().map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "totp_error",
            "could not build authenticator link",
        )
    })?;
    Ok(Json(TotpSetupResponse {
        secret: secret.to_base32(),
        otpauth_url,
    }))
}

async fn totp_enable(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TotpEnable>,
) -> Result<StatusCode, ApiError> {
    let row = sqlx::query!(
        "SELECT email, totp_secret, totp_enabled FROM users WHERE id = $1",
        auth.id
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if row.totp_enabled {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "totp_enabled",
            "authenticator is already enabled",
        ));
    }
    let secret = decrypt_secret(&state, row.totp_secret.as_deref())?;
    if !totp_matches(&secret, &row.email, &body.code)? {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_totp",
            "authenticator code did not match",
        ));
    }
    sqlx::query!(
        "UPDATE users SET totp_enabled = TRUE, updated_at = now() WHERE id = $1",
        auth.id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "user.totp_enable",
        "user",
        Some(&auth.id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn totp_disable(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<TotpDisable>,
) -> Result<StatusCode, ApiError> {
    let row = sqlx::query!(
        "SELECT email, password_hash, totp_secret, totp_enabled FROM users WHERE id = $1",
        auth.id
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if !row.totp_enabled {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "totp_disabled",
            "authenticator is not enabled",
        ));
    }
    if !auth::verify_password(&body.password, &row.password_hash) {
        return Err(invalid_credentials());
    }
    let secret = decrypt_secret(&state, row.totp_secret.as_deref())?;
    if !totp_matches(&secret, &row.email, &body.code)? {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_totp",
            "authenticator code did not match",
        ));
    }
    sqlx::query!(
        "UPDATE users SET totp_secret = NULL, totp_enabled = FALSE, updated_at = now() WHERE id = $1",
        auth.id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "user.totp_disable",
        "user",
        Some(&auth.id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_users(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<UserView>>, ApiError> {
    auth.require_admin()?;
    let rows = sqlx::query!(
        r#"
        SELECT id, email, display_name, role, totp_enabled
        FROM users
        ORDER BY created_at
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| UserView {
                id: row.id,
                email: row.email,
                display_name: row.display_name,
                role: row.role,
                totp_enabled: row.totp_enabled,
            })
            .collect(),
    ))
}

async fn create_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CreateUser>,
) -> Result<(StatusCode, Json<UserView>), ApiError> {
    auth.require_write()?;
    if body.role != "admin" && body.role != "viewer" {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_role",
            "role must be admin or viewer",
        ));
    }
    let email = auth::normalize_email(&body.email)?;
    auth::validate_password(&body.password)?;
    let display_name = display_name(body.display_name.as_deref(), &email);
    let password_hash = auth::hash_password(&body.password)?;
    let inserted = sqlx::query!(
        r#"
        INSERT INTO users (email, display_name, password_hash, role)
        VALUES ($1, $2, $3, $4)
        RETURNING id, email, display_name, role, totp_enabled
        "#,
        email,
        display_name,
        password_hash,
        body.role,
    )
    .fetch_one(&state.pool)
    .await;
    let user = match inserted {
        Ok(row) => row,
        Err(error) => {
            if is_unique(&error) {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "email_taken",
                    "email is already registered",
                ));
            }
            return Err(ApiError::database(error));
        }
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "user.create",
        "user",
        Some(&user.id.to_string()),
        json!({ "role": user.role }),
        None,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(UserView {
            id: user.id,
            email: user.email,
            display_name: user.display_name,
            role: user.role,
            totp_enabled: user.totp_enabled,
        }),
    ))
}

async fn update_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateUser>,
) -> Result<Json<UserView>, ApiError> {
    auth.require_write()?;
    if body.role != "admin" && body.role != "viewer" {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_role",
            "role must be admin or viewer",
        ));
    }
    if body.role == "viewer" {
        ensure_admin_remains(&state, id).await?;
    }
    let row = sqlx::query!(
        r#"
        UPDATE users SET role = $1, updated_at = now()
        WHERE id = $2
        RETURNING id, email, display_name, role, totp_enabled
        "#,
        body.role,
        id,
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let Some(user) = row else {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "user not found",
        ));
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "user.update",
        "user",
        Some(&user.id.to_string()),
        json!({ "role": user.role }),
        None,
    )
    .await;
    Ok(Json(UserView {
        id: user.id,
        email: user.email,
        display_name: user.display_name,
        role: user.role,
        totp_enabled: user.totp_enabled,
    }))
}

async fn delete_user(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    if auth.id == id {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "cannot_delete_self",
            "you cannot delete your own user",
        ));
    }
    ensure_admin_remains(&state, id).await?;
    let result = sqlx::query!("DELETE FROM users WHERE id = $1", id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "user not found",
        ));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "user.delete",
        "user",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_tokens(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<TokenView>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT id, name, scopes, expires_at, last_used_at, created_at
        FROM api_tokens
        WHERE user_id = $1
        ORDER BY created_at DESC
        "#,
        auth.id,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| TokenView {
                id: row.id,
                name: row.name,
                scopes: row.scopes,
                expires_at: row.expires_at,
                last_used_at: row.last_used_at,
                created_at: row.created_at,
            })
            .collect(),
    ))
}

async fn create_token(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CreateToken>,
) -> Result<(StatusCode, Json<CreatedToken>), ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 64 || name.chars().any(|ch| ch.is_control()) {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "invalid_name",
            "token name must be 1 to 64 visible characters",
        ));
    }
    let scopes = auth::validate_scopes(&body.scopes)?;
    if scopes.iter().any(|scope| scope == "write") {
        auth.require_write()?;
    }
    let token = auth::random_token("rdr");
    let token_hash = auth::sha256_hex(&token);
    let row = sqlx::query!(
        r#"
        INSERT INTO api_tokens (user_id, name, token_hash, scopes, expires_at)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
        "#,
        auth.id,
        name,
        token_hash,
        &scopes,
        body.expires_at,
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "token.create",
        "api_token",
        Some(&row.id.to_string()),
        json!({ "scopes": scopes }),
        None,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(CreatedToken {
            id: row.id,
            name: name.to_owned(),
            scopes,
            token,
            expires_at: body.expires_at,
        }),
    ))
}

async fn delete_token(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let result = sqlx::query!(
        "DELETE FROM api_tokens WHERE id = $1 AND user_id = $2",
        id,
        auth.id,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::new(
            StatusCode::NOT_FOUND,
            "not_found",
            "token not found",
        ));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "token.delete",
        "api_token",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_audit(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<Json<Vec<AuditView>>, ApiError> {
    auth.require_admin()?;
    let rows = sqlx::query!(
        r#"
        SELECT id, actor_user_id, action, resource_type, resource_id, metadata, ip, created_at
        FROM audit_log
        ORDER BY created_at DESC
        LIMIT 200
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| AuditView {
                id: row.id,
                actor_user_id: row.actor_user_id,
                action: row.action,
                resource_type: row.resource_type,
                resource_id: row.resource_id,
                metadata: row.metadata,
                ip: row.ip,
                created_at: row.created_at,
            })
            .collect(),
    ))
}

async fn issue(
    state: &AppState,
    user: &AuthUser,
    headers: &axum::http::HeaderMap,
    jar: CookieJar,
    ip: Option<String>,
) -> Result<(CookieJar, Json<SessionResponse>), ApiError> {
    let token = auth::random_token("");
    let csrf = auth::random_token("");
    let user_agent = headers
        .get(axum::http::header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(300).collect::<String>());
    sqlx::query!(
        r#"
        INSERT INTO sessions (user_id, token_hash, csrf_token, expires_at, user_agent, ip)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        user.id,
        auth::sha256_hex(&token),
        csrf,
        auth::session_expiry(),
        user_agent,
        ip,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok((
        jar.add(auth::session_cookie(&token, state.config.cookie_secure)),
        Json(SessionResponse {
            user: view_of(user),
            csrf_token: Some(csrf),
        }),
    ))
}

async fn user_count(state: &AppState) -> Result<i64, ApiError> {
    let count = sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM users"#)
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::database)?;
    Ok(count)
}

async fn ensure_admin_remains(state: &AppState, changing: Uuid) -> Result<(), ApiError> {
    let admins = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM users WHERE role = 'admin' AND id <> $1"#,
        changing
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if admins == 0 {
        Err(ApiError::new(
            StatusCode::CONFLICT,
            "last_admin",
            "at least one admin is required",
        ))
    } else {
        Ok(())
    }
}

fn display_name(requested: Option<&str>, email: &str) -> String {
    let trimmed = requested.unwrap_or("").trim();
    if trimmed.is_empty() {
        email.split('@').next().unwrap_or("owner").to_owned()
    } else {
        trimmed.chars().take(80).collect()
    }
}

fn invalid_credentials() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_credentials",
        "email or password is incorrect",
    )
}

fn is_unique(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.is_unique_violation())
}

fn decrypt_secret(state: &AppState, blob: Option<&[u8]>) -> Result<Vec<u8>, ApiError> {
    let Some(blob) = blob else {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "totp_missing",
            "set up an authenticator first",
        ));
    };
    state.cipher.decrypt(blob).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "could not read authenticator secret",
        )
    })
}

fn build_totp(secret: &[u8], email: &str) -> Result<Totp, ApiError> {
    Builder::new()
        .with_secret(secret.to_vec())
        .with_account_name(email.to_owned())
        .with_issuer(Some("Rudder"))
        .build()
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "totp_error",
                "could not build authenticator",
            )
        })
}

fn totp_matches(secret: &[u8], email: &str, code: &str) -> Result<bool, ApiError> {
    let totp = build_totp(secret, email)?;
    Ok(totp.check_current(code).is_some())
}

fn cookie_token(headers: &axum::http::HeaderMap) -> Option<String> {
    let header = headers.get(axum::http::header::COOKIE)?.to_str().ok()?;
    for item in cookie::Cookie::split_parse(header) {
        let Ok(cookie) = item else { continue };
        if cookie.name() == auth::SESSION_COOKIE {
            return Some(cookie.value().to_owned());
        }
    }
    None
}
