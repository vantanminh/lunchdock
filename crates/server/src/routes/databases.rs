//! One-click databases and container lifecycle for any service.
//!
//! Passwords are generated here, stored encrypted in `env_vars`, and only
//! returned by the admin connection endpoint. They never enter audit metadata,
//! deployment logs, or tracing fields.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use base64::Engine as _;
use rudder_core::{
    connection_string, database_runtime, validate_name, ContainerSpec, DatabaseRequest, Engine,
    NodeDriver, PortMapping, VolumeMount, PROXY_NETWORK,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::nodes::{driver_for, server_kind};
use crate::state::AppState;

use super::catalog::{bad, conflict, core_error, is_fk, is_unique, not_found, ServiceView};

#[derive(Deserialize, ToSchema)]
pub struct CreateDatabase {
    pub name: String,
    /// postgres, mysql, mariadb, mongo, or redis.
    pub engine: String,
    pub version: String,
    pub server_id: Uuid,
    pub database: Option<String>,
    pub username: Option<String>,
    /// Publish the database port on the host. Omit to keep it internal.
    pub public_port: Option<u16>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    /// Start the container right away. Defaults to true.
    pub start: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateDatabase {
    pub public_port: Option<u16>,
    pub clear_public_port: Option<bool>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct DatabaseEngineView {
    pub engine: String,
    pub versions: Vec<String>,
    pub default_port: u16,
}

#[derive(Serialize, ToSchema)]
pub struct ConnectionView {
    pub engine: String,
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: String,
    pub internal_url: String,
    pub public_url: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct LifecycleView {
    pub status: String,
    pub container_id: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct LogsView {
    pub logs: String,
}

#[derive(Deserialize)]
pub struct LogsQuery {
    pub tail: Option<u32>,
}

#[derive(Deserialize)]
pub struct DeleteQuery {
    pub purge_volume: Option<bool>,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/database-engines", get(engines))
        .route("/api/v1/environments/{id}/databases", post(create_database))
        .route(
            "/api/v1/services/{id}/database",
            axum::routing::patch(update_database).delete(delete_database),
        )
        .route("/api/v1/services/{id}/connection", get(connection))
        .route("/api/v1/services/{id}/start", post(start))
        .route("/api/v1/services/{id}/stop", post(stop))
        .route("/api/v1/services/{id}/restart", post(restart))
        .route("/api/v1/services/{id}/logs", get(logs))
}

const ENGINES: &[(&str, &[&str])] = &[
    ("postgres", &["17", "16"]),
    ("mysql", &["8.4", "8.0"]),
    ("mariadb", &["11.4", "10.11"]),
    ("mongo", &["8", "7"]),
    ("redis", &["7.4", "7.2"]),
];

async fn engines(_auth: AuthUser) -> Json<Vec<DatabaseEngineView>> {
    Json(
        ENGINES
            .iter()
            .filter_map(|(name, versions)| {
                Engine::parse(name).ok().map(|engine| DatabaseEngineView {
                    engine: (*name).to_owned(),
                    versions: versions.iter().map(|v| (*v).to_owned()).collect(),
                    default_port: engine.port(),
                })
            })
            .collect(),
    )
}

/// URL-safe password with no shell or URL metacharacters: 32 characters.
pub fn generate_password() -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(rudder_core::random_bytes::<24>())
        .replace(['-', '_'], "x")
}

/// Everything needed to run or back up one database service.
#[derive(Clone)]
pub struct DatabaseInfo {
    pub service_id: Uuid,
    pub server_id: Uuid,
    pub engine: Engine,
    pub version: String,
    pub container: String,
    pub volume: String,
    pub database: String,
    pub username: String,
    pub password: String,
    pub root_password: Option<String>,
    pub public_port: Option<u16>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub current_container_id: Option<String>,
}

impl DatabaseInfo {
    pub fn credentials(&self) -> rudder_core::DumpCredentials {
        rudder_core::DumpCredentials {
            engine: self.engine,
            database: self.database.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            root_password: self.root_password.clone(),
        }
    }

    pub fn secrets(&self) -> Vec<&str> {
        let mut out = vec![self.password.as_str()];
        if let Some(root) = &self.root_password {
            out.push(root.as_str());
        }
        out
    }
}

fn password_key(engine: Engine) -> &'static str {
    match engine {
        Engine::Postgres => "POSTGRES_PASSWORD",
        Engine::Mysql => "MYSQL_PASSWORD",
        Engine::Mariadb => "MARIADB_PASSWORD",
        Engine::Mongo => "MONGO_INITDB_ROOT_PASSWORD",
        Engine::Redis => "REDIS_PASSWORD",
    }
}

fn root_key(engine: Engine) -> Option<&'static str> {
    match engine {
        Engine::Mysql => Some("MYSQL_ROOT_PASSWORD"),
        Engine::Mariadb => Some("MARIADB_ROOT_PASSWORD"),
        _ => None,
    }
}

fn spec_str(spec: &Value, key: &str) -> Option<String> {
    spec.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub async fn load_database(state: &AppState, service_id: Uuid) -> Result<DatabaseInfo, ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT id, server_id, kind, status, spec, current_container_id
        FROM services
        WHERE id = $1
        "#,
        service_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))?;
    if row.kind != "database" {
        return Err(bad("invalid_kind", "this service is not a database"));
    }
    let spec = row.spec;
    let engine =
        Engine::parse(&spec_str(&spec, "engine").unwrap_or_default()).map_err(core_error)?;
    let secrets = sqlx::query!(
        r#"SELECT key, value_enc FROM env_vars WHERE service_id = $1 AND is_build_secret = FALSE"#,
        service_id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let mut password = None;
    let mut root_password = None;
    for secret in secrets {
        let wanted = secret.key == password_key(engine);
        let wanted_root = root_key(engine) == Some(secret.key.as_str());
        if !wanted && !wanted_root {
            continue;
        }
        let bytes = state.cipher.decrypt(&secret.value_enc).map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "crypto_error",
                "stored database password could not be decrypted",
            )
        })?;
        let value = String::from_utf8(bytes)
            .map_err(|_| bad("invalid", "stored database password is not UTF-8"))?;
        if wanted {
            password = Some(value);
        } else {
            root_password = Some(value);
        }
    }
    let password = password.ok_or_else(|| {
        conflict(
            "missing_password",
            "the database password variable was removed; set it again under Environment",
        )
    })?;
    Ok(DatabaseInfo {
        service_id: row.id,
        server_id: row.server_id,
        engine,
        version: spec_str(&spec, "version").unwrap_or_default(),
        container: spec_str(&spec, "container").unwrap_or_default(),
        volume: spec_str(&spec, "volume").unwrap_or_default(),
        database: spec_str(&spec, "database").unwrap_or_default(),
        username: spec_str(&spec, "username").unwrap_or_default(),
        password,
        root_password,
        public_port: spec
            .get("public_port")
            .and_then(Value::as_u64)
            .and_then(|port| u16::try_from(port).ok()),
        memory_bytes: spec.get("memory_bytes").and_then(Value::as_i64),
        nano_cpus: spec.get("nano_cpus").and_then(Value::as_i64),
        current_container_id: row.current_container_id,
    })
}

fn container_spec(info: &DatabaseInfo) -> Result<ContainerSpec, ApiError> {
    let runtime = database_runtime(&DatabaseRequest {
        engine: info.engine,
        version: info.version.clone(),
        service: info.container.clone(),
        database: info.database.clone(),
        username: info.username.clone(),
        password: info.password.clone(),
        root_password: info.root_password.clone().unwrap_or_default(),
    })
    .map_err(core_error)?;
    Ok(ContainerSpec {
        name: info.container.clone(),
        image: runtime.image,
        env: runtime.env,
        command: runtime.command,
        ports: vec![PortMapping {
            container: runtime.port,
            host: info.public_port,
            protocol: "tcp".into(),
        }],
        volumes: vec![VolumeMount {
            source: runtime.volume_name,
            target: runtime.volume_target,
            read_only: false,
        }],
        memory_bytes: info.memory_bytes,
        nano_cpus: info.nano_cpus,
        restart: "unless-stopped".into(),
        network: Some(PROXY_NETWORK.into()),
        labels: vec![
            ("rudder.managed".into(), "true".into()),
            ("rudder.service".into(), info.service_id.to_string()),
            ("rudder.kind".into(), "database".into()),
            ("rudder.engine".into(), info.engine.as_str().into()),
        ],
    })
}

fn check_resources(memory: Option<i64>, cpus: Option<i64>) -> Result<(), ApiError> {
    if memory.is_some_and(|m| m < 6 * 1024 * 1024) {
        return Err(bad("invalid", "memory limit must be at least 6 MiB"));
    }
    if cpus.is_some_and(|c| c < 10_000_000) {
        return Err(bad("invalid", "CPU limit must be at least 0.01 CPU"));
    }
    Ok(())
}

async fn create_database(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(environment_id): Path<Uuid>,
    Json(body): Json<CreateDatabase>,
) -> Result<(StatusCode, Json<ServiceView>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    let engine = Engine::parse(&body.engine).map_err(core_error)?;
    check_resources(body.memory_bytes, body.nano_cpus)?;
    if body.public_port == Some(0) {
        return Err(bad("invalid", "public port must be 1 to 65535"));
    }
    let database = body.database.unwrap_or_else(|| "app".into());
    let username = body.username.unwrap_or_else(|| "app".into());
    let password = generate_password();
    let root_password = root_key(engine).map(|_| generate_password());
    let suffix = &Uuid::new_v4().simple().to_string()[..6];
    let base: String = body.name.chars().take(40).collect();
    let container = format!("{}-{suffix}", base.trim_end_matches('-'));
    // Validates version, identifiers, and password through the shared rules.
    let runtime = database_runtime(&DatabaseRequest {
        engine,
        version: body.version.clone(),
        service: container.clone(),
        database: database.clone(),
        username: username.clone(),
        password: password.clone(),
        root_password: root_password.clone().unwrap_or_default(),
    })
    .map_err(core_error)?;
    let spec = json!({
        "engine": engine.as_str(),
        "version": body.version,
        "database": database,
        "username": if engine == Engine::Redis { String::new() } else { username },
        "container": container,
        "volume": runtime.volume_name,
        "port": runtime.port,
        "public_port": body.public_port,
        "memory_bytes": body.memory_bytes,
        "nano_cpus": body.nano_cpus,
        "image": runtime.image,
    });
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    let inserted = sqlx::query!(
        r#"
        INSERT INTO services (environment_id, server_id, name, kind, spec, source_type)
        VALUES ($1, $2, $3, 'database', $4, 'database')
        RETURNING id
        "#,
        environment_id,
        body.server_id,
        body.name,
        spec,
    )
    .fetch_one(&mut *tx)
    .await;
    let service_id = match inserted {
        Ok(row) => row.id,
        Err(error) if is_unique(&error) => {
            return Err(conflict("name_taken", "that service already exists"))
        }
        Err(error) if is_fk(&error) => return Err(not_found("environment or server not found")),
        Err(error) => return Err(ApiError::database(error)),
    };
    let mut secrets: Vec<(String, String)> = runtime.env.clone();
    if engine == Engine::Redis {
        secrets.push(("REDIS_PASSWORD".into(), password.clone()));
    }
    for (key, value) in secrets {
        let encrypted = state.cipher.encrypt(value.as_bytes()).map_err(core_error)?;
        sqlx::query!(
            r#"
            INSERT INTO env_vars (service_id, key, value_enc, is_build_secret)
            VALUES ($1, $2, $3, FALSE)
            "#,
            service_id,
            key,
            encrypted,
        )
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database)?;
    }
    tx.commit().await.map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "database.create",
        "service",
        Some(&service_id.to_string()),
        json!({ "engine": engine.as_str(), "version": body.version }),
        None,
    )
    .await;
    if body.start.unwrap_or(true) {
        if server_kind(&state, body.server_id).await? == "local" {
            if let Err(error) = start_service(&state, service_id).await {
                tracing::warn!(%service_id, message = %error.public_message(), "database did not start");
            }
        } else {
            // The row stays so the database starts once the agent connects.
            driver_for(&state, body.server_id).await?;
            start_service(&state, service_id).await?;
        }
    }
    let view = super::catalog::load_service_view(&state, service_id).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn update_database(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateDatabase>,
) -> Result<Json<ServiceView>, ApiError> {
    auth.require_write()?;
    check_resources(body.memory_bytes, body.nano_cpus)?;
    let info = load_database(&state, id).await?;
    let public_port: Option<u16> = if body.clear_public_port.unwrap_or(false) {
        None
    } else {
        body.public_port.or(info.public_port)
    };
    let patch = json!({
        "public_port": public_port,
        "memory_bytes": body.memory_bytes.or(info.memory_bytes),
        "nano_cpus": body.nano_cpus.or(info.nano_cpus),
    });
    sqlx::query!(
        r#"UPDATE services SET spec = spec || $2, updated_at = now() WHERE id = $1"#,
        id,
        patch
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "database.update",
        "service",
        Some(&id.to_string()),
        json!({ "public": public_port.is_some() }),
        None,
    )
    .await;
    if info.current_container_id.is_some() {
        recreate(&state, id).await?;
    }
    Ok(Json(super::catalog::load_service_view(&state, id).await?))
}

async fn delete_database(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<DeleteQuery>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let info = load_database(&state, id).await?;
    if let Some(container) = &info.current_container_id {
        let driver = driver_for(&state, info.server_id).await?;
        if let Err(error) = driver.remove_container(container, true).await {
            tracing::warn!(%error, service_id = %id, "database container was not removed");
        }
        if query.purge_volume.unwrap_or(false) {
            driver
                .remove_volume(&info.volume)
                .await
                .map_err(core_error)?;
        }
    }
    sqlx::query!(r#"DELETE FROM services WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "database.delete",
        "service",
        Some(&id.to_string()),
        json!({ "purge_volume": query.purge_volume.unwrap_or(false) }),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn connection(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<ConnectionView>, ApiError> {
    auth.require_admin()?;
    auth.require_write()?;
    let info = load_database(&state, id).await?;
    let port = info.engine.port();
    let internal_url = connection_string(
        info.engine,
        &info.container,
        port,
        &info.database,
        &info.username,
        &info.password,
    )
    .map_err(core_error)?;
    let public_ip = sqlx::query_scalar!(
        r#"SELECT public_ip FROM servers WHERE id = $1"#,
        info.server_id
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let public_url = match (info.public_port, public_ip) {
        (Some(public_port), Some(ip)) if !ip.is_empty() => Some(
            connection_string(
                info.engine,
                &ip,
                public_port,
                &info.database,
                &info.username,
                &info.password,
            )
            .map_err(core_error)?,
        ),
        _ => None,
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "database.reveal_connection",
        "service",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(Json(ConnectionView {
        engine: info.engine.as_str().into(),
        host: info.container.clone(),
        port,
        database: info.database,
        username: info.username,
        password: info.password,
        internal_url,
        public_url,
    }))
}

/// Create and start the database container, or start the existing one.
pub async fn start_service(state: &AppState, id: Uuid) -> Result<LifecycleView, ApiError> {
    let row = service_row(state, id).await?;
    let driver = driver_for(state, row.server_id).await?;
    if row.kind == "database" {
        let info = load_database(state, id).await?;
        if let Some(container) = &info.current_container_id {
            if driver.container_running(container).await.unwrap_or(false) {
                return set_status(state, id, "running", Some(container.clone())).await;
            }
            if driver.start_container(container).await.is_ok() {
                return set_status(state, id, "running", Some(container.clone())).await;
            }
            let _ = driver.remove_container(container, true).await;
        }
        return create_database_container(state, driver.as_ref(), &info).await;
    }
    let container = row
        .current_container_id
        .ok_or_else(|| conflict("not_deployed", "deploy the service before starting it"))?;
    driver
        .start_container(&container)
        .await
        .map_err(core_error)?;
    set_status(state, id, "running", Some(container)).await
}

async fn create_database_container(
    state: &AppState,
    driver: &dyn NodeDriver,
    info: &DatabaseInfo,
) -> Result<LifecycleView, ApiError> {
    let spec = container_spec(info)?;
    sqlx::query!(
        r#"UPDATE services SET status = 'deploying', updated_at = now() WHERE id = $1"#,
        info.service_id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let result = async {
        driver.pull_image(&spec.image, None).await?;
        let id = driver.create_container(&spec).await?;
        if let Err(error) = driver.start_container(&id).await {
            let _ = driver.remove_container(&id, true).await;
            return Err(error);
        }
        Ok(id)
    }
    .await;
    match result {
        Ok(container_id) => {
            sqlx::query!(
                r#"
                UPDATE services
                SET current_container_id = $2, current_image = $3, status = 'running',
                    updated_at = now()
                WHERE id = $1
                "#,
                info.service_id,
                container_id,
                spec.image
            )
            .execute(&state.pool)
            .await
            .map_err(ApiError::database)?;
            crate::events::record(
                &state.pool,
                crate::events::Event::info("database.started", "Database started")
                    .service(info.service_id)
                    .server(info.server_id),
            )
            .await;
            Ok(LifecycleView {
                status: "running".into(),
                container_id: Some(container_id),
            })
        }
        Err(error) => {
            let message = rudder_core::redact(&error.to_string(), &info.secrets());
            let _ = set_status(state, info.service_id, "failed", None).await;
            Err(ApiError::new(StatusCode::BAD_GATEWAY, "node", message))
        }
    }
}

/// Replace the container with the current spec. The named volume is kept.
pub async fn recreate(state: &AppState, id: Uuid) -> Result<LifecycleView, ApiError> {
    let info = load_database(state, id).await?;
    let driver = driver_for(state, info.server_id).await?;
    if let Some(container) = &info.current_container_id {
        let _ = driver.stop_container(container, 30).await;
        driver
            .remove_container(container, true)
            .await
            .map_err(core_error)?;
    }
    create_database_container(state, driver.as_ref(), &info).await
}

pub async fn stop_service(state: &AppState, id: Uuid) -> Result<LifecycleView, ApiError> {
    let row = service_row(state, id).await?;
    let driver = driver_for(state, row.server_id).await?;
    let container = row
        .current_container_id
        .ok_or_else(|| conflict("not_deployed", "the service has no container"))?;
    driver
        .stop_container(&container, 30)
        .await
        .map_err(core_error)?;
    set_status(state, id, "stopped", Some(container)).await
}

struct ServiceRow {
    server_id: Uuid,
    kind: String,
    current_container_id: Option<String>,
}

async fn service_row(state: &AppState, id: Uuid) -> Result<ServiceRow, ApiError> {
    sqlx::query_as!(
        ServiceRow,
        r#"SELECT server_id, kind, current_container_id FROM services WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))
}

async fn set_status(
    state: &AppState,
    id: Uuid,
    status: &str,
    container: Option<String>,
) -> Result<LifecycleView, ApiError> {
    sqlx::query!(
        r#"UPDATE services SET status = $2, updated_at = now() WHERE id = $1"#,
        id,
        status
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(LifecycleView {
        status: status.into(),
        container_id: container,
    })
}

async fn lifecycle_audit(state: &AppState, auth: &AuthUser, id: Uuid, action: &str) {
    audit::record(
        &state.pool,
        Some(auth.id),
        action,
        "service",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
}

async fn start(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<LifecycleView>, ApiError> {
    auth.require_write()?;
    let view = start_service(&state, id).await?;
    lifecycle_audit(&state, &auth, id, "service.start").await;
    Ok(Json(view))
}

async fn stop(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<LifecycleView>, ApiError> {
    auth.require_write()?;
    let view = stop_service(&state, id).await?;
    lifecycle_audit(&state, &auth, id, "service.stop").await;
    Ok(Json(view))
}

async fn restart(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<LifecycleView>, ApiError> {
    auth.require_write()?;
    let row = service_row(&state, id).await?;
    let driver = driver_for(&state, row.server_id).await?;
    let container = row
        .current_container_id
        .ok_or_else(|| conflict("not_deployed", "the service has no container"))?;
    driver
        .restart_container(&container)
        .await
        .map_err(core_error)?;
    let view = set_status(&state, id, "running", Some(container)).await?;
    lifecycle_audit(&state, &auth, id, "service.restart").await;
    Ok(Json(view))
}

async fn logs(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<LogsQuery>,
) -> Result<Json<LogsView>, ApiError> {
    let row = service_row(&state, id).await?;
    let driver = driver_for(&state, row.server_id).await?;
    let container = row
        .current_container_id
        .ok_or_else(|| conflict("not_deployed", "the service has no container"))?;
    let logs = driver
        .container_logs(&container, query.tail.unwrap_or(200).min(5000))
        .await
        .map_err(core_error)?;
    let logs = if row.kind == "database" {
        match load_database(&state, id).await {
            Ok(info) => rudder_core::redact(&logs, &info.secrets()),
            Err(_) => logs,
        }
    } else {
        logs
    };
    Ok(Json(LogsView { logs }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_passwords_are_long_and_url_safe() {
        let first = generate_password();
        assert_eq!(first.len(), 32);
        assert!(first.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(first, generate_password());
    }
}
