//! Compose stacks: one service row of kind `compose` that runs a whole
//! `docker compose` project.
//!
//! The user's compose file is kept as written in `spec.compose_yaml`. The
//! transformed file (project name, labels, proxy network) is stored in
//! `spec.generated_yaml` and written to the node next to a `.env` built from
//! the service's encrypted environment variables. Domains are routed by the
//! panel's Traefik file provider to the container named in `spec.containers`.

use std::collections::BTreeMap;
use std::path::Path;

use axum::http::StatusCode;
use rudder_core::{
    redact, render_env_file, transform_compose, validate_branch, validate_compose_service,
    validate_git_url, validate_rel_path, ComposeAction, NodeDriver, MAX_COMPOSE_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::deploy::{load_deployment, DeploymentView};
use crate::error::ApiError;
use crate::nodes::{driver_for, server_kind};
use crate::routes::catalog::{bad, conflict, core_error, not_found};
use crate::state::AppState;

pub const COMPOSE_FILE: &str = "rudder-compose.yml";

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct StackGit {
    pub provider_id: Uuid,
    pub repo: String,
    pub branch: String,
    /// Path of the compose file inside the repository.
    pub compose_file: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct StackServiceSpec {
    pub name: String,
    pub container: String,
    pub image: Option<String>,
    pub routed: bool,
}

/// Stored in `services.spec` for compose services.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StackSpec {
    pub project: String,
    /// `inline` or `git`.
    pub source: String,
    /// The compose file as written by the user, or as last fetched from Git.
    pub compose_yaml: String,
    pub git: Option<StackGit>,
    pub template: Option<String>,
    pub main_service: Option<String>,
    pub generated_yaml: String,
    pub services: Vec<StackServiceSpec>,
    /// Compose service name to container name; read by the route publisher.
    pub containers: BTreeMap<String, String>,
    pub warnings: Vec<String>,
    /// Generated file, relative to the node data directory. Empty until first written.
    pub compose_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackAction {
    Up,
    Down,
    Restart,
    Pull,
}

impl StackAction {
    pub fn parse(text: &str) -> Result<Self, ApiError> {
        match text {
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "restart" => Ok(Self::Restart),
            "pull" => Ok(Self::Pull),
            _ => Err(not_found("unknown stack action")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
            Self::Restart => "restart",
            Self::Pull => "pull",
        }
    }

    fn compose(self) -> ComposeAction {
        match self {
            Self::Up => ComposeAction::Up,
            Self::Down => ComposeAction::Down,
            Self::Restart => ComposeAction::Restart,
            Self::Pull => ComposeAction::Pull,
        }
    }

    fn writes_files(self) -> bool {
        matches!(self, Self::Up | Self::Pull)
    }
}

pub struct StackRow {
    pub id: Uuid,
    pub server_id: Uuid,
    pub status: String,
    pub spec: StackSpec,
}

/// Compose project name: the service name (at most 40 characters) plus six hex
/// characters, so two environments can hold a stack with the same name.
pub fn project_name(name: &str) -> String {
    let base: String = name.chars().take(40).collect();
    let suffix = &Uuid::new_v4().simple().to_string()[..6];
    format!("{}-{suffix}", base.trim_end_matches('-'))
}

pub fn check_compose_text(text: &str) -> Result<(), ApiError> {
    if text.trim().is_empty() {
        return Err(bad("invalid", "compose file is empty"));
    }
    if text.len() > MAX_COMPOSE_BYTES || text.contains('\0') {
        return Err(bad(
            "invalid",
            "compose file must be at most 256 KiB without NUL bytes",
        ));
    }
    Ok(())
}

pub fn check_git(git: &StackGit) -> Result<(), ApiError> {
    validate_git_url(&git.repo).map_err(core_error)?;
    validate_branch(&git.branch).map_err(core_error)?;
    validate_rel_path(&git.compose_file).map_err(core_error)?;
    if git.compose_file == "."
        || !(git.compose_file.ends_with(".yml") || git.compose_file.ends_with(".yaml"))
    {
        return Err(bad("invalid", "compose file must end in .yml or .yaml"));
    }
    Ok(())
}

pub async fn load(state: &AppState, id: Uuid) -> Result<StackRow, ApiError> {
    let row = sqlx::query!(
        r#"SELECT id, server_id, kind, status, spec FROM services WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))?;
    if row.kind != "compose" {
        return Err(bad("invalid_kind", "this service is not a compose stack"));
    }
    let spec: StackSpec = serde_json::from_value(row.spec)
        .map_err(|_| bad("invalid_spec", "stored stack spec is not valid"))?;
    Ok(StackRow {
        id: row.id,
        server_id: row.server_id,
        status: row.status,
        spec,
    })
}

pub async fn save(state: &AppState, id: Uuid, spec: &StackSpec) -> Result<(), ApiError> {
    let value = serde_json::to_value(spec).map_err(|_| bad("invalid_spec", "stack spec"))?;
    sqlx::query!(
        r#"UPDATE services SET spec = spec || $2, updated_at = now() WHERE id = $1"#,
        id,
        value
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(())
}

/// Compose services that need the proxy network: every domain target plus the main service.
pub async fn routed_services(
    state: &AppState,
    id: Uuid,
    spec: &StackSpec,
) -> Result<Vec<String>, ApiError> {
    let mut routed: Vec<String> = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT compose_service AS "service!"
        FROM domains
        WHERE service_id = $1 AND compose_service IS NOT NULL
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if let Some(main) = &spec.main_service {
        if !routed.contains(main) {
            routed.push(main.clone());
        }
    }
    routed.sort();
    Ok(routed)
}

/// Re-run the transform. A Git stack that was never fetched has no plan yet.
pub fn replan(spec: &mut StackSpec, routed: &[String]) -> Result<(), ApiError> {
    if spec.compose_yaml.trim().is_empty() {
        spec.generated_yaml.clear();
        spec.services.clear();
        spec.containers.clear();
        spec.warnings.clear();
        return Ok(());
    }
    let routed: Vec<&str> = routed.iter().map(String::as_str).collect();
    let plan = transform_compose(&spec.project, &spec.compose_yaml, &routed).map_err(core_error)?;
    spec.generated_yaml = plan.yaml;
    spec.warnings = plan.warnings;
    spec.containers = plan
        .services
        .iter()
        .map(|service| (service.name.clone(), service.container.clone()))
        .collect();
    spec.services = plan
        .services
        .into_iter()
        .map(|service| StackServiceSpec {
            name: service.name,
            container: service.container,
            image: service.image,
            routed: service.routed,
        })
        .collect();
    Ok(())
}

/// The compose service a new domain points at. Other kinds must not set one.
pub async fn domain_target(
    state: &AppState,
    service_id: Uuid,
    requested: Option<&str>,
) -> Result<Option<String>, ApiError> {
    let kind = sqlx::query_scalar!(r#"SELECT kind FROM services WHERE id = $1"#, service_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?
        .ok_or_else(|| not_found("service not found"))?;
    if kind != "compose" {
        if requested.is_some() {
            return Err(bad(
                "invalid",
                "compose_service only applies to compose stacks",
            ));
        }
        return Ok(None);
    }
    let stack = load(state, service_id).await?;
    let target = requested
        .map(str::to_owned)
        .or(stack.spec.main_service.clone())
        .ok_or_else(|| {
            bad(
                "missing_compose_service",
                "choose which compose service receives this domain",
            )
        })?;
    validate_compose_service(&target).map_err(core_error)?;
    if !stack.spec.services.is_empty() && !stack.spec.containers.contains_key(&target) {
        return Err(bad(
            "invalid",
            format!("compose service '{target}' is not in this stack"),
        ));
    }
    Ok(Some(target))
}

/// After a domain change, attach the target to the proxy network and re-apply a running stack.
pub fn refresh_network_later(state: &AppState, service_id: Uuid) {
    let state = state.clone();
    tokio::spawn(async move {
        let Ok(mut stack) = load(&state, service_id).await else {
            return;
        };
        let Ok(routed) = routed_services(&state, service_id, &stack.spec).await else {
            return;
        };
        let mut before: Vec<String> = stack
            .spec
            .services
            .iter()
            .filter(|service| service.routed)
            .map(|service| service.name.clone())
            .collect();
        before.sort();
        if stack.spec.services.is_empty() || before == routed {
            return;
        }
        if let Err(error) = replan(&mut stack.spec, &routed) {
            tracing::warn!(%service_id, message = %error.public_message(), "stack replan failed");
            return;
        }
        if save(&state, service_id, &stack.spec).await.is_err() {
            return;
        }
        if stack.status == "running" {
            if let Err(error) = start_action(&state, service_id, StackAction::Up, "domain").await {
                tracing::warn!(%service_id, message = %error.public_message(), "stack refresh did not start");
            }
        }
    });
}

async fn load_env(state: &AppState, id: Uuid) -> Result<Vec<(String, String)>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT key, value_enc
        FROM env_vars
        WHERE service_id = $1 AND is_build_secret = FALSE
        ORDER BY key
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let bytes = state.cipher.decrypt(&row.value_enc).map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "crypto_error",
                "stored environment value could not be decrypted",
            )
        })?;
        let value = String::from_utf8(bytes)
            .map_err(|_| bad("invalid", "stored environment value is not UTF-8"))?;
        out.push((row.key, value));
    }
    Ok(out)
}

pub async fn env_keys(state: &AppState, id: Uuid) -> Result<Vec<String>, ApiError> {
    sqlx::query_scalar!(
        r#"SELECT key FROM env_vars WHERE service_id = $1 AND is_build_secret = FALSE ORDER BY key"#,
        id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)
}

fn secret_values(env: &[(String, String)]) -> Vec<&str> {
    env.iter()
        .map(|(_, value)| value.as_str())
        .filter(|value| value.len() >= 4)
        .collect()
}

/// Lock the stack, record a deployment, and run the action in the background.
pub async fn start_action(
    state: &AppState,
    id: Uuid,
    action: StackAction,
    trigger: &str,
) -> Result<DeploymentView, ApiError> {
    let stack = load(state, id).await?;
    if stack.spec.project.is_empty() {
        return Err(conflict("not_configured", "save a compose file first"));
    }
    if stack.spec.source == "git" && server_kind(state, stack.server_id).await? != "local" {
        return Err(conflict(
            "remote_git",
            "Git-sourced stacks run on the local server; paste the compose file for remote servers",
        ));
    }
    if !action.writes_files() && stack.spec.compose_path.is_empty() {
        return Err(conflict(
            "not_deployed",
            "start the stack before this action",
        ));
    }
    driver_for(state, stack.server_id).await?;
    let previous = sqlx::query_scalar!(
        r#"
        WITH old AS (
            SELECT id, status FROM services WHERE id = $1 AND kind = 'compose' FOR UPDATE
        )
        UPDATE services s
        SET status = 'deploying', updated_at = now()
        FROM old
        WHERE s.id = old.id AND old.status <> 'deploying'
        RETURNING old.status
        "#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| conflict("deploy_in_progress", "a stack action is already running"))?;
    let deployment_id = sqlx::query_scalar!(
        r#"
        INSERT INTO deployments (service_id, status, trigger, logs, started_at)
        VALUES ($1, 'running', $2, '', now())
        RETURNING id
        "#,
        id,
        format!("{trigger}:{}", action.as_str())
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let task_state = state.clone();
    tokio::spawn(async move {
        execute(&task_state, id, deployment_id, action, &previous).await;
    });
    load_deployment(state, deployment_id).await
}

async fn execute(
    state: &AppState,
    id: Uuid,
    deployment_id: Uuid,
    action: StackAction,
    previous: &str,
) {
    let mut logs = String::new();
    let result = run(state, id, action, &mut logs).await;
    let (status, error, service_status) = match &result {
        Ok(()) => (
            "succeeded",
            None,
            match action {
                StackAction::Down => "stopped",
                StackAction::Pull => previous,
                StackAction::Up | StackAction::Restart => "running",
            },
        ),
        Err(message) => (
            "failed",
            Some(message.clone()),
            if action == StackAction::Up {
                "failed"
            } else {
                previous
            },
        ),
    };
    if let Some(message) = &error {
        logs.push_str(message);
        logs.push('\n');
    }
    let finished = sqlx::query!(
        r#"
        UPDATE deployments
        SET status = $2, error = $3, logs = $4, finished_at = now()
        WHERE id = $1
        "#,
        deployment_id,
        status,
        error,
        logs
    )
    .execute(&state.pool)
    .await;
    if let Err(error) = finished {
        tracing::error!(%error, "could not finish the stack deployment");
    }
    let _ = sqlx::query!(
        r#"UPDATE services SET status = $2, updated_at = now() WHERE id = $1"#,
        id,
        service_status
    )
    .execute(&state.pool)
    .await;
    let event = match &result {
        Ok(()) => crate::events::Event::info(
            &format!("stack.{}", action.as_str()),
            format!("Stack {} finished", action.as_str()),
        ),
        Err(message) => crate::events::Event::warning(
            "stack.failed",
            format!("Stack {} failed", action.as_str()),
        )
        .message(message.clone()),
    };
    crate::events::record(&state.pool, event.service(id)).await;
}

async fn run(
    state: &AppState,
    id: Uuid,
    action: StackAction,
    logs: &mut String,
) -> Result<(), String> {
    let mut stack = load(state, id)
        .await
        .map_err(|error| error.public_message())?;
    let env = load_env(state, id)
        .await
        .map_err(|error| error.public_message())?;
    let secrets = secret_values(&env);
    let driver = driver_for(state, stack.server_id)
        .await
        .map_err(|error| error.public_message())?;
    if action.writes_files() {
        write_files(state, &mut stack, driver.as_ref(), &env, logs).await?;
    }
    logs.push_str(&format!("docker compose {}\n", action.as_str()));
    let output = driver
        .compose(
            &stack.spec.project,
            &stack.spec.compose_path,
            action.compose(),
        )
        .await;
    match output {
        Ok(text) => {
            logs.push_str(&redact(&text, &secrets));
            Ok(())
        }
        Err(error) => Err(redact(&error.to_string(), &secrets)),
    }
}

async fn write_files(
    state: &AppState,
    stack: &mut StackRow,
    driver: &dyn NodeDriver,
    env: &[(String, String)],
    logs: &mut String,
) -> Result<(), String> {
    let project = stack.spec.project.clone();
    let dir = if stack.spec.source == "git" {
        let git = stack
            .spec
            .git
            .clone()
            .ok_or_else(|| "git source is not configured".to_string())?;
        let checkout = format!("stacks/{project}/src");
        let dest = state.config.data_dir.join(&checkout);
        logs.push_str(&format!("syncing {} ({})\n", git.repo, git.branch));
        let output = crate::build_queue::sync_repo(
            state,
            stack.id,
            git.provider_id,
            &git.repo,
            &git.branch,
            &dest,
        )
        .await?;
        logs.push_str(&output);
        let file = dest.join(&git.compose_file);
        let text = std::fs::read_to_string(&file)
            .map_err(|_| format!("{} was not found in the repository", git.compose_file))?;
        check_compose_text(&text).map_err(|error| error.public_message())?;
        stack.spec.compose_yaml = text;
        let routed = routed_services(state, stack.id, &stack.spec)
            .await
            .map_err(|error| error.public_message())?;
        replan(&mut stack.spec, &routed).map_err(|error| error.public_message())?;
        match Path::new(&git.compose_file).parent().and_then(Path::to_str) {
            Some(parent) if !parent.is_empty() => format!("{checkout}/{parent}"),
            _ => checkout,
        }
    } else {
        format!("stacks/{project}")
    };
    if stack.spec.generated_yaml.is_empty() {
        return Err("the stack has no compose file".into());
    }
    let env_file = render_env_file(env).map_err(|error| error.to_string())?;
    let compose_path = format!("{dir}/{COMPOSE_FILE}");
    driver
        .write_file(&compose_path, stack.spec.generated_yaml.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    driver
        .write_file(&format!("{dir}/.env"), env_file.as_bytes())
        .await
        .map_err(|error| error.to_string())?;
    stack.spec.compose_path = compose_path;
    save(state, stack.id, &stack.spec)
        .await
        .map_err(|error| error.public_message())?;
    for warning in &stack.spec.warnings {
        logs.push_str(&format!("warning: {warning}\n"));
    }
    logs.push_str(&format!(
        "wrote {} and .env ({} variables)\n",
        stack.spec.compose_path,
        env.len()
    ));
    Ok(())
}

/// Logs of one compose service, with environment values masked.
pub async fn service_logs(
    state: &AppState,
    id: Uuid,
    service: &str,
    tail: u32,
) -> Result<String, ApiError> {
    let stack = load(state, id).await?;
    let container = stack
        .spec
        .containers
        .get(service)
        .ok_or_else(|| not_found("that compose service is not in this stack"))?;
    let driver = driver_for(state, stack.server_id).await?;
    let logs = driver
        .container_logs(container, tail.min(5000))
        .await
        .map_err(core_error)?;
    let env = load_env(state, id).await?;
    Ok(redact(&logs, &secret_values(&env)))
}

/// Whether each compose service's container is running. `None` when the node is unreachable.
pub async fn running_states(state: &AppState, stack: &StackRow) -> Vec<Option<bool>> {
    let Ok(driver) = driver_for(state, stack.server_id).await else {
        return vec![None; stack.spec.services.len()];
    };
    let mut out = Vec::with_capacity(stack.spec.services.len());
    for service in &stack.spec.services {
        out.push(Some(
            driver
                .container_running(&service.container)
                .await
                .unwrap_or(false),
        ));
    }
    out
}

/// Stop the project, optionally drop its named volumes, blank the `.env`, and delete the row.
pub async fn destroy(state: &AppState, id: Uuid, volumes: bool) -> Result<(), ApiError> {
    let stack = load(state, id).await?;
    if stack.status == "deploying" {
        return Err(conflict("deploy_in_progress", "a stack action is running"));
    }
    if !stack.spec.compose_path.is_empty() {
        let driver = driver_for(state, stack.server_id).await?;
        let action = if volumes {
            ComposeAction::Destroy
        } else {
            ComposeAction::Down
        };
        let env = load_env(state, id).await?;
        driver
            .compose(&stack.spec.project, &stack.spec.compose_path, action)
            .await
            .map_err(|error| {
                ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "node",
                    redact(&error.to_string(), &secret_values(&env)),
                )
            })?;
        if let Some((dir, _)) = stack.spec.compose_path.rsplit_once('/') {
            let _ = driver.write_file(&format!("{dir}/.env"), b"").await;
        }
    }
    sqlx::query!(r#"DELETE FROM services WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    Ok(())
}

/// Fields a new stack row starts with.
pub fn initial_spec(
    name: &str,
    source: &str,
    compose_yaml: String,
    git: Option<StackGit>,
    main_service: Option<String>,
    template: Option<String>,
) -> Result<StackSpec, ApiError> {
    let mut spec = StackSpec {
        project: project_name(name),
        source: source.to_owned(),
        compose_yaml,
        git,
        template,
        main_service,
        ..StackSpec::default()
    };
    validate_source(&spec)?;
    let routed: Vec<String> = spec.main_service.iter().cloned().collect();
    replan(&mut spec, &routed)?;
    Ok(spec)
}

pub fn validate_source(spec: &StackSpec) -> Result<(), ApiError> {
    if let Some(main) = &spec.main_service {
        validate_compose_service(main).map_err(core_error)?;
    }
    match spec.source.as_str() {
        "inline" => check_compose_text(&spec.compose_yaml),
        "git" => {
            let git = spec
                .git
                .as_ref()
                .ok_or_else(|| bad("invalid", "git source needs a repository"))?;
            check_git(git)
        }
        _ => Err(bad("invalid", "source must be inline or git")),
    }
}

pub fn spec_json(spec: &StackSpec) -> Value {
    serde_json::to_value(spec).unwrap_or_else(|_| json!({}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_are_valid_and_unique() {
        let long = "a".repeat(63);
        let first = project_name(&long);
        assert_eq!(first.len(), 47);
        rudder_core::validate_name(&first).unwrap();
        assert_ne!(first, project_name(&long));
        assert!(project_name("web-").starts_with("web-"));
    }

    #[test]
    fn replan_fills_containers_and_clears_unfetched_git() {
        let mut spec = initial_spec(
            "blog",
            "inline",
            "services:\n  web:\n    image: nginx\n  db:\n    image: postgres\n".into(),
            None,
            Some("web".into()),
            None,
        )
        .unwrap();
        assert_eq!(spec.containers["web"], format!("{}-web-1", spec.project));
        assert!(spec.services.iter().any(|s| s.name == "web" && s.routed));
        assert!(spec.services.iter().any(|s| s.name == "db" && !s.routed));
        spec.compose_yaml.clear();
        replan(&mut spec, &[]).unwrap();
        assert!(spec.generated_yaml.is_empty() && spec.containers.is_empty());
    }

    #[test]
    fn rejects_bad_sources() {
        let missing = initial_spec(
            "blog",
            "inline",
            "services:\n  web:\n    image: nginx\n".into(),
            None,
            Some("api".into()),
            None,
        );
        assert!(missing.is_err());
        assert!(initial_spec("blog", "inline", " ".into(), None, None, None).is_err());
        assert!(initial_spec("blog", "ftp", "x".into(), None, None, None).is_err());
        let git = StackGit {
            provider_id: Uuid::nil(),
            repo: "https://github.com/acme/app".into(),
            branch: "main".into(),
            compose_file: "deploy/compose.yml".into(),
        };
        assert!(initial_spec("blog", "git", String::new(), Some(git.clone()), None, None).is_ok());
        for file in ["../compose.yml", "/etc/compose.yml", "compose.txt", "."] {
            let bad = StackGit {
                compose_file: file.into(),
                ..git.clone()
            };
            assert!(check_git(&bad).is_err(), "{file}");
        }
        assert!(check_git(&StackGit {
            branch: "-x".into(),
            ..git
        })
        .is_err());
    }
}
