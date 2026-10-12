//! Compose stack and template endpoints.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Json;
use rudder_core::{compose_variable_refs, validate_host, validate_name, Template, VariableKind};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::deploy::DeploymentView;
use crate::error::ApiError;
use crate::nodes::driver_for;
use crate::stacks::{self, StackAction, StackGit, StackSpec};
use crate::state::AppState;

use super::catalog::{
    bad, conflict, core_error, is_fk, is_unique, load_service_view, not_found, ServiceView,
};

#[derive(Deserialize, ToSchema)]
pub struct CreateStack {
    pub name: String,
    pub server_id: Uuid,
    /// `inline` (paste YAML) or `git`.
    pub source: String,
    pub compose_yaml: Option<String>,
    pub git: Option<StackGit>,
    /// Compose service new domains point at by default.
    pub main_service: Option<String>,
    /// Run `docker compose up` right away. Defaults to false.
    pub start: Option<bool>,
    /// Deploy also builds images that have a `build:` section. Defaults to false.
    pub build_on_deploy: Option<bool>,
    /// A push to the Git branch deploys the stack. Defaults to false.
    pub deploy_on_push: Option<bool>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateStack {
    pub source: String,
    pub compose_yaml: Option<String>,
    pub git: Option<StackGit>,
    pub main_service: Option<String>,
    /// Deploy also builds images that have a `build:` section. Unchanged when omitted.
    pub build_on_deploy: Option<bool>,
    /// A push to the Git branch deploys the stack. Unchanged when omitted.
    pub deploy_on_push: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct StackServiceView {
    pub name: String,
    pub container: String,
    pub image: Option<String>,
    /// Built from a `build:` section, so a plain deploy does not pick up new code.
    pub build: bool,
    pub routed: bool,
    /// Null when the server cannot be reached.
    pub running: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct StackView {
    pub service: ServiceView,
    pub project: String,
    pub source: String,
    pub compose_yaml: String,
    pub git: Option<StackGit>,
    pub template: Option<String>,
    pub main_service: Option<String>,
    /// The file Rudder runs: project name, labels, and proxy network applied.
    pub generated_yaml: String,
    pub compose_path: String,
    pub services: Vec<StackServiceView>,
    pub warnings: Vec<String>,
    /// `${NAME}` references in the compose file.
    pub variables: Vec<String>,
    /// Referenced variables with no value under Environment and no fallback in the file.
    pub missing_variables: Vec<String>,
    /// Every variable the file reads, and whether it is set or has a fallback.
    pub variable_details: Vec<StackVariableView>,
    /// Deploy also builds images that have a `build:` section.
    pub build_on_deploy: bool,
    /// A push to the Git branch deploys the stack.
    pub deploy_on_push: bool,
}

#[derive(Serialize, ToSchema)]
pub struct StackVariableView {
    pub name: String,
    /// A value is saved under Environment.
    pub set: bool,
    /// The file gives a fallback (`${NAME:-x}`) everywhere it reads the variable.
    pub has_default: bool,
    /// The file refuses to start without it (`${NAME:?error}`).
    pub required: bool,
}

#[derive(Serialize, ToSchema)]
pub struct StackLogsView {
    pub service: String,
    pub logs: String,
}

#[derive(Deserialize)]
pub struct StackLogsQuery {
    pub service: String,
    pub tail: Option<u32>,
}

#[derive(Deserialize)]
pub struct DeleteStackQuery {
    pub volumes: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct TemplateVariableView {
    pub name: String,
    pub label: String,
    pub description: String,
    /// domain, string, email, secret, number, or boolean.
    pub kind: String,
    pub default: Option<String>,
    pub required: bool,
}

#[derive(Serialize, ToSchema)]
pub struct TemplateView {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub tags: Vec<String>,
    pub website: String,
    pub main_service: String,
    pub port: u16,
    pub notes: String,
    pub variables: Vec<TemplateVariableView>,
    pub services: Vec<String>,
    pub images: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TemplateDetail {
    #[serde(flatten)]
    pub template: TemplateView,
    pub compose: String,
}

#[derive(Deserialize, ToSchema)]
pub struct DeployTemplate {
    pub name: String,
    pub server_id: Uuid,
    #[serde(default)]
    pub values: HashMap<String, String>,
    /// Run `docker compose up` right away. Defaults to true.
    pub start: Option<bool>,
}

#[derive(Serialize, ToSchema)]
pub struct GeneratedValue {
    pub key: String,
    pub value: String,
}

#[derive(Serialize, ToSchema)]
pub struct TemplateDeployed {
    pub service: ServiceView,
    pub domain: Option<String>,
    pub deployment: Option<DeploymentView>,
    /// Secrets generated for empty fields. Shown once; they are stored encrypted.
    pub generated: Vec<GeneratedValue>,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/environments/{id}/stacks", post(create_stack))
        .route(
            "/api/v1/services/{id}/stack",
            get(get_stack).put(update_stack).delete(delete_stack),
        )
        .route("/api/v1/services/{id}/stack/logs", get(stack_logs))
        .route("/api/v1/services/{id}/stack/cancel", post(cancel_stack))
        .route("/api/v1/services/{id}/stack/refresh", post(refresh_stack))
        .route("/api/v1/services/{id}/stack/{action}", post(stack_action))
        .route("/api/v1/templates", get(list_templates))
        .route("/api/v1/templates/{slug}", get(get_template))
        .route(
            "/api/v1/environments/{id}/templates/{slug}/deploy",
            post(deploy_template),
        )
}

fn source_parts(
    source: &str,
    compose_yaml: Option<String>,
    git: Option<StackGit>,
) -> Result<(String, Option<StackGit>), ApiError> {
    match source {
        "inline" => Ok((compose_yaml.unwrap_or_default(), None)),
        "git" => Ok((String::new(), git)),
        _ => Err(bad("invalid", "source must be inline or git")),
    }
}

async fn insert_stack(
    environment_id: Uuid,
    server_id: Uuid,
    name: &str,
    spec: &StackSpec,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> Result<Uuid, ApiError> {
    let source_type = format!("compose_{}", spec.source);
    let inserted = sqlx::query_scalar!(
        r#"
        INSERT INTO services (environment_id, server_id, name, kind, spec, source_type)
        VALUES ($1, $2, $3, 'compose', $4, $5)
        RETURNING id
        "#,
        environment_id,
        server_id,
        name,
        stacks::spec_json(spec),
        source_type
    )
    .fetch_one(&mut **tx)
    .await;
    match inserted {
        Ok(id) => Ok(id),
        Err(error) if is_unique(&error) => {
            Err(conflict("name_taken", "that service already exists"))
        }
        Err(error) if is_fk(&error) => Err(not_found("environment or server not found")),
        Err(error) => Err(ApiError::database(error)),
    }
}

async fn create_stack(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(environment_id): Path<Uuid>,
    Json(body): Json<CreateStack>,
) -> Result<(StatusCode, Json<StackView>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    let (compose_yaml, git) = source_parts(&body.source, body.compose_yaml, body.git)?;
    let mut spec = stacks::initial_spec(
        &body.name,
        &body.source,
        compose_yaml,
        git,
        body.main_service,
        None,
    )?;
    spec.build_on_deploy = body.build_on_deploy.unwrap_or(false);
    spec.deploy_on_push = body.deploy_on_push.unwrap_or(false);
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    let id = insert_stack(environment_id, body.server_id, &body.name, &spec, &mut tx).await?;
    tx.commit().await.map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "stack.create",
        "service",
        Some(&id.to_string()),
        json!({ "source": spec.source }),
        None,
    )
    .await;
    if body.start.unwrap_or(false) {
        stacks::start_action(&state, id, StackAction::Up, "create").await?;
    }
    Ok((StatusCode::CREATED, Json(stack_view(&state, id).await?)))
}

async fn stack_view(state: &AppState, id: Uuid) -> Result<StackView, ApiError> {
    let stack = stacks::load(state, id).await?;
    let service = load_service_view(state, id).await?;
    let running = stacks::running_states(state, &stack).await;
    let keys = stacks::env_keys(state, id).await?;
    let refs = compose_variable_refs(&stack.spec.compose_yaml);
    let variables: Vec<String> = refs.iter().map(|variable| variable.name.clone()).collect();
    let missing_variables = refs
        .iter()
        .filter(|variable| !variable.has_default && !keys.contains(&variable.name))
        .map(|variable| variable.name.clone())
        .collect();
    let variable_details = refs
        .into_iter()
        .map(|variable| StackVariableView {
            set: keys.contains(&variable.name),
            name: variable.name,
            has_default: variable.has_default,
            required: variable.required,
        })
        .collect();
    let spec = stack.spec;
    Ok(StackView {
        service,
        project: spec.project,
        source: spec.source,
        compose_yaml: spec.compose_yaml,
        git: spec.git,
        template: spec.template,
        main_service: spec.main_service,
        generated_yaml: spec.generated_yaml,
        compose_path: spec.compose_path,
        services: spec
            .services
            .into_iter()
            .zip(running)
            .map(|(service, running)| StackServiceView {
                name: service.name,
                container: service.container,
                image: service.image,
                build: service.build,
                routed: service.routed,
                running,
            })
            .collect(),
        warnings: spec.warnings,
        variables,
        missing_variables,
        variable_details,
        build_on_deploy: spec.build_on_deploy,
        deploy_on_push: spec.deploy_on_push,
    })
}

async fn get_stack(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<StackView>, ApiError> {
    Ok(Json(stack_view(&state, id).await?))
}

async fn update_stack(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateStack>,
) -> Result<Json<StackView>, ApiError> {
    auth.require_write()?;
    let mut stack = stacks::load(&state, id).await?;
    if stack.status == "deploying" {
        return Err(conflict(
            "deploy_in_progress",
            "wait for the running stack action",
        ));
    }
    let (compose_yaml, git) = source_parts(&body.source, body.compose_yaml, body.git)?;
    if stack.spec.project.is_empty() {
        let name = load_service_view(&state, id).await?.name;
        stack.spec.project = stacks::project_name(&name);
    }
    let switching = stack.spec.source != body.source;
    stack.spec.source = body.source;
    stack.spec.main_service = body.main_service;
    if let Some(build) = body.build_on_deploy {
        stack.spec.build_on_deploy = build;
    }
    if let Some(push) = body.deploy_on_push {
        stack.spec.deploy_on_push = push;
    }
    if stack.spec.source == "git" {
        let changed = switching
            || stack.spec.git.as_ref().is_none_or(|old| {
                git.as_ref()
                    .is_none_or(|new| old.repo != new.repo || old.compose_file != new.compose_file)
            });
        stack.spec.git = git;
        if changed {
            stack.spec.compose_yaml.clear();
        }
    } else {
        stack.spec.git = None;
        stack.spec.compose_yaml = compose_yaml;
    }
    if switching {
        stack.spec.compose_path.clear();
    }
    stacks::validate_source(&stack.spec)?;
    let routed = stacks::routed_services(&state, id, &stack.spec).await?;
    stacks::replan(&mut stack.spec, &routed)?;
    stacks::save(&state, id, &stack.spec).await?;
    let source_type = format!("compose_{}", stack.spec.source);
    sqlx::query!(
        r#"UPDATE services SET source_type = $2 WHERE id = $1"#,
        id,
        source_type
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "stack.update",
        "service",
        Some(&id.to_string()),
        json!({ "source": stack.spec.source }),
        None,
    )
    .await;
    Ok(Json(stack_view(&state, id).await?))
}

async fn stack_action(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((id, action)): Path<(Uuid, String)>,
) -> Result<(StatusCode, Json<DeploymentView>), ApiError> {
    auth.require_write()?;
    let action = StackAction::parse(&action)?;
    let deployment = stacks::start_action(&state, id, action, "manual").await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        &format!("stack.{}", action.as_str()),
        "service",
        Some(&id.to_string()),
        json!({ "deployment_id": deployment.id }),
        None,
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(deployment)))
}

/// Fetch the compose file from Git and plan it again, without deploying.
async fn refresh_stack(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<StackView>, ApiError> {
    auth.require_write()?;
    stacks::refresh_from_git(&state, id).await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "stack.refresh",
        "service",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(Json(stack_view(&state, id).await?))
}

async fn cancel_stack(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    stacks::cancel(&state, id).await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "stack.cancel",
        "service",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::ACCEPTED)
}

async fn stack_logs(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<StackLogsQuery>,
) -> Result<Json<StackLogsView>, ApiError> {
    let logs = stacks::service_logs(&state, id, &query.service, query.tail.unwrap_or(200)).await?;
    Ok(Json(StackLogsView {
        service: query.service,
        logs,
    }))
}

async fn delete_stack(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Query(query): Query<DeleteStackQuery>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let volumes = query.volumes.unwrap_or(false);
    let server_id = stacks::load(&state, id).await?.server_id;
    stacks::destroy(&state, id, volumes).await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "stack.delete",
        "service",
        Some(&id.to_string()),
        json!({ "volumes": volumes }),
        None,
    )
    .await;
    if let Err(error) = crate::publish::publish_server(&state, server_id).await {
        tracing::warn!(message = %error.public_message(), "routes were not republished");
    }
    Ok(StatusCode::NO_CONTENT)
}

fn kind_name(kind: VariableKind) -> &'static str {
    match kind {
        VariableKind::Domain => "domain",
        VariableKind::String => "string",
        VariableKind::Email => "email",
        VariableKind::Secret => "secret",
        VariableKind::Number => "number",
        VariableKind::Boolean => "boolean",
    }
}

fn template_view(template: &Template) -> TemplateView {
    let plan = rudder_core::transform_compose(&template.slug, &template.compose, &[]).ok();
    let services = plan
        .as_ref()
        .map(|plan| plan.services.iter().map(|s| s.name.clone()).collect())
        .unwrap_or_default();
    let mut images: Vec<String> = plan
        .map(|plan| plan.services.into_iter().filter_map(|s| s.image).collect())
        .unwrap_or_default();
    images.dedup();
    TemplateView {
        slug: template.slug.clone(),
        name: template.name.clone(),
        description: template.description.clone(),
        category: template.category.clone(),
        tags: template.tags.clone(),
        website: template.website.clone(),
        main_service: template.main_service.clone(),
        port: template.port,
        notes: template.notes.clone(),
        variables: template
            .variables
            .iter()
            .map(|variable| TemplateVariableView {
                name: variable.name.clone(),
                label: if variable.label.is_empty() {
                    variable.name.clone()
                } else {
                    variable.label.clone()
                },
                description: variable.description.clone(),
                kind: kind_name(variable.kind).into(),
                default: variable.default.clone(),
                required: variable.is_required(),
            })
            .collect(),
        services,
        images,
    }
}

async fn list_templates(_auth: AuthUser) -> Json<Vec<TemplateView>> {
    Json(crate::templates::all().iter().map(template_view).collect())
}

async fn get_template(
    _auth: AuthUser,
    Path(slug): Path<String>,
) -> Result<Json<TemplateDetail>, ApiError> {
    let template = crate::templates::get(&slug).ok_or_else(|| not_found("template not found"))?;
    Ok(Json(TemplateDetail {
        template: template_view(template),
        compose: template.compose.clone(),
    }))
}

/// Random alphanumeric string without modulo bias.
pub fn generate_secret(length: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = String::with_capacity(length);
    while out.len() < length {
        for byte in rudder_core::random_bytes::<64>() {
            if byte < 248 && out.len() < length {
                out.push(ALPHABET[usize::from(byte) % ALPHABET.len()] as char);
            }
        }
    }
    out
}

async fn deploy_template(
    State(state): State<AppState>,
    auth: AuthUser,
    Path((environment_id, slug)): Path<(Uuid, String)>,
    Json(body): Json<DeployTemplate>,
) -> Result<(StatusCode, Json<TemplateDeployed>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    let template = crate::templates::get(&slug).ok_or_else(|| not_found("template not found"))?;
    let resolved = template
        .resolve(&body.values, generate_secret)
        .map_err(core_error)?;
    let domain = match &resolved.domain {
        Some(host) => {
            let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
            validate_host(&host).map_err(core_error)?;
            Some(host)
        }
        None => None,
    };
    let generated: Vec<GeneratedValue> = template
        .variables
        .iter()
        .filter(|variable| {
            variable.kind == VariableKind::Secret
                && variable.default.is_none()
                && body
                    .values
                    .get(&variable.name)
                    .is_none_or(|value| value.trim().is_empty())
        })
        .filter_map(|variable| {
            resolved
                .env
                .iter()
                .find(|(key, _)| key == &variable.name)
                .map(|(key, value)| GeneratedValue {
                    key: key.clone(),
                    value: value.clone(),
                })
        })
        .collect();
    let spec = stacks::initial_spec(
        &body.name,
        "inline",
        template.compose.clone(),
        None,
        Some(template.main_service.clone()),
        Some(template.slug.clone()),
    )?;
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    let id = insert_stack(environment_id, body.server_id, &body.name, &spec, &mut tx).await?;
    for (key, value) in &resolved.env {
        let encrypted = state.cipher.encrypt(value.as_bytes()).map_err(core_error)?;
        sqlx::query!(
            r#"
            INSERT INTO env_vars (service_id, key, value_enc, is_build_secret)
            VALUES ($1, $2, $3, FALSE)
            "#,
            id,
            key,
            encrypted
        )
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database)?;
    }
    if let Some(host) = &domain {
        let inserted = sqlx::query!(
            r#"
            INSERT INTO domains (service_id, host, path_prefix, target_port, https_redirect,
                                 ssl_mode, compose_service)
            VALUES ($1, $2, '/', $3, TRUE, 'letsencrypt', $4)
            "#,
            id,
            host,
            i32::from(template.port),
            template.main_service
        )
        .execute(&mut *tx)
        .await;
        match inserted {
            Ok(_) => {}
            Err(error) if is_unique(&error) => {
                return Err(conflict("domain_taken", "that domain is already routed"))
            }
            Err(error) => return Err(ApiError::database(error)),
        }
    }
    tx.commit().await.map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "template.deploy",
        "service",
        Some(&id.to_string()),
        json!({ "template": template.slug, "domain": domain }),
        None,
    )
    .await;
    if domain.is_some() {
        if let Err(error) = crate::publish::publish_server(&state, body.server_id).await {
            tracing::warn!(message = %error.public_message(), "template route was not published");
        }
    }
    let deployment = if body.start.unwrap_or(true) {
        driver_for(&state, body.server_id).await?;
        Some(stacks::start_action(&state, id, StackAction::Up, "template").await?)
    } else {
        None
    };
    Ok((
        StatusCode::CREATED,
        Json(TemplateDeployed {
            service: load_service_view(&state, id).await?,
            domain,
            deployment,
            generated,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_secrets_are_alphanumeric_and_sized() {
        let secret = generate_secret(40);
        assert_eq!(secret.len(), 40);
        assert!(secret.bytes().all(|b| b.is_ascii_alphanumeric()));
        assert_ne!(secret, generate_secret(40));
    }

    #[test]
    fn every_template_has_a_view() {
        for template in crate::templates::all() {
            let view = template_view(template);
            assert!(view.services.contains(&view.main_service), "{}", view.slug);
            assert!(!view.images.is_empty());
        }
    }
}
