//! Domains, certificates, registries, and image deploys.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Json;
use chrono::{DateTime, Utc};
use rudder_core::{
    matching_certificate, preview_hostname, validate_host, validate_key_and_chain, validate_name,
    validate_path_prefix, validate_registry_url, HostMatch,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::deploy::{self, health_from_request, DeploymentView};
use crate::error::ApiError;
use crate::publish::{self, expiry_unix_to_time};
use crate::state::AppState;

use super::catalog::{bad, conflict, core_error, is_fk, is_unique, not_found};

#[derive(Serialize, ToSchema)]
pub struct CertificateView {
    pub id: Uuid,
    pub name: String,
    pub common_name: Option<String>,
    pub sans: Vec<String>,
    pub issuer: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    pub not_after: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Deserialize, ToSchema)]
pub struct UploadCertificate {
    pub name: String,
    pub cert_pem: String,
    pub key_pem: String,
    pub chain_pem: Option<String>,
}

#[derive(Deserialize)]
pub struct HostQuery {
    pub host: String,
}

#[derive(Serialize, ToSchema)]
pub struct DomainView {
    pub id: Uuid,
    pub service_id: Uuid,
    pub host: String,
    pub path_prefix: String,
    pub target_port: i32,
    pub https_redirect: bool,
    pub ssl_mode: String,
    pub certificate_id: Option<Uuid>,
    pub matched_certificate_id: Option<Uuid>,
    pub matched_certificate_name: Option<String>,
    pub basic_auth_user: Option<String>,
    pub cache_enabled: bool,
    pub is_preview: bool,
    pub published: bool,
    /// Compose service inside a stack that receives the traffic.
    pub compose_service: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateDomain {
    pub host: String,
    pub path_prefix: Option<String>,
    pub target_port: i32,
    pub https_redirect: Option<bool>,
    pub ssl_mode: String,
    pub certificate_id: Option<Uuid>,
    pub basic_auth_user: Option<String>,
    pub basic_auth_password: Option<String>,
    /// Required for compose stacks unless the stack has a main service.
    pub compose_service: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateDomain {
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub target_port: Option<i32>,
    pub https_redirect: Option<bool>,
    pub ssl_mode: Option<String>,
    pub certificate_id: Option<Uuid>,
    pub basic_auth_user: Option<String>,
    pub basic_auth_password: Option<String>,
    pub clear_basic_auth: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct PreviewDomain {
    pub target_port: Option<i32>,
}

#[derive(Serialize, ToSchema)]
pub struct RegistryView {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub username: Option<String>,
    pub has_password: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct CreateRegistry {
    pub name: String,
    pub url: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct DeployBody {
    pub image: Option<String>,
    pub registry_id: Option<Uuid>,
    pub health_kind: Option<String>,
    pub health_path: Option<String>,
    pub health_port: Option<u16>,
    pub health_timeout_secs: Option<u64>,
    pub watch_tag: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct RollbackBody {
    pub deployment_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct PanelView {
    pub panel_domain: Option<String>,
    pub panel_ssl_mode: String,
    pub panel_certificate_id: Option<Uuid>,
    pub acme_email: Option<String>,
    pub matched_certificate_name: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdatePanel {
    pub panel_domain: Option<String>,
    pub panel_ssl_mode: Option<String>,
    pub panel_certificate_id: Option<Uuid>,
    pub acme_email: Option<String>,
    pub clear_domain: Option<bool>,
}

#[derive(Clone)]
struct CertificateRow {
    id: Uuid,
    name: String,
    common_name: Option<String>,
    sans: Vec<String>,
    issuer: Option<String>,
    not_before: Option<DateTime<Utc>>,
    not_after: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

struct DomainRow {
    id: Uuid,
    service_id: Uuid,
    host: String,
    path_prefix: String,
    target_port: i32,
    https_redirect: bool,
    ssl_mode: String,
    certificate_id: Option<Uuid>,
    basic_auth_user: Option<String>,
    cache_enabled: bool,
    is_preview: bool,
    compose_service: Option<String>,
}

struct RegistryRow {
    id: Uuid,
    name: String,
    url: String,
    username: Option<String>,
    has_password: bool,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/certificates",
            get(list_certificates).post(create_certificate),
        )
        .route("/api/v1/certificates/match", get(match_certificate))
        .route(
            "/api/v1/certificates/{id}",
            get(get_certificate)
                .put(replace_certificate)
                .delete(delete_certificate),
        )
        .route(
            "/api/v1/services/{id}/domains",
            get(list_domains).post(create_domain),
        )
        .route(
            "/api/v1/services/{id}/preview-domain",
            axum::routing::post(create_preview),
        )
        .route(
            "/api/v1/domains/{id}",
            axum::routing::patch(update_domain).delete(delete_domain),
        )
        .route(
            "/api/v1/registries",
            get(list_registries).post(create_registry),
        )
        .route(
            "/api/v1/registries/{id}",
            axum::routing::delete(delete_registry),
        )
        .route(
            "/api/v1/services/{id}/deploy",
            axum::routing::post(deploy_image),
        )
        .route(
            "/api/v1/services/{id}/rollback",
            axum::routing::post(rollback),
        )
        .route("/api/v1/services/{id}/deployments", get(list_deployments))
        .route("/api/v1/panel", get(get_panel).patch(update_panel))
}

async fn list_certificates(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<CertificateView>>, ApiError> {
    let rows = load_certificates(&state).await?;
    Ok(Json(rows.into_iter().map(certificate_view).collect()))
}

async fn create_certificate(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<UploadCertificate>,
) -> Result<(StatusCode, Json<CertificateView>), ApiError> {
    auth.require_write()?;
    let view = insert_certificate(&state, &auth, None, &body).await?;
    publish_all_local(&state).await?;
    Ok((StatusCode::CREATED, Json(view)))
}

async fn get_certificate(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CertificateView>, ApiError> {
    let row = load_certificate(&state, id).await?;
    Ok(Json(certificate_view(row)))
}

async fn replace_certificate(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UploadCertificate>,
) -> Result<Json<CertificateView>, ApiError> {
    auth.require_write()?;
    let _existing = load_certificate(&state, id).await?;
    let view = insert_certificate(&state, &auth, Some(id), &body).await?;
    publish_all_local(&state).await?;
    Ok(Json(view))
}

async fn delete_certificate(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let deleted = sqlx::query!(r#"DELETE FROM certificates WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if deleted.rows_affected() == 0 {
        return Err(not_found("certificate not found"));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "certificate.delete",
        "certificate",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    publish_all_local(&state).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn match_certificate(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(query): Query<HostQuery>,
) -> Result<Json<Option<CertificateView>>, ApiError> {
    validate_host(&query.host).map_err(core_error)?;
    let rows = load_certificates(&state).await?;
    let matches: Vec<HostMatch> = rows
        .iter()
        .map(|row| HostMatch {
            name: row.id.to_string(),
            common_name: row.common_name.clone(),
            sans: row.sans.clone(),
        })
        .collect();
    let found = matching_certificate(&query.host, &matches)
        .and_then(|item| rows.iter().find(|row| row.id.to_string() == item.name))
        .map(|row| certificate_view(row.clone()));
    Ok(Json(found))
}

async fn list_domains(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(service_id): Path<Uuid>,
) -> Result<Json<Vec<DomainView>>, ApiError> {
    let server_id = service_server(&state, service_id).await?;
    let local = server_is_local(&state, server_id).await?;
    let rows = domain_rows(&state, service_id).await?;
    let certs = load_certificates(&state).await?;
    let mut views = Vec::new();
    for row in rows {
        views.push(domain_view(row, &certs, local));
    }
    Ok(Json(views))
}

async fn create_domain(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(service_id): Path<Uuid>,
    Json(body): Json<CreateDomain>,
) -> Result<(StatusCode, Json<DomainView>), ApiError> {
    auth.require_write()?;
    let server_id = service_server(&state, service_id).await?;
    let domain = normalize_domain(
        &body.host,
        body.path_prefix.as_deref(),
        body.target_port,
        &body.ssl_mode,
    )?;
    let hash = basic_hash(
        body.basic_auth_user.as_deref(),
        body.basic_auth_password.as_deref(),
    )?;
    if let Some(certificate_id) = body.certificate_id {
        let _ = load_certificate(&state, certificate_id).await?;
    }
    let compose_service =
        crate::stacks::domain_target(&state, service_id, body.compose_service.as_deref()).await?;
    let inserted = sqlx::query_as!(
        DomainRow,
        r#"
        INSERT INTO domains (
            service_id, host, path_prefix, target_port, https_redirect, ssl_mode, certificate_id,
            basic_auth_user, basic_auth_hash, compose_service
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        RETURNING id, service_id, host, path_prefix, target_port, https_redirect, ssl_mode,
                  certificate_id, basic_auth_user, cache_enabled, is_preview, compose_service
        "#,
        service_id,
        domain.host,
        domain.path_prefix,
        domain.port,
        body.https_redirect.unwrap_or(true),
        domain.ssl_mode,
        body.certificate_id,
        body.basic_auth_user,
        hash,
        compose_service,
    )
    .fetch_one(&state.pool)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict(
                "domain_taken",
                "that host and path are already routed",
            ))
        }
        Err(error) if is_fk(&error) => return Err(not_found("service not found")),
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "domain.create",
        "domain",
        Some(&row.id.to_string()),
        json!({ "host": row.host, "ssl_mode": row.ssl_mode }),
        None,
    )
    .await;
    crate::stacks::refresh_network_later(&state, service_id);
    let published = publish::publish_server(&state, server_id).await?;
    let certs = load_certificates(&state).await?;
    Ok((
        StatusCode::CREATED,
        Json(domain_view(row, &certs, published)),
    ))
}

async fn create_preview(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(service_id): Path<Uuid>,
    Json(body): Json<PreviewDomain>,
) -> Result<(StatusCode, Json<DomainView>), ApiError> {
    auth.require_write()?;
    let (name, server_id, public_ip) = service_preview(&state, service_id).await?;
    let nonce = hex::encode(rudder_core::random_bytes::<4>());
    let host =
        preview_hostname(&name, &nonce, public_ip.as_deref().unwrap_or("")).map_err(core_error)?;
    let port = body.target_port.unwrap_or(80);
    if !(1..=65535).contains(&port) {
        return Err(bad("invalid", "target port must be between 1 and 65535"));
    }
    let compose_service = crate::stacks::domain_target(&state, service_id, None).await?;
    let inserted = sqlx::query_as!(
        DomainRow,
        r#"
        INSERT INTO domains (
            service_id, host, path_prefix, target_port, https_redirect, ssl_mode, is_preview,
            compose_service
        )
        VALUES ($1, $2, '/', $3, TRUE, 'letsencrypt', TRUE, $4)
        RETURNING id, service_id, host, path_prefix, target_port, https_redirect, ssl_mode,
                  certificate_id, basic_auth_user, cache_enabled, is_preview, compose_service
        "#,
        service_id,
        host,
        port,
        compose_service,
    )
    .fetch_one(&state.pool)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict(
                "domain_taken",
                "that host and path are already routed",
            ))
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "domain.preview",
        "domain",
        Some(&row.id.to_string()),
        json!({ "host": row.host }),
        None,
    )
    .await;
    let published = publish::publish_server(&state, server_id).await?;
    let certs = load_certificates(&state).await?;
    Ok((
        StatusCode::CREATED,
        Json(domain_view(row, &certs, published)),
    ))
}

async fn update_domain(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateDomain>,
) -> Result<Json<DomainView>, ApiError> {
    auth.require_write()?;
    let current = load_domain(&state, id).await?;
    let host = body.host.unwrap_or(current.host);
    let path = body.path_prefix.unwrap_or(current.path_prefix);
    let port = body.target_port.unwrap_or(current.target_port);
    let ssl_mode = body.ssl_mode.unwrap_or(current.ssl_mode);
    let normalized = normalize_domain(&host, Some(&path), port, &ssl_mode)?;
    let certificate_id = body.certificate_id.or(current.certificate_id);
    if let Some(certificate_id) = certificate_id {
        let _ = load_certificate(&state, certificate_id).await?;
    }
    let (user, hash) = if body.clear_basic_auth.unwrap_or(false) {
        (None, None)
    } else if body.basic_auth_password.is_some() {
        let user = body.basic_auth_user.clone().or(current.basic_auth_user);
        let hash = basic_hash(user.as_deref(), body.basic_auth_password.as_deref())?;
        (user, hash)
    } else {
        (current.basic_auth_user, None)
    };
    let row = if hash.is_some() || body.clear_basic_auth.unwrap_or(false) {
        sqlx::query_as!(
            DomainRow,
            r#"
            UPDATE domains
            SET host = $2, path_prefix = $3, target_port = $4, https_redirect = $5, ssl_mode = $6,
                certificate_id = $7, basic_auth_user = $8, basic_auth_hash = $9
            WHERE id = $1
            RETURNING id, service_id, host, path_prefix, target_port, https_redirect, ssl_mode,
                      certificate_id, basic_auth_user, cache_enabled, is_preview, compose_service
            "#,
            id,
            normalized.host,
            normalized.path_prefix,
            normalized.port,
            body.https_redirect.unwrap_or(current.https_redirect),
            normalized.ssl_mode,
            certificate_id,
            user,
            hash,
        )
        .fetch_one(&state.pool)
        .await
    } else {
        sqlx::query_as!(
            DomainRow,
            r#"
            UPDATE domains
            SET host = $2, path_prefix = $3, target_port = $4, https_redirect = $5, ssl_mode = $6,
                certificate_id = $7, basic_auth_user = COALESCE($8, basic_auth_user)
            WHERE id = $1
            RETURNING id, service_id, host, path_prefix, target_port, https_redirect, ssl_mode,
                      certificate_id, basic_auth_user, cache_enabled, is_preview, compose_service
            "#,
            id,
            normalized.host,
            normalized.path_prefix,
            normalized.port,
            body.https_redirect.unwrap_or(current.https_redirect),
            normalized.ssl_mode,
            certificate_id,
            user,
        )
        .fetch_one(&state.pool)
        .await
    };
    let row = match row {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict(
                "domain_taken",
                "that host and path are already routed",
            ))
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    let server_id = service_server(&state, row.service_id).await?;
    let published = publish::publish_server(&state, server_id).await?;
    let certs = load_certificates(&state).await?;
    Ok(Json(domain_view(row, &certs, published)))
}

async fn delete_domain(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let current = load_domain(&state, id).await?;
    sqlx::query!(r#"DELETE FROM domains WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let server_id = service_server(&state, current.service_id).await?;
    publish::publish_server(&state, server_id).await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "domain.delete",
        "domain",
        Some(&id.to_string()),
        json!({ "host": current.host }),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_registries(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<RegistryView>>, ApiError> {
    let rows = sqlx::query_as!(
        RegistryRow,
        r#"
        SELECT id, name, url, username, (password_enc IS NOT NULL) AS "has_password!"
        FROM registries
        ORDER BY name
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| RegistryView {
                id: row.id,
                name: row.name,
                url: row.url,
                username: row.username,
                has_password: row.has_password,
            })
            .collect(),
    ))
}

async fn create_registry(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CreateRegistry>,
) -> Result<(StatusCode, Json<RegistryView>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    let url = validate_registry_url(&body.url).map_err(core_error)?;
    if let Some(username) = &body.username {
        if username.len() > 256 || username.contains(['\n', '\r', '\0']) {
            return Err(bad("invalid", "invalid registry username"));
        }
    }
    let password = match body.password.as_deref() {
        Some(password) if !password.is_empty() => {
            if password.len() > 4096 {
                return Err(bad("invalid", "registry password is too long"));
            }
            Some(
                state
                    .cipher
                    .encrypt(password.as_bytes())
                    .map_err(core_error)?,
            )
        }
        _ => None,
    };
    let inserted = sqlx::query_as!(
        RegistryRow,
        r#"
        INSERT INTO registries (name, url, username, password_enc)
        VALUES ($1, $2, $3, $4)
        RETURNING id, name, url, username, (password_enc IS NOT NULL) AS "has_password!"
        "#,
        body.name,
        url,
        body.username,
        password,
    )
    .fetch_one(&state.pool)
    .await;
    let row = match inserted {
        Ok(row) => row,
        Err(error) if is_unique(&error) => {
            return Err(conflict("name_taken", "a registry with that name exists"))
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "registry.create",
        "registry",
        Some(&row.id.to_string()),
        json!({ "name": row.name, "url": row.url }),
        None,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(RegistryView {
            id: row.id,
            name: row.name,
            url: row.url,
            username: row.username,
            has_password: row.has_password,
        }),
    ))
}

async fn delete_registry(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let deleted = sqlx::query!(r#"DELETE FROM registries WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if deleted.rows_affected() == 0 {
        return Err(not_found("registry not found"));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "registry.delete",
        "registry",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn deploy_image(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<DeployBody>,
) -> Result<Json<DeploymentView>, ApiError> {
    auth.require_write()?;
    let health = health_from_request(
        body.health_kind.as_deref(),
        body.health_path.as_deref(),
        body.health_port,
        body.health_timeout_secs,
    )?;
    let view = deploy::deploy_service(
        &state,
        deploy::DeployParams {
            service_id: id,
            image: body.image,
            registry_id: body.registry_id,
            health,
            trigger: "manual".into(),
            actor: Some(auth.id),
            watch_tag: body.watch_tag,
        },
    )
    .await?;
    Ok(Json(view))
}

async fn rollback(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<RollbackBody>,
) -> Result<Json<DeploymentView>, ApiError> {
    auth.require_write()?;
    let view = deploy::rollback_image(&state, id, body.deployment_id, Some(auth.id)).await?;
    Ok(Json(view))
}

async fn list_deployments(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<DeploymentView>>, ApiError> {
    let _server = service_server(&state, id).await?;
    Ok(Json(deploy::list_deployments(&state, id).await?))
}

async fn get_panel(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<PanelView>, ApiError> {
    Ok(Json(load_panel(&state).await?))
}

async fn update_panel(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<UpdatePanel>,
) -> Result<Json<PanelView>, ApiError> {
    auth.require_write()?;
    if let Some(mode) = &body.panel_ssl_mode {
        publish::tls_mode(mode)?;
    }
    if let Some(domain) = &body.panel_domain {
        if !domain.is_empty() {
            validate_host(domain).map_err(core_error)?;
        }
    }
    if let Some(certificate_id) = body.panel_certificate_id {
        let _ = load_certificate(&state, certificate_id).await?;
    }
    let domain = if body.clear_domain.unwrap_or(false) {
        None
    } else {
        body.panel_domain
            .as_ref()
            .map(|value| {
                if value.is_empty() {
                    None
                } else {
                    Some(value.clone())
                }
            })
            .unwrap_or(None)
    };
    sqlx::query!(
        r#"
        UPDATE panel_settings
        SET panel_domain = CASE WHEN $1 THEN NULL ELSE COALESCE($2, panel_domain) END,
            panel_ssl_mode = COALESCE($3, panel_ssl_mode),
            panel_certificate_id = COALESCE($4, panel_certificate_id),
            acme_email = COALESCE($5, acme_email),
            updated_at = now()
        WHERE id = TRUE
        "#,
        body.clear_domain.unwrap_or(false),
        domain,
        body.panel_ssl_mode,
        body.panel_certificate_id,
        body.acme_email,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if let Some(server_id) = local_server_id(&state).await? {
        publish::publish_server(&state, server_id).await?;
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "panel.update",
        "panel",
        None,
        json!({ "ssl_mode": body.panel_ssl_mode }),
        None,
    )
    .await;
    Ok(Json(load_panel(&state).await?))
}

struct NormalizedDomain {
    host: String,
    path_prefix: String,
    port: i32,
    ssl_mode: String,
}

fn normalize_domain(
    host: &str,
    path_prefix: Option<&str>,
    port: i32,
    ssl_mode: &str,
) -> Result<NormalizedDomain, ApiError> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    validate_host(&host).map_err(core_error)?;
    let path_prefix = path_prefix.unwrap_or("/").to_owned();
    validate_path_prefix(&path_prefix).map_err(core_error)?;
    if !(1..=65535).contains(&port) {
        return Err(bad("invalid", "target port must be between 1 and 65535"));
    }
    publish::tls_mode(ssl_mode)?;
    Ok(NormalizedDomain {
        host,
        path_prefix,
        port,
        ssl_mode: ssl_mode.to_owned(),
    })
}

fn basic_hash(user: Option<&str>, password: Option<&str>) -> Result<Option<String>, ApiError> {
    match (user, password) {
        (None, None) | (None, Some("")) => Ok(None),
        (Some(user), Some(password)) if !user.is_empty() && !password.is_empty() => {
            if user.len() > 64
                || !user
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                return Err(bad("invalid", "invalid basic auth user"));
            }
            if password.len() > 256 {
                return Err(bad("invalid", "basic auth password is too long"));
            }
            let hash = bcrypt::hash(password, 8).map_err(|_| {
                ApiError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "crypto_error",
                    "could not hash the basic auth password",
                )
            })?;
            Ok(Some(hash))
        }
        _ => Err(bad(
            "invalid",
            "basic auth needs both a user and a password",
        )),
    }
}

async fn insert_certificate(
    state: &AppState,
    auth: &AuthUser,
    existing: Option<Uuid>,
    body: &UploadCertificate,
) -> Result<CertificateView, ApiError> {
    validate_name(&body.name).map_err(core_error)?;
    let details = validate_key_and_chain(&body.cert_pem, &body.key_pem, body.chain_pem.as_deref())
        .map_err(core_error)?;
    let cert_enc = state
        .cipher
        .encrypt(body.cert_pem.as_bytes())
        .map_err(core_error)?;
    let key_enc = state
        .cipher
        .encrypt(body.key_pem.as_bytes())
        .map_err(core_error)?;
    let chain_enc = match body
        .chain_pem
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(chain) => Some(state.cipher.encrypt(chain.as_bytes()).map_err(core_error)?),
        None => None,
    };
    let not_before = expiry_unix_to_time(details.not_before_unix);
    let not_after = expiry_unix_to_time(details.not_after_unix);
    let row = if let Some(id) = existing {
        sqlx::query_as!(
            CertificateRow,
            r#"
            UPDATE certificates
            SET name = $2, cert_pem_enc = $3, key_pem_enc = $4, chain_pem_enc = $5,
                common_name = $6, sans = $7, issuer = $8, not_before = $9, not_after = $10,
                updated_at = now()
            WHERE id = $1
            RETURNING id, name, common_name, sans, issuer, not_before, not_after, created_at
            "#,
            id,
            body.name,
            cert_enc,
            key_enc,
            chain_enc,
            details.common_name,
            &details.sans,
            details.issuer,
            not_before,
            not_after,
        )
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::database)?
    } else {
        sqlx::query_as!(
            CertificateRow,
            r#"
            INSERT INTO certificates (
                name, cert_pem_enc, key_pem_enc, chain_pem_enc, common_name, sans, issuer,
                not_before, not_after
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, name, common_name, sans, issuer, not_before, not_after, created_at
            "#,
            body.name,
            cert_enc,
            key_enc,
            chain_enc,
            details.common_name,
            &details.sans,
            details.issuer,
            not_before,
            not_after,
        )
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::database)?
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        if existing.is_some() {
            "certificate.replace"
        } else {
            "certificate.create"
        },
        "certificate",
        Some(&row.id.to_string()),
        json!({ "name": row.name, "sans": row.sans }),
        None,
    )
    .await;
    Ok(certificate_view(row))
}

fn certificate_view(row: CertificateRow) -> CertificateView {
    CertificateView {
        id: row.id,
        name: row.name,
        common_name: row.common_name,
        sans: row.sans,
        issuer: row.issuer,
        not_before: row.not_before,
        not_after: row.not_after,
        created_at: row.created_at,
    }
}

async fn load_certificates(state: &AppState) -> Result<Vec<CertificateRow>, ApiError> {
    sqlx::query_as!(
        CertificateRow,
        r#"
        SELECT id, name, common_name, sans, issuer, not_before, not_after, created_at
        FROM certificates
        ORDER BY name
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)
}

async fn load_certificate(state: &AppState, id: Uuid) -> Result<CertificateRow, ApiError> {
    sqlx::query_as!(
        CertificateRow,
        r#"
        SELECT id, name, common_name, sans, issuer, not_before, not_after, created_at
        FROM certificates
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("certificate not found"))
}

async fn domain_rows(state: &AppState, service_id: Uuid) -> Result<Vec<DomainRow>, ApiError> {
    sqlx::query_as!(
        DomainRow,
        r#"
        SELECT id, service_id, host, path_prefix, target_port, https_redirect, ssl_mode,
               certificate_id, basic_auth_user, cache_enabled, is_preview, compose_service
        FROM domains
        WHERE service_id = $1
        ORDER BY host, path_prefix
        "#,
        service_id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)
}

async fn load_domain(state: &AppState, id: Uuid) -> Result<DomainRow, ApiError> {
    sqlx::query_as!(
        DomainRow,
        r#"
        SELECT id, service_id, host, path_prefix, target_port, https_redirect, ssl_mode,
               certificate_id, basic_auth_user, cache_enabled, is_preview, compose_service
        FROM domains
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("domain not found"))
}

fn domain_view(row: DomainRow, certs: &[CertificateRow], published: bool) -> DomainView {
    let matches: Vec<HostMatch> = certs
        .iter()
        .map(|cert| HostMatch {
            name: cert.id.to_string(),
            common_name: cert.common_name.clone(),
            sans: cert.sans.clone(),
        })
        .collect();
    let matched = matching_certificate(&row.host, &matches);
    let matched_certificate_id = matched.and_then(|item| Uuid::parse_str(&item.name).ok());
    let matched_certificate_name = matched.and_then(|item| {
        certs
            .iter()
            .find(|cert| cert.id.to_string() == item.name)
            .map(|cert| cert.name.clone())
    });
    DomainView {
        id: row.id,
        service_id: row.service_id,
        host: row.host,
        path_prefix: row.path_prefix,
        target_port: row.target_port,
        https_redirect: row.https_redirect,
        ssl_mode: row.ssl_mode,
        certificate_id: row.certificate_id,
        matched_certificate_id,
        matched_certificate_name,
        basic_auth_user: row.basic_auth_user,
        cache_enabled: row.cache_enabled,
        is_preview: row.is_preview,
        published,
        compose_service: row.compose_service,
    }
}

async fn server_is_local(state: &AppState, server_id: Uuid) -> Result<bool, ApiError> {
    let kind = sqlx::query_scalar!(r#"SELECT kind FROM servers WHERE id = $1"#, server_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| not_found("server not found"))?;
    Ok(kind == "local")
}

async fn service_server(state: &AppState, service_id: Uuid) -> Result<Uuid, ApiError> {
    sqlx::query_scalar!(
        r#"SELECT server_id FROM services WHERE id = $1"#,
        service_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))
}

async fn service_preview(
    state: &AppState,
    service_id: Uuid,
) -> Result<(String, Uuid, Option<String>), ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT s.name, s.server_id, servers.public_ip
        FROM services s
        JOIN servers ON servers.id = s.server_id
        WHERE s.id = $1
        "#,
        service_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Err(not_found("service not found"));
    };
    Ok((row.name, row.server_id, row.public_ip))
}

async fn local_server_id(state: &AppState) -> Result<Option<Uuid>, ApiError> {
    sqlx::query_scalar!(r#"SELECT id FROM servers WHERE kind = 'local' LIMIT 1"#)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)
}

async fn publish_all_local(state: &AppState) -> Result<(), ApiError> {
    if let Some(server_id) = local_server_id(state).await? {
        publish::publish_server(state, server_id).await?;
    }
    Ok(())
}

async fn load_panel(state: &AppState) -> Result<PanelView, ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT panel_domain, panel_ssl_mode, panel_certificate_id, acme_email
        FROM panel_settings
        WHERE id = TRUE
        "#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let certs = load_certificates(state).await?;
    let matched_certificate_name = row.panel_domain.as_deref().and_then(|host| {
        let matches: Vec<HostMatch> = certs
            .iter()
            .map(|cert| HostMatch {
                name: cert.name.clone(),
                common_name: cert.common_name.clone(),
                sans: cert.sans.clone(),
            })
            .collect();
        matching_certificate(host, &matches).map(|item| item.name.clone())
    });
    Ok(PanelView {
        panel_domain: row.panel_domain,
        panel_ssl_mode: row.panel_ssl_mode,
        panel_certificate_id: row.panel_certificate_id,
        acme_email: row.acme_email,
        matched_certificate_name,
    })
}
