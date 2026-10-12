//! Git providers, service source, and push webhooks.
//!
//! Provider tokens and the webhook secret are encrypted. The webhook route
//! reads the raw body so the GitHub HMAC covers the bytes GitHub signed.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::get;
use axum::Json;
use chrono::{DateTime, Utc};
use rudder_core::{
    evaluate_hook, validate_branch, validate_git_url, validate_name, validate_rel_path,
    BuildStrategy, Cipher, HookInput, HookKind, HookOutcome,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::build_queue::{self, BuildTicket};
use crate::error::ApiError;
use crate::gitkey::generate_deploy_key;
use crate::routes::catalog::{bad, conflict, core_error, is_unique, not_found};
use crate::state::AppState;

#[derive(Serialize, ToSchema)]
pub struct GitProviderView {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
    pub created_at: DateTime<Utc>,
    /// `ready`, or `pending_install` for a GitHub App that is not installed yet.
    pub status: String,
    /// Slug of a GitHub App created from the panel.
    pub app_slug: Option<String>,
    /// GitHub account that installed the app.
    pub account: Option<String>,
}

/// The view of a stored provider. Only GitHub Apps need their config read.
pub(crate) fn provider_view(
    cipher: &Cipher,
    id: Uuid,
    name: String,
    kind: String,
    created_at: DateTime<Utc>,
    config_enc: &[u8],
) -> GitProviderView {
    let config = (kind == "github_app")
        .then(|| cipher.decrypt(config_enc).ok())
        .flatten()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    let field = |key: &str| {
        config
            .as_ref()
            .and_then(|config| config.get(key))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let pending = config.is_some() && field("installation_id").is_none();
    GitProviderView {
        id,
        name,
        kind,
        created_at,
        status: if pending { "pending_install" } else { "ready" }.into(),
        app_slug: field("slug"),
        account: field("account"),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct CreateGitProvider {
    pub name: String,
    pub kind: String,
    pub token: Option<String>,
    pub app_id: Option<String>,
    pub installation_id: Option<String>,
    pub private_key_pem: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct SourceView {
    pub provider_id: Uuid,
    pub repo: String,
    pub branch: String,
    pub strategy: String,
    pub dockerfile: String,
    pub context: String,
    pub publish_dir: String,
    pub has_webhook_secret: bool,
    pub public_key: Option<String>,
    pub webhook_secret: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateSource {
    pub provider_id: Uuid,
    pub repo: String,
    pub branch: String,
    pub strategy: String,
    pub dockerfile: Option<String>,
    pub context: Option<String>,
    pub publish_dir: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct BuildAccepted {
    pub deployment_id: Uuid,
    pub queue_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct BuildQueueView {
    pub id: Uuid,
    pub deployment_id: Uuid,
    pub status: String,
    pub cancel_requested: bool,
    pub created_at: DateTime<Utc>,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/git-providers",
            get(list_providers).post(create_provider),
        )
        .route(
            "/api/v1/git-providers/{id}",
            axum::routing::delete(delete_provider),
        )
        .route(
            "/api/v1/services/{id}/source",
            get(get_source).put(put_source),
        )
        .route(
            "/api/v1/services/{id}/build",
            axum::routing::post(build_now),
        )
        .route("/api/v1/services/{id}/builds", get(list_builds))
        .route(
            "/api/v1/build-queue/{id}/cancel",
            axum::routing::post(cancel_build),
        )
        .route(
            "/api/v1/services/{id}/webhook",
            axum::routing::post(webhook),
        )
}

async fn list_providers(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<GitProviderView>>, ApiError> {
    let rows = sqlx::query!(
        r#"SELECT id, name, kind, config_enc, created_at FROM git_providers ORDER BY name"#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| {
                provider_view(
                    &state.cipher,
                    row.id,
                    row.name,
                    row.kind,
                    row.created_at,
                    &row.config_enc,
                )
            })
            .collect(),
    ))
}

async fn create_provider(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CreateGitProvider>,
) -> Result<(StatusCode, Json<GitProviderView>), ApiError> {
    auth.require_write()?;
    validate_name(&body.name).map_err(core_error)?;
    let config = provider_config(&body)?;
    let encoded = serde_json::to_vec(&config).map_err(|_| bad("invalid", "provider config"))?;
    let encrypted = state.cipher.encrypt(&encoded).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "could not encrypt the git provider",
        )
    })?;
    let row = sqlx::query!(
        r#"
        INSERT INTO git_providers (name, kind, config_enc)
        VALUES ($1, $2, $3)
        RETURNING id, name, kind, created_at
        "#,
        body.name,
        body.kind,
        encrypted
    )
    .fetch_one(&state.pool)
    .await
    .map_err(|error| {
        if is_unique(&error) {
            conflict("duplicate", "a git provider with that name already exists")
        } else {
            ApiError::database(error)
        }
    })?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "git_provider.create",
        "git_provider",
        Some(&row.id.to_string()),
        json!({ "name": row.name, "kind": row.kind }),
        None,
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(provider_view(
            &state.cipher,
            row.id,
            row.name,
            row.kind,
            row.created_at,
            &encrypted,
        )),
    ))
}

async fn delete_provider(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let deleted = sqlx::query!(r#"DELETE FROM git_providers WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if deleted.rows_affected() == 0 {
        return Err(not_found("git provider not found"));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "git_provider.delete",
        "git_provider",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

fn provider_config(body: &CreateGitProvider) -> Result<Value, ApiError> {
    match body.kind.as_str() {
        "github_pat" | "gitlab_pat" => {
            let token = required_secret(body.token.as_deref(), "token")?;
            Ok(json!({ "token": token }))
        }
        "github_app" => {
            let app_id = body.app_id.as_deref().unwrap_or("");
            let installation_id = body.installation_id.as_deref().unwrap_or("");
            if !numeric_id(app_id) || !numeric_id(installation_id) {
                return Err(bad(
                    "invalid",
                    "github app id and installation id must be numeric",
                ));
            }
            let private_key_pem = body.private_key_pem.as_deref().unwrap_or("");
            if private_key_pem.len() < 32 || private_key_pem.len() > 16_384 {
                return Err(bad("invalid", "github app private key is missing"));
            }
            Ok(json!({
                "app_id": app_id,
                "installation_id": installation_id,
                "private_key_pem": private_key_pem,
            }))
        }
        "generic_ssh" => Ok(json!({})),
        _ => Err(bad(
            "invalid_kind",
            "kind must be github_pat, gitlab_pat, github_app, or generic_ssh",
        )),
    }
}

fn required_secret(value: Option<&str>, label: &str) -> Result<String, ApiError> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(bad("invalid", format!("{label} is required")));
    };
    if value.len() > 512 || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(bad("invalid", format!("{label} is invalid")));
    }
    Ok(value.to_owned())
}

fn numeric_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 20 && value.bytes().all(|byte| byte.is_ascii_digit())
}

async fn get_source(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<SourceView>, ApiError> {
    let row = load_service(&state, id).await?;
    let mut view = source_view(&row.spec, row.webhook_secret_enc.is_some())?;
    view.public_key = deploy_public_key(&state, id).await?;
    Ok(Json(view))
}

async fn put_source(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateSource>,
) -> Result<Json<SourceView>, ApiError> {
    auth.require_write()?;
    validate_git_url(&body.repo).map_err(core_error)?;
    validate_branch(&body.branch).map_err(core_error)?;
    let strategy = BuildStrategy::parse(&body.strategy).map_err(core_error)?;
    let dockerfile = body.dockerfile.unwrap_or_else(|| "Dockerfile".into());
    let context = body.context.unwrap_or_else(|| ".".into());
    let publish_dir = body.publish_dir.unwrap_or_else(|| ".".into());
    validate_rel_path(&dockerfile).map_err(core_error)?;
    validate_rel_path(&context).map_err(core_error)?;
    validate_rel_path(&publish_dir).map_err(core_error)?;
    let ssh = body.repo.starts_with("git@") || body.repo.starts_with("ssh://");
    let provider = sqlx::query!(
        r#"SELECT id, name, kind, config_enc, created_at FROM git_providers WHERE id = $1"#,
        body.provider_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("git provider not found"))?;
    let provider = provider_view(
        &state.cipher,
        provider.id,
        provider.name,
        provider.kind,
        provider.created_at,
        &provider.config_enc,
    );
    if provider.status == "pending_install" {
        return Err(bad(
            "invalid",
            "install the GitHub App on GitHub before using it as a source",
        ));
    }
    let provider_kind = provider.kind;
    if provider_kind == "generic_ssh" && !ssh {
        return Err(bad(
            "invalid",
            "an SSH provider requires a git@ or ssh:// url",
        ));
    }
    if ssh && provider_kind != "generic_ssh" && provider_kind != "github_app" {
        return Err(bad(
            "invalid",
            "ssh urls use an SSH provider or a GitHub App",
        ));
    }
    let mut row = load_service(&state, id).await?;
    if row.kind != "application" {
        return Err(bad(
            "invalid_kind",
            "git source is available for application services",
        ));
    }
    super::github_repos::check_source_access(&state, body.provider_id, &body.repo, &body.branch)
        .await?;
    let secret = hex::encode(rudder_core::random_bytes::<32>());
    let secret_enc = state.cipher.encrypt(secret.as_bytes()).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "could not encrypt the webhook secret",
        )
    })?;
    row.spec["git"] = json!({
        "provider_id": body.provider_id,
        "repo": body.repo,
        "branch": body.branch,
        "strategy": strategy.as_str(),
        "dockerfile": dockerfile,
        "context": context,
        "publish_dir": publish_dir,
    });
    sqlx::query!(
        r#"
        UPDATE services
        SET spec = $2, source_type = 'git', webhook_secret_enc = $3, updated_at = now()
        WHERE id = $1
        "#,
        id,
        row.spec,
        secret_enc
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let public_key = if ssh {
        Some(ensure_deploy_key(&state, id).await?)
    } else {
        deploy_public_key(&state, id).await?
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "service.source",
        "service",
        Some(&id.to_string()),
        json!({
            "provider_id": body.provider_id,
            "repo": body.repo,
            "branch": body.branch,
            "strategy": strategy.as_str(),
        }),
        None,
    )
    .await;
    let mut view = source_view(&row.spec, true)?;
    view.public_key = public_key;
    view.webhook_secret = Some(secret);
    Ok(Json(view))
}

async fn build_now(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<(StatusCode, Json<BuildAccepted>), ApiError> {
    auth.require_write()?;
    let ticket = build_queue::enqueue(&state, id, "manual", None).await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "service.build",
        "service",
        Some(&id.to_string()),
        json!({ "deployment_id": ticket.deployment_id, "trigger": "manual" }),
        None,
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(accepted(ticket))))
}

async fn list_builds(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<BuildQueueView>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT q.id, q.deployment_id, q.status, q.cancel_requested, q.created_at
        FROM build_queue q
        JOIN deployments d ON d.id = q.deployment_id
        WHERE d.service_id = $1
        ORDER BY q.created_at DESC
        LIMIT 50
        "#,
        id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| BuildQueueView {
                id: row.id,
                deployment_id: row.deployment_id,
                status: row.status,
                cancel_requested: row.cancel_requested,
                created_at: row.created_at,
            })
            .collect(),
    ))
}

async fn cancel_build(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    build_queue::cancel(&state, id).await?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "build.cancel",
        "build_queue",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

async fn webhook(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let row = sqlx::query!(
        r#"SELECT spec, webhook_secret_enc FROM services WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))?;
    let Some(blob) = row.webhook_secret_enc else {
        return Err(unauthorized());
    };
    let secret = state.cipher.decrypt(&blob).map_err(|_| unauthorized())?;
    let branch = row
        .spec
        .get("git")
        .and_then(|git| git.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let gitlab = headers.get("x-gitlab-token").is_some() || headers.get("x-gitlab-event").is_some();
    let event = header_str(
        &headers,
        if gitlab {
            "x-gitlab-event"
        } else {
            "x-github-event"
        },
    )
    .unwrap_or("");
    let outcome = evaluate_hook(&HookInput {
        kind: if gitlab {
            HookKind::Gitlab
        } else {
            HookKind::Github
        },
        event,
        body: &body,
        github_signature: header_str(&headers, "x-hub-signature-256"),
        gitlab_token: header_str(&headers, "x-gitlab-token"),
        secret: &secret,
        branch,
    });
    let provider = if gitlab { "gitlab" } else { "github" };
    match outcome {
        HookOutcome::Unauthorized => {
            record_delivery(&state, id, provider, &body, false, "invalid signature").await;
            Err(unauthorized())
        }
        HookOutcome::Rejected(detail) => {
            record_delivery(&state, id, provider, &body, false, detail).await;
            Err(bad("invalid", detail))
        }
        HookOutcome::Ignored(detail) => {
            record_delivery(&state, id, provider, &body, true, detail).await;
            Ok((
                StatusCode::OK,
                Json(json!({ "ok": true, "deploy": false, "detail": detail })),
            ))
        }
        HookOutcome::Deploy { git_sha } => {
            let ticket = build_queue::enqueue(&state, id, "webhook", git_sha).await?;
            record_delivery(&state, id, provider, &body, true, "queued").await;
            Ok((
                StatusCode::ACCEPTED,
                Json(json!({
                    "ok": true,
                    "deploy": true,
                    "deployment_id": ticket.deployment_id,
                    "queue_id": ticket.queue_id,
                })),
            ))
        }
    }
}

pub(crate) fn unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "invalid_signature",
        "invalid signature",
    )
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

pub(crate) async fn record_delivery(
    state: &AppState,
    service_id: Uuid,
    provider: &str,
    body: &[u8],
    accepted: bool,
    detail: &str,
) {
    let digest = hex::encode(Sha256::digest(body));
    let result = sqlx::query!(
        r#"
        INSERT INTO webhook_deliveries (service_id, provider, payload_sha, accepted, detail)
        VALUES ($1, $2, $3, $4, $5)
        "#,
        service_id,
        provider,
        digest,
        accepted,
        detail
    )
    .execute(&state.pool)
    .await;
    if let Err(error) = result {
        tracing::error!(%error, "failed to record webhook delivery");
    }
}

struct ServiceSourceRow {
    kind: String,
    spec: Value,
    webhook_secret_enc: Option<Vec<u8>>,
}

async fn load_service(state: &AppState, id: Uuid) -> Result<ServiceSourceRow, ApiError> {
    let row = sqlx::query!(
        r#"SELECT kind, spec, webhook_secret_enc FROM services WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))?;
    Ok(ServiceSourceRow {
        kind: row.kind,
        spec: row.spec,
        webhook_secret_enc: row.webhook_secret_enc,
    })
}

fn source_view(spec: &Value, has_webhook_secret: bool) -> Result<SourceView, ApiError> {
    let git = spec
        .get("git")
        .ok_or_else(|| not_found("git source is not configured"))?;
    let provider_id = git
        .get("provider_id")
        .and_then(Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or_else(|| bad("invalid", "git provider is missing"))?;
    Ok(SourceView {
        provider_id,
        repo: text_field(git, "repo"),
        branch: text_field(git, "branch"),
        strategy: text_field(git, "strategy"),
        dockerfile: text_field(git, "dockerfile"),
        context: text_field(git, "context"),
        publish_dir: text_field(git, "publish_dir"),
        has_webhook_secret,
        public_key: None,
        webhook_secret: None,
    })
}

fn text_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

async fn deploy_public_key(state: &AppState, service_id: Uuid) -> Result<Option<String>, ApiError> {
    let key = sqlx::query_scalar!(
        r#"
        SELECT public_key
        FROM deploy_keys
        WHERE service_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        "#,
        service_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(key)
}

async fn ensure_deploy_key(state: &AppState, service_id: Uuid) -> Result<String, ApiError> {
    if let Some(existing) = deploy_public_key(state, service_id).await? {
        return Ok(existing);
    }
    let material = generate_deploy_key()?;
    let encrypted = state
        .cipher
        .encrypt(material.private_openssh.as_bytes())
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "crypto_error",
                "could not encrypt the deploy key",
            )
        })?;
    sqlx::query!(
        r#"
        INSERT INTO deploy_keys (service_id, public_key, private_key_enc)
        VALUES ($1, $2, $3)
        "#,
        service_id,
        material.public_key,
        encrypted
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(material.public_key)
}

fn accepted(ticket: BuildTicket) -> BuildAccepted {
    BuildAccepted {
        deployment_id: ticket.deployment_id,
        queue_id: ticket.queue_id,
    }
}
