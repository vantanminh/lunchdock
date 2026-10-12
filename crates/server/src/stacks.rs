//! Compose stacks: one service row of kind `compose` that runs a whole
//! `docker compose` project.
//!
//! The user's compose file is kept as written in `spec.compose_yaml`. The
//! transformed file (project name, labels, proxy network) is stored in
//! `spec.generated_yaml` and written to the node next to a `.env` built from
//! the service's encrypted environment variables. Domains are routed by the
//! panel's Traefik file provider to the container named in `spec.containers`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use rudder_core::{
    redact, render_env_file, transform_compose, validate_branch, validate_compose_service,
    validate_git_url, validate_rel_path, ComposeAction, ComposeOptions, LogSink, NodeDriver,
    RegistryAuth, MAX_COMPOSE_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
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
    /// Built from a `build:` section instead of (or as well as) pulled.
    #[serde(default)]
    pub build: bool,
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
    /// Plain Deploy also builds images that have a `build:` section.
    pub build_on_deploy: bool,
    /// A push to the Git source's branch deploys the stack (GitHub App providers).
    pub deploy_on_push: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StackAction {
    Up,
    /// `up --build`: rebuild images from their `build:` sections, then start.
    Rebuild,
    /// `up --pull always`: fetch newer images, then start.
    Update,
    Down,
    Restart,
    Pull,
}

impl StackAction {
    pub fn parse(text: &str) -> Result<Self, ApiError> {
        match text {
            "up" => Ok(Self::Up),
            "rebuild" => Ok(Self::Rebuild),
            "update" => Ok(Self::Update),
            "down" => Ok(Self::Down),
            "restart" => Ok(Self::Restart),
            "pull" => Ok(Self::Pull),
            _ => Err(not_found("unknown stack action")),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Rebuild => "rebuild",
            Self::Update => "update",
            Self::Down => "down",
            Self::Restart => "restart",
            Self::Pull => "pull",
        }
    }

    /// The compose command for this action; a stack that builds on deploy turns Up into a rebuild.
    fn compose(self, build_on_deploy: bool) -> ComposeAction {
        match self {
            Self::Up if build_on_deploy => ComposeAction::UpBuild,
            Self::Up => ComposeAction::Up,
            Self::Rebuild => ComposeAction::UpBuild,
            Self::Update => ComposeAction::UpPull,
            Self::Down => ComposeAction::Down,
            Self::Restart => ComposeAction::Restart,
            Self::Pull => ComposeAction::Pull,
        }
    }

    fn writes_files(self) -> bool {
        matches!(self, Self::Up | Self::Rebuild | Self::Update | Self::Pull)
    }

    /// Brings containers up, so a failure leaves the service failed.
    fn starts(self) -> bool {
        matches!(self, Self::Up | Self::Rebuild | Self::Update)
    }

    /// Talks to registries, so saved logins apply.
    fn pulls(self) -> bool {
        self.writes_files()
    }
}

/// Compose actions that are running now, so one can be cancelled.
#[derive(Default)]
pub struct StackRuns {
    running: Mutex<HashMap<Uuid, CancellationToken>>,
}

/// Removes a stack from [`StackRuns`] when its action ends, however it ends.
pub struct RunGuard {
    runs: Arc<StackRuns>,
    id: Uuid,
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if let Ok(mut running) = self.runs.running.lock() {
            running.remove(&self.id);
        }
    }
}

impl StackRuns {
    fn begin(self: &Arc<Self>, id: Uuid) -> (CancellationToken, RunGuard) {
        let token = CancellationToken::new();
        if let Ok(mut running) = self.running.lock() {
            running.insert(id, token.clone());
        }
        (
            token,
            RunGuard {
                runs: self.clone(),
                id,
            },
        )
    }

    /// Ask the running action of `id` to stop. False when nothing is running.
    pub fn cancel(&self, id: Uuid) -> bool {
        let token = self
            .running
            .lock()
            .ok()
            .and_then(|running| running.get(&id).cloned());
        match token {
            Some(token) => {
                token.cancel();
                true
            }
            None => false,
        }
    }
}

/// Most output kept for one action; older lines are dropped and the log says so.
const MAX_LOG_BYTES: usize = 1024 * 1024;
const FLUSH_EVERY: Duration = Duration::from_secs(1);

/// The output of a running action, with secrets masked, written to the
/// deployment row while the action runs so the panel can follow it.
#[derive(Clone, Default)]
pub struct LiveLog {
    inner: Arc<Mutex<LogState>>,
}

#[derive(Default)]
struct LogState {
    text: String,
    dropped: bool,
    dirty: bool,
    secrets: Vec<String>,
}

impl LiveLog {
    /// Values to mask in everything pushed from now on.
    pub fn set_secrets(&self, secrets: Vec<String>) {
        if let Ok(mut state) = self.inner.lock() {
            state.secrets = secrets
                .into_iter()
                .filter(|value| value.len() >= 4)
                .collect();
        }
    }

    pub fn push(&self, text: &str) {
        let Ok(mut state) = self.inner.lock() else {
            return;
        };
        let masked = {
            let secrets: Vec<&str> = state.secrets.iter().map(String::as_str).collect();
            redact(text, &secrets)
        };
        state.text.push_str(&masked);
        state.dirty = true;
        if state.text.len() > MAX_LOG_BYTES + MAX_LOG_BYTES / 4 {
            let mut cut = state.text.len() - MAX_LOG_BYTES;
            while !state.text.is_char_boundary(cut) {
                cut += 1;
            }
            state.text.drain(..cut);
            state.dropped = true;
        }
    }

    pub fn snapshot(&self) -> String {
        match self.inner.lock() {
            Ok(state) if state.dropped => {
                format!("[earlier output was dropped]\n{}", state.text)
            }
            Ok(state) => state.text.clone(),
            Err(_) => String::new(),
        }
    }

    /// The whole log when something was added since the last call.
    fn take_dirty(&self) -> Option<String> {
        let changed = {
            let mut state = self.inner.lock().ok()?;
            std::mem::take(&mut state.dirty)
        };
        changed.then(|| self.snapshot())
    }
}

/// Write the log to the deployment row every second until the task is aborted.
fn spawn_flusher(
    state: &AppState,
    deployment_id: Uuid,
    log: &LiveLog,
) -> tokio::task::JoinHandle<()> {
    let pool = state.pool.clone();
    let log = log.clone();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(FLUSH_EVERY);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let Some(text) = log.take_dirty() else {
                continue;
            };
            let written = sqlx::query!(
                r#"UPDATE deployments SET logs = $2 WHERE id = $1 AND status = 'running'"#,
                deployment_id,
                text
            )
            .execute(&pool)
            .await;
            if let Err(error) = written {
                tracing::warn!(%error, "could not store the live stack log");
            }
        }
    })
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
            build: service.build,
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

pub(crate) async fn load_env(
    state: &AppState,
    id: Uuid,
) -> Result<Vec<(String, String)>, ApiError> {
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
    let inserted = sqlx::query_scalar!(
        r#"
        INSERT INTO deployments (service_id, status, trigger, logs, started_at)
        VALUES ($1, 'running', $2, '', now())
        RETURNING id
        "#,
        id,
        format!("{trigger}:{}", action.as_str())
    )
    .fetch_one(&state.pool)
    .await;
    let deployment_id = match inserted {
        Ok(deployment_id) => deployment_id,
        Err(error) => {
            // Do not leave the stack locked in `deploying`.
            let _ = sqlx::query!(
                r#"UPDATE services SET status = $2, updated_at = now() WHERE id = $1"#,
                id,
                previous
            )
            .execute(&state.pool)
            .await;
            return Err(ApiError::database(error));
        }
    };
    let (cancel, guard) = state.stack_runs.begin(id);
    let task_state = state.clone();
    tokio::spawn(async move {
        execute(
            &task_state,
            id,
            deployment_id,
            action,
            &previous,
            cancel,
            guard,
        )
        .await;
    });
    load_deployment(state, deployment_id).await
}

/// A push reached the Git branch of a stack: deploy it when the stack asked for that.
/// `None` when push deploys are off. A stack that builds on deploy rebuilds, because
/// `build_on_deploy` turns the plain deploy into `up --build`.
pub async fn push_deploy(state: &AppState, id: Uuid) -> Result<Option<DeploymentView>, ApiError> {
    let stack = load(state, id).await?;
    if !stack.spec.deploy_on_push || stack.spec.source != "git" {
        return Ok(None);
    }
    start_action(state, id, StackAction::Up, "push")
        .await
        .map(Some)
}

/// Stop the running action of a stack. The action ends as `cancelled`.
pub async fn cancel(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    if state.stack_runs.cancel(id) {
        Ok(())
    } else {
        Err(conflict("not_running", "this stack has no action running"))
    }
}

/// How an action ended.
enum Ended {
    Finished,
    Failed(String),
    Cancelled,
    TimedOut(u64),
}

async fn execute(
    state: &AppState,
    id: Uuid,
    deployment_id: Uuid,
    action: StackAction,
    previous: &str,
    cancel: CancellationToken,
    guard: RunGuard,
) {
    let log = LiveLog::default();
    let flusher = spawn_flusher(state, deployment_id, &log);
    let limit = state.config.stack_timeout_secs;
    let ended = {
        let work = run(state, id, action, &log);
        tokio::pin!(work);
        let timeout = async {
            if limit == 0 {
                std::future::pending::<()>().await;
            } else {
                tokio::time::sleep(Duration::from_secs(limit)).await;
            }
        };
        // Dropping `work` at the end of this block stops the docker compose process.
        tokio::select! {
            result = &mut work => match result {
                Ok(()) => Ended::Finished,
                Err(message) => Ended::Failed(message),
            },
            () = cancel.cancelled() => Ended::Cancelled,
            () = timeout => Ended::TimedOut(limit),
        }
    };
    flusher.abort();
    // The action is over: a Cancel from here on is answered "not running". The service stays
    // `deploying` until the final status below, so no new action can start in between.
    drop(guard);
    let (status, error, service_status) = match &ended {
        Ended::Finished => (
            "succeeded",
            None,
            match action {
                StackAction::Down => "stopped",
                StackAction::Pull => previous,
                _ => "running",
            },
        ),
        Ended::Failed(message) => (
            "failed",
            Some(message.clone()),
            if action.starts() { "failed" } else { previous },
        ),
        Ended::TimedOut(secs) => (
            "failed",
            Some(format!(
                "stopped after {} minutes; raise RUDDER_STACK_TIMEOUT_SECS for slower builds",
                secs.div_ceil(60)
            )),
            if action.starts() { "failed" } else { previous },
        ),
        Ended::Cancelled => (
            "cancelled",
            Some("cancelled".to_owned()),
            // The old containers keep running when the build never got that far.
            if !action.starts() {
                previous
            } else if previous == "running" {
                "running"
            } else {
                "stopped"
            },
        ),
    };
    if let Some(message) = &error {
        log.push(&format!("{message}\n"));
    }
    let logs = log.snapshot();
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
    let event = match &ended {
        Ended::Finished => crate::events::Event::info(
            &format!("stack.{}", action.as_str()),
            format!("Stack {} finished", action.as_str()),
        ),
        Ended::Cancelled => crate::events::Event::warning(
            "stack.cancelled",
            format!("Stack {} was cancelled", action.as_str()),
        ),
        Ended::Failed(_) | Ended::TimedOut(_) => crate::events::Event::warning(
            "stack.failed",
            format!("Stack {} failed", action.as_str()),
        )
        .message(error.clone().unwrap_or_default()),
    };
    crate::events::record(&state.pool, event.service(id)).await;
}

/// Registry logins for the images a stack pulls. Images from a registry without a
/// saved login are left to the node's own `docker login`.
async fn registry_logins(state: &AppState, spec: &StackSpec) -> Vec<RegistryAuth> {
    let mut logins: Vec<RegistryAuth> = Vec::new();
    for image in spec
        .services
        .iter()
        .filter_map(|service| service.image.as_deref())
    {
        match crate::deploy::registry_auth(state, image, None).await {
            Ok(Some(login)) if !logins.iter().any(|known| known.server == login.server) => {
                logins.push(login);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(image, message = %error.public_message(), "registry login skipped");
            }
        }
    }
    logins
}

/// The registries named by an authentication failure, to tell the user where to add a login.
fn registry_hint(message: &str, spec: &StackSpec) -> Option<String> {
    let lower = message.to_ascii_lowercase();
    let denied = [
        "unauthorized",
        "denied",
        "authentication required",
        "no basic auth credentials",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if !denied {
        return None;
    }
    let mut hosts: Vec<&str> = spec
        .services
        .iter()
        .filter_map(|service| service.image.as_deref())
        .map(rudder_core::image_registry)
        .collect();
    hosts.sort_unstable();
    hosts.dedup();
    Some(format!(
        "The registry refused the request. If an image is private, save a login for its registry (this stack uses {}) under Registries; stacks use those logins when they pull.",
        hosts.join(", ")
    ))
}

async fn run(state: &AppState, id: Uuid, action: StackAction, log: &LiveLog) -> Result<(), String> {
    let mut stack = load(state, id)
        .await
        .map_err(|error| error.public_message())?;
    let env = load_env(state, id)
        .await
        .map_err(|error| error.public_message())?;
    let mut secrets: Vec<String> = env.iter().map(|(_, value)| value.clone()).collect();
    log.set_secrets(secrets.clone());
    let driver = driver_for(state, stack.server_id)
        .await
        .map_err(|error| error.public_message())?;
    if action.writes_files() {
        write_files(state, &mut stack, driver.as_ref(), &env, log).await?;
    }
    let logins = if action.pulls() {
        registry_logins(state, &stack.spec).await
    } else {
        Vec::new()
    };
    secrets.extend(logins.iter().map(|login| login.password.clone()));
    log.set_secrets(secrets.clone());
    let compose = action.compose(stack.spec.build_on_deploy);
    log.push(&format!("$ docker compose {}\n", compose_label(compose)));
    let sink: LogSink = {
        let log = log.clone();
        Arc::new(move |text: &str| log.push(text))
    };
    let secret_refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let output = driver
        .compose_run(
            &stack.spec.project,
            &stack.spec.compose_path,
            compose,
            &ComposeOptions { logins },
            Some(&sink),
        )
        .await;
    match output {
        Ok(_) => Ok(()),
        Err(error) => {
            let mut message = redact(&error.to_string(), &secret_refs);
            if let Some(hint) = registry_hint(&message, &stack.spec) {
                message.push('\n');
                message.push_str(&hint);
            }
            Err(message)
        }
    }
}

fn compose_label(action: ComposeAction) -> &'static str {
    match action {
        ComposeAction::Up => "up -d",
        ComposeAction::UpBuild => "up -d --build",
        ComposeAction::UpPull => "up -d --pull always",
        ComposeAction::Down => "down",
        ComposeAction::Restart => "restart",
        ComposeAction::Pull => "pull",
        ComposeAction::Destroy => "down --volumes",
    }
}

/// Fetch the Git stack's repository on the panel host, read its compose file into the spec and
/// plan it again. Returns the directory, relative to the node data directory, the files go in.
async fn sync_compose(
    state: &AppState,
    stack: &mut StackRow,
    logs: Option<&LiveLog>,
) -> Result<String, String> {
    let project = stack.spec.project.clone();
    let git = stack
        .spec
        .git
        .clone()
        .ok_or_else(|| "git source is not configured".to_string())?;
    let checkout = format!("stacks/{project}/src");
    let dest = state.config.data_dir.join(&checkout);
    if let Some(logs) = logs {
        logs.push(&format!("syncing {} ({})\n", git.repo, git.branch));
    }
    let output = crate::build_queue::sync_repo(
        state,
        stack.id,
        git.provider_id,
        &git.repo,
        &git.branch,
        &dest,
    )
    .await?;
    if let Some(logs) = logs {
        logs.push(&output);
    }
    let file = dest.join(&git.compose_file);
    let text = std::fs::read_to_string(&file)
        .map_err(|_| format!("{} was not found in the repository", git.compose_file))?;
    check_compose_text(&text).map_err(|error| error.public_message())?;
    stack.spec.compose_yaml = text;
    let routed = routed_services(state, stack.id, &stack.spec)
        .await
        .map_err(|error| error.public_message())?;
    replan(&mut stack.spec, &routed).map_err(|error| error.public_message())?;
    Ok(
        match Path::new(&git.compose_file).parent().and_then(Path::to_str) {
            Some(parent) if !parent.is_empty() => format!("{checkout}/{parent}"),
            _ => checkout,
        },
    )
}

/// Read the compose file from Git now, so the variables it needs, the generated file and its
/// warnings are known before the first deploy. Nothing is written to the server.
pub async fn refresh_from_git(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let mut stack = load(state, id).await?;
    if stack.status == "deploying" {
        return Err(conflict(
            "deploy_in_progress",
            "wait for the running stack action",
        ));
    }
    if stack.spec.source != "git" {
        return Err(bad(
            "invalid",
            "only a stack with a Git source can be refreshed",
        ));
    }
    if stack.spec.project.is_empty() {
        let name = crate::routes::catalog::load_service_view(state, id)
            .await?
            .name;
        stack.spec.project = project_name(&name);
    }
    validate_source(&stack.spec)?;
    sync_compose(state, &mut stack, None)
        .await
        .map_err(|message| bad("git_refresh_failed", message))?;
    save(state, id, &stack.spec).await
}

async fn write_files(
    state: &AppState,
    stack: &mut StackRow,
    driver: &dyn NodeDriver,
    env: &[(String, String)],
    logs: &LiveLog,
) -> Result<(), String> {
    let project = stack.spec.project.clone();
    let dir = if stack.spec.source == "git" {
        sync_compose(state, stack, Some(logs)).await?
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
        logs.push(&format!("warning: {warning}\n"));
    }
    logs.push(&format!(
        "wrote {} and .env ({} variables)\n",
        stack.spec.compose_path,
        env.len()
    ));
    Ok(())
}

// Logs of one compose service, with environment values masked.
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
