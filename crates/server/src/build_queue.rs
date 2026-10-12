//! Per-server build queue.
//!
//! A job is claimed only when that server is under the panel concurrency limit.
//! Git, Docker, and Nixpacks are started with argument vectors. Tokens stay in
//! a mode `0600` credential file or an environment value and are stripped from
//! stored logs.

use std::path::{Path, PathBuf};
use std::time::Duration;

use rudder_core::{
    can_start, docker_build_argv, git_clone_argv, github_app_jwt, nixpacks_argv, redact,
    static_dockerfile, validate_env_key, BuildPlan, BuildStrategy,
};
use serde_json::Value;
use tokio::process::Command;
use uuid::Uuid;

use crate::deploy::{roll_built_image, BuiltRollout};
use crate::error::ApiError;
use crate::routes::catalog::{bad, conflict, not_found};
use crate::state::AppState;

pub struct BuildTicket {
    pub deployment_id: Uuid,
    pub queue_id: Uuid,
}

struct GitSource {
    provider_id: Uuid,
    repo: String,
    branch: String,
    strategy: BuildStrategy,
    dockerfile: String,
    context: String,
    publish_dir: String,
}

struct ProviderSecret {
    kind: String,
    token: Option<String>,
    app_id: Option<String>,
    installation_id: Option<String>,
    private_key_pem: Option<String>,
}

/// Claim and run at most one queued build.
pub async fn build_once(state: &AppState) -> Result<bool, ApiError> {
    let Some((queue_id, deployment_id)) = claim(state).await? else {
        return Ok(false);
    };
    run_claimed(state, queue_id, deployment_id).await?;
    Ok(true)
}

pub async fn enqueue(
    state: &AppState,
    service_id: Uuid,
    trigger: &str,
    git_sha: Option<String>,
) -> Result<BuildTicket, ApiError> {
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    let service = sqlx::query!(
        r#"
        SELECT id, server_id, kind, status, spec
        FROM services
        WHERE id = $1
        FOR UPDATE
        "#,
        service_id
    )
    .fetch_optional(&mut *tx)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))?;
    if service.kind != "application" {
        return Err(bad(
            "invalid_kind",
            "git builds are available for application services",
        ));
    }
    if service.status == "deploying" || service.status == "building" {
        return Err(conflict(
            "deploy_in_progress",
            "a deployment is already running for this service",
        ));
    }
    if service.spec.get("git").is_none() {
        return Err(bad("missing_source", "set a git source before building"));
    }
    let deployment_id = sqlx::query_scalar!(
        r#"
        INSERT INTO deployments (service_id, status, trigger, git_sha, logs)
        VALUES ($1, 'queued', $2, $3, '')
        RETURNING id
        "#,
        service.id,
        trigger,
        git_sha
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(ApiError::database)?;
    let queue_id = sqlx::query_scalar!(
        r#"
        INSERT INTO build_queue (deployment_id, server_id, status)
        VALUES ($1, $2, 'queued')
        RETURNING id
        "#,
        deployment_id,
        service.server_id
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(ApiError::database)?;
    tx.commit().await.map_err(ApiError::database)?;
    Ok(BuildTicket {
        deployment_id,
        queue_id,
    })
}

pub async fn cancel(state: &AppState, queue_id: Uuid) -> Result<(), ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT status, deployment_id, cancel_requested
        FROM build_queue
        WHERE id = $1
        "#,
        queue_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("build not found"))?;
    if row.status == "queued" {
        sqlx::query!(
            r#"
            UPDATE build_queue
            SET status = 'cancelled', cancel_requested = TRUE, finished_at = now()
            WHERE id = $1
            "#,
            queue_id
        )
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
        sqlx::query!(
            r#"
            UPDATE deployments
            SET status = 'cancelled', error = 'build cancelled', finished_at = now()
            WHERE id = $1 AND status = 'queued'
            "#,
            row.deployment_id
        )
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
        return Ok(());
    }
    if row.status == "running" {
        sqlx::query!(
            r#"UPDATE build_queue SET cancel_requested = TRUE WHERE id = $1"#,
            queue_id
        )
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
        return Ok(());
    }
    Err(bad("not_running", "that build is already finished"))
}

async fn claim(state: &AppState) -> Result<Option<(Uuid, Uuid)>, ApiError> {
    let limit = sqlx::query_scalar!(
        r#"
        SELECT build_concurrency_default AS "limit!"
        FROM panel_settings
        WHERE id = TRUE
        "#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let servers = sqlx::query_scalar!(
        r#"SELECT DISTINCT server_id FROM build_queue WHERE status = 'queued'"#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    for server_id in servers {
        let running = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) AS "count!"
            FROM build_queue
            WHERE server_id = $1 AND status = 'running'
            "#,
            server_id
        )
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::database)?;
        if !can_start(running, i64::from(limit)) {
            continue;
        }
        let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
        let row = sqlx::query!(
            r#"
            SELECT id, deployment_id, cancel_requested
            FROM build_queue
            WHERE server_id = $1 AND status = 'queued'
            ORDER BY created_at
            FOR UPDATE SKIP LOCKED
            LIMIT 1
            "#,
            server_id
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(ApiError::database)?;
        let Some(row) = row else {
            tx.commit().await.map_err(ApiError::database)?;
            continue;
        };
        if row.cancel_requested {
            sqlx::query!(
                r#"
                UPDATE build_queue
                SET status = 'cancelled', finished_at = now()
                WHERE id = $1
                "#,
                row.id
            )
            .execute(&mut *tx)
            .await
            .map_err(ApiError::database)?;
            sqlx::query!(
                r#"
                UPDATE deployments
                SET status = 'cancelled', error = 'build cancelled', finished_at = now()
                WHERE id = $1
                "#,
                row.deployment_id
            )
            .execute(&mut *tx)
            .await
            .map_err(ApiError::database)?;
            tx.commit().await.map_err(ApiError::database)?;
            continue;
        }
        sqlx::query!(
            r#"
            UPDATE build_queue
            SET status = 'running', started_at = now()
            WHERE id = $1
            "#,
            row.id
        )
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database)?;
        sqlx::query!(
            r#"
            UPDATE deployments
            SET status = 'running', started_at = COALESCE(started_at, now())
            WHERE id = $1
            "#,
            row.deployment_id
        )
        .execute(&mut *tx)
        .await
        .map_err(ApiError::database)?;
        tx.commit().await.map_err(ApiError::database)?;
        return Ok(Some((row.id, row.deployment_id)));
    }
    Ok(None)
}

async fn run_claimed(
    state: &AppState,
    queue_id: Uuid,
    deployment_id: Uuid,
) -> Result<(), ApiError> {
    let job = sqlx::query!(
        r#"
        SELECT s.id AS service_id, s.name, s.spec, s.server_id, s.current_container_id,
               sv.kind AS server_kind
        FROM deployments d
        JOIN services s ON s.id = d.service_id
        JOIN servers sv ON sv.id = s.server_id
        WHERE d.id = $1
        "#,
        deployment_id
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    sqlx::query!(
        r#"UPDATE services SET status = 'building', updated_at = now() WHERE id = $1"#,
        job.service_id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if job.server_kind != "local" && state.agents.session(job.server_id).is_none() {
        fail_job(
            state,
            queue_id,
            deployment_id,
            job.service_id,
            job.current_container_id.is_some(),
            "the agent for this server is not connected",
        )
        .await?;
        return Ok(());
    }
    let source = match parse_source(&job.spec) {
        Ok(source) => source,
        Err(message) => {
            fail_job(
                state,
                queue_id,
                deployment_id,
                job.service_id,
                job.current_container_id.is_some(),
                &message,
            )
            .await?;
            return Ok(());
        }
    };
    let mut logs = String::from("build queued\n");
    if let Err(message) = execute_local(
        state,
        queue_id,
        deployment_id,
        job.service_id,
        &job.name,
        &source,
        &mut logs,
    )
    .await
    {
        let _ = write_logs(state, deployment_id, &logs).await;
        if message == "build cancelled" {
            cancel_running(
                state,
                queue_id,
                deployment_id,
                job.service_id,
                job.current_container_id.is_some(),
                &logs,
            )
            .await?;
        } else {
            fail_job(
                state,
                queue_id,
                deployment_id,
                job.service_id,
                job.current_container_id.is_some(),
                &message,
            )
            .await?;
        }
    }
    Ok(())
}

async fn execute_local(
    state: &AppState,
    queue_id: Uuid,
    deployment_id: Uuid,
    service_id: Uuid,
    service_name: &str,
    source: &GitSource,
    logs: &mut String,
) -> Result<(), String> {
    if cancelled(state, queue_id).await {
        return Err("build cancelled".into());
    }
    let provider = load_provider(state, source.provider_id).await?;
    let build_args = load_build_args(state, service_id).await?;
    let workspace = state
        .config
        .data_dir
        .join("builds")
        .join(deployment_id.to_string());
    std::fs::create_dir_all(&workspace)
        .map_err(|_| "could not create the build directory".to_string())?;
    let _cleanup = CleanDir(workspace.clone());
    let repo_dir = workspace.join("src");
    let dest = repo_dir
        .to_str()
        .filter(|path| path_is_safe(path))
        .ok_or_else(|| "data directory path cannot be used for a build".to_string())?;
    let argv =
        git_clone_argv(&source.branch, &source.repo, dest).map_err(|error| error.to_string())?;
    logs.push_str("cloning\n");
    write_logs(state, deployment_id, logs).await.ok();
    let mut secrets = Vec::new();
    let mut env = Vec::new();
    prepare_auth(
        state,
        service_id,
        &provider,
        &source.repo,
        &workspace,
        &mut env,
        &mut secrets,
    )
    .await?;
    let secret_refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let clone_log = run_cancellable(
        state,
        queue_id,
        &argv,
        &workspace,
        &env,
        &secret_refs,
        Duration::from_secs(120),
    )
    .await?;
    logs.push_str(&clone_log);
    if cancelled(state, queue_id).await {
        return Err("build cancelled".into());
    }
    let tag = deployment_id.simple().to_string();
    let image = format!("rudder-{service_name}:{}", &tag[..12]);
    let plan = BuildPlan {
        strategy: source.strategy,
        context: source.context.clone(),
        dockerfile: source.dockerfile.clone(),
        publish_dir: source.publish_dir.clone(),
        image: image.clone(),
        build_args,
    };
    if source.strategy == BuildStrategy::Static {
        let dockerfile =
            static_dockerfile(&source.publish_dir).map_err(|error| error.to_string())?;
        let dockerfile_dir = if source.context == "." {
            repo_dir.clone()
        } else {
            repo_dir.join(&source.context)
        };
        std::fs::create_dir_all(&dockerfile_dir)
            .map_err(|_| "could not write the static dockerfile")?;
        let path = dockerfile_dir.join("Dockerfile.rudder");
        std::fs::write(&path, dockerfile).map_err(|_| "could not write the static dockerfile")?;
    }
    let build_argv = match source.strategy {
        BuildStrategy::Nixpacks => nixpacks_argv(&plan).map_err(|error| error.to_string())?,
        BuildStrategy::Dockerfile | BuildStrategy::Static => {
            docker_build_argv(&plan).map_err(|error| error.to_string())?
        }
    };
    logs.push_str("building\n");
    write_logs(state, deployment_id, logs).await.ok();
    let build_log = run_cancellable(
        state,
        queue_id,
        &build_argv,
        &repo_dir,
        &[],
        &secret_refs,
        Duration::from_secs(600),
    )
    .await?;
    logs.push_str(&build_log);
    if cancelled(state, queue_id).await {
        return Err("build cancelled".into());
    }
    logs.push_str(&format!("image {image}\n"));
    roll_built_image(
        state,
        BuiltRollout {
            service_id,
            deployment_id,
            image,
            actor: None,
            log_prefix: logs.clone(),
        },
    )
    .await
    .map_err(|error| error.public_message())?;
    sqlx::query!(
        r#"
        UPDATE build_queue
        SET status = 'succeeded', finished_at = now()
        WHERE id = $1
        "#,
        queue_id
    )
    .execute(&state.pool)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

/// Clone `repo` into `dest`, or fast-forward an existing checkout with
/// `fetch` + `reset --hard` so untracked files (bind-mounted data) survive.
/// Credentials live in a temporary directory that is removed afterwards.
pub(crate) async fn sync_repo(
    state: &AppState,
    service_id: Uuid,
    provider_id: Uuid,
    repo: &str,
    branch: &str,
    dest: &Path,
) -> Result<String, String> {
    let dest_text = dest
        .to_str()
        .filter(|path| path_is_safe(path))
        .ok_or_else(|| "data directory path cannot be used for a checkout".to_string())?;
    let clone = git_clone_argv(branch, repo, dest_text).map_err(|error| error.to_string())?;
    let provider = load_provider(state, provider_id).await?;
    let scratch = state
        .config
        .data_dir
        .join("builds")
        .join(format!("sync-{}", Uuid::new_v4().simple()));
    std::fs::create_dir_all(&scratch)
        .map_err(|_| "could not create the checkout directory".to_string())?;
    let _cleanup = CleanDir(scratch.clone());
    let mut env = Vec::new();
    let mut secrets = Vec::new();
    prepare_auth(
        state,
        service_id,
        &provider,
        repo,
        &scratch,
        &mut env,
        &mut secrets,
    )
    .await?;
    let secret_refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let timeout = Duration::from_secs(120);
    if dest.join(".git").is_dir() {
        let fetch: Vec<String> = ["git", "fetch", "--depth", "1", repo, branch]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut log = run_command(&fetch, dest, &env, &secret_refs, timeout).await?;
        let reset: Vec<String> = ["git", "reset", "--hard", "FETCH_HEAD"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        log.push_str(&run_command(&reset, dest, &[], &secret_refs, timeout).await?);
        return Ok(log);
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|_| "could not create the checkout directory".to_string())?;
    }
    run_command(&clone, &scratch, &env, &secret_refs, timeout).await
}

fn parse_source(spec: &Value) -> Result<GitSource, String> {
    let git = spec
        .get("git")
        .ok_or_else(|| "git source is not configured".to_string())?;
    let provider_id = git
        .get("provider_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| "git provider is missing".to_string())?;
    let repo = required_string(git, "repo")?;
    let branch = required_string(git, "branch")?;
    let strategy = BuildStrategy::parse(git.get("strategy").and_then(Value::as_str).unwrap_or(""))
        .map_err(|error| error.to_string())?;
    Ok(GitSource {
        provider_id,
        repo,
        branch,
        strategy,
        dockerfile: git
            .get("dockerfile")
            .and_then(Value::as_str)
            .unwrap_or("Dockerfile")
            .to_owned(),
        context: git
            .get("context")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_owned(),
        publish_dir: git
            .get("publish_dir")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_owned(),
    })
}

fn required_string(value: &Value, key: &str) -> Result<String, String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("git {key} is missing"))
}

async fn load_provider(state: &AppState, id: Uuid) -> Result<ProviderSecret, String> {
    let row = sqlx::query!(
        r#"SELECT kind, config_enc FROM git_providers WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| "could not load the git provider".to_string())?
    .ok_or_else(|| "git provider not found".to_string())?;
    let bytes = state
        .cipher
        .decrypt(&row.config_enc)
        .map_err(|_| "stored git provider could not be decrypted".to_string())?;
    let config: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "stored git provider could not be decoded".to_string())?;
    Ok(ProviderSecret {
        kind: row.kind,
        token: config
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_owned),
        app_id: config
            .get("app_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        installation_id: config
            .get("installation_id")
            .and_then(Value::as_str)
            .map(str::to_owned),
        private_key_pem: config
            .get("private_key_pem")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

async fn load_build_args(
    state: &AppState,
    service_id: Uuid,
) -> Result<Vec<(String, String)>, String> {
    let rows = sqlx::query!(
        r#"
        SELECT key, value_enc
        FROM env_vars
        WHERE service_id = $1 AND is_build_secret = TRUE
        ORDER BY key
        "#,
        service_id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| "could not load build secrets".to_string())?;
    let mut args = Vec::new();
    for row in rows {
        validate_env_key(&row.key).map_err(|error| error.to_string())?;
        let bytes = state
            .cipher
            .decrypt(&row.value_enc)
            .map_err(|_| "stored build secret could not be decrypted".to_string())?;
        let value =
            String::from_utf8(bytes).map_err(|_| "stored build secret is not UTF-8".to_string())?;
        args.push((row.key, value));
    }
    Ok(args)
}

async fn prepare_auth(
    state: &AppState,
    service_id: Uuid,
    provider: &ProviderSecret,
    repo: &str,
    workspace: &Path,
    env: &mut Vec<(String, String)>,
    secrets: &mut Vec<String>,
) -> Result<(), String> {
    let ssh = repo.starts_with("git@") || repo.starts_with("ssh://");
    if ssh {
        let key = load_private_key(state, service_id).await?;
        secrets.push(key.clone());
        let path = workspace.join("deploy_key");
        write_secret(&path, key.as_bytes())?;
        let command = ssh_command(&path)?;
        env.push(("GIT_SSH_COMMAND".into(), command));
        return Ok(());
    }
    if !repo.starts_with("https://") {
        return Ok(());
    }
    let token = match provider.kind.as_str() {
        "github_pat" | "gitlab_pat" => provider
            .token
            .clone()
            .filter(|token| !token.is_empty())
            .ok_or_else(|| "git provider token is missing".to_string())?,
        "github_app" => {
            installation_token(
                &state.config.github_api_url,
                provider.app_id.as_deref().unwrap_or(""),
                provider.installation_id.as_deref().unwrap_or(""),
                provider.private_key_pem.as_deref().unwrap_or(""),
            )
            .await?
        }
        _ => return Ok(()),
    };
    secrets.push(token.clone());
    let user = if provider.kind == "gitlab_pat" {
        "oauth2"
    } else {
        "x-access-token"
    };
    let host = repo
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .filter(|host| !host.is_empty())
        .ok_or_else(|| "git url host is missing".to_string())?;
    let line = format!("https://{user}:{}@{host}\n", url_encode(&token));
    secrets.push(url_encode(&token));
    let creds = workspace.join("git-credentials");
    write_secret(&creds, line.as_bytes())?;
    let cred_path = creds
        .to_str()
        .filter(|path| path_is_safe(path))
        .ok_or_else(|| "data directory path cannot be used for git credentials".to_string())?;
    env.push(("GIT_CONFIG_COUNT".into(), "1".into()));
    env.push(("GIT_CONFIG_KEY_0".into(), "credential.helper".into()));
    env.push((
        "GIT_CONFIG_VALUE_0".into(),
        format!("store --file={cred_path}"),
    ));
    Ok(())
}

async fn load_private_key(state: &AppState, service_id: Uuid) -> Result<String, String> {
    let blob = sqlx::query_scalar!(
        r#"
        SELECT private_key_enc
        FROM deploy_keys
        WHERE service_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
        service_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| "could not load the deploy key".to_string())?
    .ok_or_else(|| "this service has no deploy key".to_string())?;
    let bytes = state
        .cipher
        .decrypt(&blob)
        .map_err(|_| "stored deploy key could not be decrypted".to_string())?;
    String::from_utf8(bytes).map_err(|_| "stored deploy key is not UTF-8".to_string())
}

async fn installation_token(
    api_url: &str,
    app_id: &str,
    installation_id: &str,
    private_key_pem: &str,
) -> Result<String, String> {
    if installation_id.is_empty() {
        return Err(
            "the GitHub App is not installed yet; finish installing it from Git providers".into(),
        );
    }
    if installation_id.len() > 20 || !installation_id.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("github installation id must be numeric".into());
    }
    let jwt = github_app_jwt(app_id, private_key_pem, chrono::Utc::now().timestamp())
        .map_err(|_| "github app key was rejected".to_string())?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "could not start the github client".to_string())?;
    let url = format!("{api_url}/app/installations/{installation_id}/access_tokens");
    let response = client
        .post(url)
        .header("authorization", format!("Bearer {jwt}"))
        .header("accept", "application/vnd.github+json")
        .header("user-agent", "rudder")
        .header("x-github-api-version", "2022-11-28")
        .send()
        .await
        .map_err(|_| "github app token request failed".to_string())?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!("github app token request returned {status}"));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|_| "github app token response was not json".to_string())?;
    let token = body
        .get("token")
        .and_then(Value::as_str)
        .ok_or_else(|| "github app token was missing".to_string())?;
    if token.is_empty()
        || token.len() > 512
        || token
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err("github app token was rejected".into());
    }
    Ok(token.to_owned())
}

fn write_secret(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| "could not write a secret file".to_string())?;
        file.write_all(bytes)
            .map_err(|_| "could not write a secret file".to_string())?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (path, bytes);
        Err("secret files require unix permissions".into())
    }
}

fn ssh_command(path: &Path) -> Result<String, String> {
    let path = path
        .to_str()
        .filter(|value| path_is_safe(value))
        .ok_or_else(|| "data directory path cannot be used for an SSH key".to_string())?;
    Ok(format!(
        "ssh -i {path} -o IdentitiesOnly=yes -o StrictHostKeyChecking=accept-new"
    ))
}

fn path_is_safe(path: &str) -> bool {
    !path.is_empty()
        && path
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'.' | b'_' | b'-'))
}

fn url_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

async fn cancelled(state: &AppState, queue_id: Uuid) -> bool {
    sqlx::query_scalar!(
        r#"SELECT cancel_requested AS "flag!" FROM build_queue WHERE id = $1"#,
        queue_id
    )
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

async fn run_cancellable(
    state: &AppState,
    queue_id: Uuid,
    argv: &[String],
    dir: &Path,
    env: &[(String, String)],
    secrets: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    tokio::select! {
        biased;
        result = run_command(argv, dir, env, secrets, timeout) => result,
        () = wait_until_cancelled(state, queue_id) => Err("build cancelled".into()),
    }
}

async fn wait_until_cancelled(state: &AppState, queue_id: Uuid) {
    loop {
        if cancelled(state, queue_id).await {
            return;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn run_command(
    argv: &[String],
    dir: &Path,
    env: &[(String, String)],
    secrets: &[&str],
    timeout: Duration,
) -> Result<String, String> {
    if argv.is_empty() {
        return Err("empty command".into());
    }
    let program = argv[0].clone();
    let mut command = Command::new(&program);
    command
        .args(&argv[1..])
        .current_dir(dir)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let child = command
        .spawn()
        .map_err(|_| format!("{program} is not available"))?;
    let collected = async move {
        let output = child
            .wait_with_output()
            .await
            .map_err(|error| error.to_string())?;
        let mut combined = output.stdout;
        combined.extend_from_slice(&output.stderr);
        let text = redact(&String::from_utf8_lossy(&combined), secrets);
        if output.status.success() {
            Ok(text)
        } else {
            Err(format!(
                "exit {}\n{text}",
                output.status.code().unwrap_or(-1)
            ))
        }
    };
    match tokio::time::timeout(timeout, collected).await {
        Ok(result) => result,
        Err(_) => Err(format!("{program} timed out")),
    }
}

async fn write_logs(state: &AppState, deployment_id: Uuid, logs: &str) -> Result<(), ApiError> {
    sqlx::query!(
        r#"UPDATE deployments SET logs = $2 WHERE id = $1"#,
        deployment_id,
        logs
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(())
}

async fn fail_job(
    state: &AppState,
    queue_id: Uuid,
    deployment_id: Uuid,
    service_id: Uuid,
    had_container: bool,
    message: &str,
) -> Result<(), ApiError> {
    let safe = redact(message, &[]);
    let existing = sqlx::query_scalar!(
        r#"SELECT logs FROM deployments WHERE id = $1"#,
        deployment_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .unwrap_or_default();
    let logs = if existing.contains(&safe) {
        existing
    } else {
        format!("{existing}{safe}\n")
    };
    sqlx::query!(
        r#"
        UPDATE deployments
        SET status = 'failed', error = $2, logs = $3, finished_at = now()
        WHERE id = $1
        "#,
        deployment_id,
        safe,
        logs
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    crate::events::record(
        &state.pool,
        crate::events::Event::warning("build.failed", "Build failed")
            .service(service_id)
            .message(safe.clone())
            .payload(serde_json::json!({ "deployment_id": deployment_id })),
    )
    .await;
    sqlx::query!(
        r#"
        UPDATE build_queue
        SET status = 'failed', finished_at = now()
        WHERE id = $1 AND status <> 'cancelled'
        "#,
        queue_id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let service_status = if had_container { "running" } else { "failed" };
    sqlx::query!(
        r#"
        UPDATE services
        SET status = $2, updated_at = now()
        WHERE id = $1 AND status IN ('building', 'deploying')
        "#,
        service_id,
        service_status
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(())
}

async fn cancel_running(
    state: &AppState,
    queue_id: Uuid,
    deployment_id: Uuid,
    service_id: Uuid,
    had_container: bool,
    logs: &str,
) -> Result<(), ApiError> {
    sqlx::query!(
        r#"
        UPDATE deployments
        SET status = 'cancelled', error = 'build cancelled', logs = $2, finished_at = now()
        WHERE id = $1
        "#,
        deployment_id,
        logs
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    sqlx::query!(
        r#"
        UPDATE build_queue
        SET status = 'cancelled', cancel_requested = TRUE, finished_at = now()
        WHERE id = $1
        "#,
        queue_id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let service_status = if had_container {
        "running"
    } else {
        "cancelled"
    };
    sqlx::query!(
        r#"
        UPDATE services
        SET status = $2, updated_at = now()
        WHERE id = $1 AND status IN ('building', 'deploying')
        "#,
        service_id,
        service_status
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(())
}

struct CleanDir(PathBuf);

impl Drop for CleanDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
