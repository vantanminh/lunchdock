//! Servers, projects, environments, and services.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Json;
use chrono::{DateTime, Duration, Utc};
use rudder_core::{validate_env_key, validate_name, Error as CoreError, NodeDriver};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::{self, AuthUser};
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct ServerView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub public_ip: Option<String>,
    pub description: String,
    pub agent_version: Option<String>,
    pub docker_version: Option<String>,
    pub os_info: Option<String>,
    pub cpu_count: Option<i32>,
    pub memory_bytes: Option<i64>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct CreatedServer {
    #[serde(flatten)]
    pub server: ServerView,
    pub join_token: Option<String>,
    pub join_command: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateServer {
    pub name: String,
    pub kind: String,
    pub public_ip: Option<String>,
    pub description: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateServer {
    pub name: Option<String>,
    pub public_ip: Option<String>,
    pub description: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ProjectView {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateProject {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct EnvironmentView {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateEnvironment {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ServiceView {
    pub id: Uuid,
    pub environment_id: Uuid,
    pub server_id: Uuid,
    pub name: String,
    pub kind: String,
    pub status: String,
    pub spec: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateService {
    pub name: String,
    pub kind: String,
    pub server_id: Uuid,
    pub spec: Option<Value>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateService {
    pub spec: Option<Value>,
    pub server_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct EnvVarView {
    pub key: String,
    pub is_build_secret: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct PutEnvVar {
    pub key: String,
    pub value: String,
    pub is_build_secret: bool,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/servers", get(list_servers).post(create_server))
        .route(
            "/api/v1/servers/{id}",
            axum::routing::patch(update_server).delete(delete_server),
        )
        .route(
            "/api/v1/servers/{id}/probe",
            axum::routing::post(probe_server),
        )
        .route("/api/v1/projects", get(list_projects).post(create_project))
        .route(
            "/api/v1/projects/{id}",
            get(get_project)
                .patch(update_project)
                .delete(delete_project),
        )
        .route(
            "/api/v1/projects/{id}/environments",
            get(list_environments).post(create_environment),
        )
        .route(
            "/api/v1/environments/{id}",
            axum::routing::delete(delete_environment),
        )
        .route(
            "/api/v1/environments/{id}/services",
            get(list_services).post(create_service),
        )
        .route(
            "/api/v1/services/{id}",
            get(get_service)
                .patch(update_service)
                .delete(delete_service),
        )
        .route("/api/v1/services/{id}/env", get(list_env).put(put_env))
        .route(
            "/api/v1/services/{id}/env/{key}",
            axum::routing::delete(delete_env),
        )
}

async fn list_servers(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<ServerView>>, ApiError> {
    let rows = sqlx::query_as!(
        ServerRow,
        r#"
        SELECT id, name, kind, status, public_ip, description, agent_version, docker_version,
               os_info, cpu_count, memory_bytes, last_heartbeat_at, created_at
        FROM servers
        ORDER BY created_at
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(rows.into_iter().map(server_from_row).collect()))
}

async fn create_server(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CreateServer>,
) -> Result<(StatusCode, Json<CreatedServer>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    if body.kind != "local" && body.kind != "remote" {
        return Err(bad("invalid_kind", "kind must be local or remote"));
    }
    if body.kind == "local" {
        let existing =
            sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM servers WHERE kind = 'local'"#)
                .fetch_one(&state.pool)
                .await
                .map_err(ApiError::database)?;
        if existing > 0 {
            return Err(conflict("local_exists", "the local server already exists"));
        }
    }
    let (join_hash, join_expires, join_token) = if body.kind == "remote" {
        let token = auth::random_token("join");
        (
            Some(auth::sha256_hex(&token)),
            Some(Utc::now() + Duration::hours(1)),
            Some(token),
        )
    } else {
        (None, None, None)
    };
    let description = body.description.unwrap_or_default();
    let inserted = sqlx::query_as!(
        ServerRow,
        r#"
        INSERT INTO servers (
            name, kind, status, public_ip, description, join_token_hash, join_token_expires_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        RETURNING id, name, kind, status, public_ip, description, agent_version, docker_version,
                  os_info, cpu_count, memory_bytes, last_heartbeat_at, created_at
        "#,
        body.name,
        body.kind,
        if body.kind == "remote" {
            "pending"
        } else {
            "unknown"
        },
        body.public_ip,
        description,
        join_hash,
        join_expires,
    )
    .fetch_one(&state.pool)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict("name_taken", "a server with that name exists"));
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "server.create",
        "server",
        Some(&row.id.to_string()),
        json!({ "kind": row.kind }),
        None,
    )
    .await;
    let command = join_token.as_ref().map(|token| {
        format!(
            "rudder-agent join --server {} --token {token}",
            state
                .config
                .public_url
                .clone()
                .unwrap_or_else(|| "https://panel.example".into())
        )
    });
    Ok((
        StatusCode::CREATED,
        Json(CreatedServer {
            server: server_from_row(row),
            join_token,
            join_command: command,
        }),
    ))
}

async fn update_server(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateServer>,
) -> Result<Json<ServerView>, ApiError> {
    auth.require_write()?;
    if let Some(name) = &body.name {
        validate_name(name).map_err(core_error)?;
    }
    let row = sqlx::query_as!(
        ServerRow,
        r#"
        UPDATE servers
        SET name = COALESCE($2, name),
            public_ip = COALESCE($3, public_ip),
            description = COALESCE($4, description),
            updated_at = now()
        WHERE id = $1
        RETURNING id, name, kind, status, public_ip, description, agent_version, docker_version,
                  os_info, cpu_count, memory_bytes, last_heartbeat_at, created_at
        "#,
        id,
        body.name,
        body.public_ip,
        body.description,
    )
    .fetch_optional(&state.pool)
    .await;
    let row = match row {
        Ok(Some(row)) => row,
        Ok(None) => return Err(not_found("server not found")),
        Err(error) if is_unique(&error) => {
            return Err(conflict("name_taken", "a server with that name exists"));
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "server.update",
        "server",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(Json(server_from_row(row)))
}

async fn delete_server(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let row = sqlx::query!(r#"SELECT kind FROM servers WHERE id = $1"#, id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Err(not_found("server not found"));
    };
    if row.kind == "local" {
        return Err(conflict(
            "local_server",
            "the local server cannot be removed",
        ));
    }
    let services = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM services WHERE server_id = $1"#,
        id
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if services > 0 {
        return Err(conflict(
            "server_in_use",
            "move or delete services before removing this server",
        ));
    }
    sqlx::query!(r#"DELETE FROM servers WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "server.delete",
        "server",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn probe_server(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ServerView>, ApiError> {
    auth.require_write()?;
    let kind = sqlx::query!(r#"SELECT kind FROM servers WHERE id = $1"#, id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let Some(kind) = kind else {
        return Err(not_found("server not found"));
    };
    if kind.kind != "local" {
        return Err(conflict(
            "agent_required",
            "remote servers report status through the agent",
        ));
    }
    let info = state.local_driver.docker_info().await.map_err(core_error)?;
    let row = sqlx::query_as!(
        ServerRow,
        r#"
        UPDATE servers
        SET status = 'online',
            docker_version = $2,
            os_info = $3,
            cpu_count = $4,
            memory_bytes = $5,
            last_heartbeat_at = now(),
            updated_at = now()
        WHERE id = $1
        RETURNING id, name, kind, status, public_ip, description, agent_version, docker_version,
                  os_info, cpu_count, memory_bytes, last_heartbeat_at, created_at
        "#,
        id,
        info.version,
        info.os,
        i32::try_from(info.cpus).unwrap_or(i32::MAX),
        info.memory_bytes,
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(server_from_row(row)))
}

async fn list_projects(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<ProjectView>>, ApiError> {
    let rows =
        sqlx::query!(r#"SELECT id, name, description, created_at FROM projects ORDER BY name"#)
            .fetch_all(&state.pool)
            .await
            .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| ProjectView {
                id: row.id,
                name: row.name,
                description: row.description,
                created_at: row.created_at,
            })
            .collect(),
    ))
}

async fn create_project(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CreateProject>,
) -> Result<(StatusCode, Json<ProjectView>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    let description = body.description.unwrap_or_default();
    let inserted = sqlx::query!(
        r#"
        INSERT INTO projects (name, description)
        VALUES ($1, $2)
        RETURNING id, name, description, created_at
        "#,
        body.name,
        description,
    )
    .fetch_one(&state.pool)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict("name_taken", "a project with that name exists"));
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "project.create",
        "project",
        Some(&row.id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(ProjectView {
            id: row.id,
            name: row.name,
            description: row.description,
            created_at: row.created_at,
        }),
    ))
}

async fn get_project(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ProjectView>, ApiError> {
    let row = sqlx::query!(
        r#"SELECT id, name, description, created_at FROM projects WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Err(not_found("project not found"));
    };
    Ok(Json(ProjectView {
        id: row.id,
        name: row.name,
        description: row.description,
        created_at: row.created_at,
    }))
}

async fn update_project(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateProject>,
) -> Result<Json<ProjectView>, ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    let description = body.description.unwrap_or_default();
    let updated = sqlx::query!(
        r#"
        UPDATE projects SET name = $2, description = $3, updated_at = now()
        WHERE id = $1
        RETURNING id, name, description, created_at
        "#,
        id,
        body.name,
        description,
    )
    .fetch_optional(&state.pool)
    .await;
    let row = match updated {
        Ok(Some(row)) => row,
        Ok(None) => return Err(not_found("project not found")),
        Err(error) if is_unique(&error) => {
            return Err(conflict("name_taken", "a project with that name exists"));
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    Ok(Json(ProjectView {
        id: row.id,
        name: row.name,
        description: row.description,
        created_at: row.created_at,
    }))
}

async fn delete_project(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let result = sqlx::query!(r#"DELETE FROM projects WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if result.rows_affected() == 0 {
        return Err(not_found("project not found"));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "project.delete",
        "project",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_environments(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<EnvironmentView>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT id, project_id, name, description
        FROM environments
        WHERE project_id = $1
        ORDER BY name
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| EnvironmentView {
                id: row.id,
                project_id: row.project_id,
                name: row.name,
                description: row.description,
            })
            .collect(),
    ))
}

async fn create_environment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(project_id): Path<Uuid>,
    Json(body): Json<CreateEnvironment>,
) -> Result<(StatusCode, Json<EnvironmentView>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    let description = body.description.unwrap_or_default();
    let inserted = sqlx::query!(
        r#"
        INSERT INTO environments (project_id, name, description)
        VALUES ($1, $2, $3)
        RETURNING id, project_id, name, description
        "#,
        project_id,
        body.name,
        description,
    )
    .fetch_one(&state.pool)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict("name_taken", "that environment already exists"));
        }
        Err(error) if is_fk(&error) => return Err(not_found("project not found")),
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "environment.create",
        "environment",
        Some(&row.id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(EnvironmentView {
            id: row.id,
            project_id: row.project_id,
            name: row.name,
            description: row.description,
        }),
    ))
}

async fn delete_environment(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let result = sqlx::query!(r#"DELETE FROM environments WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if result.rows_affected() == 0 {
        return Err(not_found("environment not found"));
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn list_services(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(environment_id): Path<Uuid>,
) -> Result<Json<Vec<ServiceView>>, ApiError> {
    let rows = sqlx::query_as!(
        ServiceRow,
        r#"
        SELECT id, environment_id, server_id, name, kind, status, spec, created_at
        FROM services
        WHERE environment_id = $1
        ORDER BY name
        "#,
        environment_id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(rows.into_iter().map(service_from_row).collect()))
}

async fn create_service(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(environment_id): Path<Uuid>,
    Json(body): Json<CreateService>,
) -> Result<(StatusCode, Json<ServiceView>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    check_kind(&body.kind)?;
    let spec = checked_spec(body.spec.unwrap_or_else(|| json!({})))?;
    let inserted = sqlx::query_as!(
        ServiceRow,
        r#"
        INSERT INTO services (environment_id, server_id, name, kind, spec)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id, environment_id, server_id, name, kind, status, spec, created_at
        "#,
        environment_id,
        body.server_id,
        body.name,
        body.kind,
        spec,
    )
    .fetch_one(&state.pool)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict("name_taken", "that service already exists"));
        }
        Err(error) if is_fk(&error) => {
            return Err(not_found("environment or server not found"));
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "service.create",
        "service",
        Some(&row.id.to_string()),
        json!({ "kind": row.kind }),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(service_from_row(row))))
}

async fn get_service(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ServiceView>, ApiError> {
    let row = sqlx::query_as!(
        ServiceRow,
        r#"
        SELECT id, environment_id, server_id, name, kind, status, spec, created_at
        FROM services WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Err(not_found("service not found"));
    };
    Ok(Json(service_from_row(row)))
}

async fn update_service(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateService>,
) -> Result<Json<ServiceView>, ApiError> {
    auth.require_write()?;
    if let Some(spec) = &body.spec {
        checked_spec(spec.clone())?;
    }
    let row = sqlx::query_as!(
        ServiceRow,
        r#"
        UPDATE services
        SET spec = COALESCE($2, spec),
            server_id = COALESCE($3, server_id),
            updated_at = now()
        WHERE id = $1
        RETURNING id, environment_id, server_id, name, kind, status, spec, created_at
        "#,
        id,
        body.spec,
        body.server_id,
    )
    .fetch_optional(&state.pool)
    .await;
    let row = match row {
        Ok(Some(row)) => row,
        Ok(None) => return Err(not_found("service not found")),
        Err(error) if is_fk(&error) => return Err(not_found("server not found")),
        Err(error) => return Err(ApiError::database(error)),
    };
    Ok(Json(service_from_row(row)))
}

async fn delete_service(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let result = sqlx::query!(r#"DELETE FROM services WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if result.rows_affected() == 0 {
        return Err(not_found("service not found"));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "service.delete",
        "service",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_env(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<EnvVarView>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT key, is_build_secret
        FROM env_vars
        WHERE service_id = $1
        ORDER BY key
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| EnvVarView {
                key: row.key,
                is_build_secret: row.is_build_secret,
            })
            .collect(),
    ))
}

async fn put_env(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<PutEnvVar>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    validate_env_key(&body.key).map_err(core_error)?;
    if body.value.contains('\0') || body.value.len() > 16_384 {
        return Err(bad(
            "invalid_value",
            "environment value cannot contain NUL and must be at most 16 KiB",
        ));
    }
    let encrypted = state.cipher.encrypt(body.value.as_bytes()).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "could not store environment value",
        )
    })?;
    let inserted = sqlx::query!(
        r#"
        INSERT INTO env_vars (service_id, key, value_enc, is_build_secret)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (service_id, key, is_build_secret)
        DO UPDATE SET value_enc = EXCLUDED.value_enc
        "#,
        id,
        body.key,
        encrypted.as_slice(),
        body.is_build_secret,
    )
    .execute(&state.pool)
    .await;
    match inserted {
        Ok(_) => {}
        Err(error) if is_fk(&error) => return Err(not_found("service not found")),
        Err(error) => return Err(ApiError::database(error)),
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "service.env_set",
        "service",
        Some(&id.to_string()),
        json!({ "key": body.key, "build_secret": body.is_build_secret }),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn delete_env(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, key)): Path<(Uuid, String)>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    sqlx::query!(
        r#"DELETE FROM env_vars WHERE service_id = $1 AND key = $2"#,
        id,
        key
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(sqlx::FromRow)]
struct ServerRow {
    id: Uuid,
    name: String,
    kind: String,
    status: String,
    public_ip: Option<String>,
    description: String,
    agent_version: Option<String>,
    docker_version: Option<String>,
    os_info: Option<String>,
    cpu_count: Option<i32>,
    memory_bytes: Option<i64>,
    last_heartbeat_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

fn server_from_row(row: ServerRow) -> ServerView {
    ServerView {
        id: row.id,
        name: row.name,
        kind: row.kind,
        status: row.status,
        public_ip: row.public_ip,
        description: row.description,
        agent_version: row.agent_version,
        docker_version: row.docker_version,
        os_info: row.os_info,
        cpu_count: row.cpu_count,
        memory_bytes: row.memory_bytes,
        last_heartbeat_at: row.last_heartbeat_at,
        created_at: row.created_at,
    }
}

#[derive(sqlx::FromRow)]
struct ServiceRow {
    id: Uuid,
    environment_id: Uuid,
    server_id: Uuid,
    name: String,
    kind: String,
    status: String,
    spec: Value,
    created_at: DateTime<Utc>,
}

fn service_from_row(row: ServiceRow) -> ServiceView {
    ServiceView {
        id: row.id,
        environment_id: row.environment_id,
        server_id: row.server_id,
        name: row.name,
        kind: row.kind,
        status: row.status,
        spec: row.spec,
        created_at: row.created_at,
    }
}

fn checked_spec(spec: Value) -> Result<Value, ApiError> {
    if !spec.is_object() {
        return Err(bad("invalid_spec", "service spec must be a JSON object"));
    }
    if spec.to_string().len() > 65_536 {
        return Err(bad("invalid_spec", "service spec is too large"));
    }
    Ok(spec)
}

fn check_kind(kind: &str) -> Result<(), ApiError> {
    if matches!(kind, "application" | "database" | "compose") {
        Ok(())
    } else {
        Err(bad(
            "invalid_kind",
            "kind must be application, database, or compose",
        ))
    }
}

fn core_error(error: CoreError) -> ApiError {
    match error {
        CoreError::InvalidName(name) => bad("invalid_name", format!("invalid name '{name}'")),
        CoreError::Validation(message) => bad("invalid", message),
        CoreError::Node(message) => ApiError::new(StatusCode::BAD_GATEWAY, "node", message),
        CoreError::Crypto => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "crypto failure",
        ),
    }
}

fn bad(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, code, message)
}

fn conflict(code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

fn not_found(message: impl Into<String>) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "not_found", message)
}

fn is_unique(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.is_unique_violation())
}

fn is_fk(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db.is_foreign_key_violation())
}
