//! One-click GitHub App setup.
//!
//! GitHub's manifest flow: the panel gives the browser a manifest, GitHub shows
//! a confirmation page and creates the app, then sends the browser back with a
//! one-time code. The panel exchanges the code for the app's id, private key and
//! webhook secret. A second redirect follows the installation.
//!
//! Both redirects land on the web UI (`/git/github`), which calls the endpoints
//! below, so every step is an ordinary authenticated, CSRF-protected API call.
//! The `state` that comes back from GitHub is an AES-GCM sealed blob naming the
//! admin, the provider id, and what the step is for, so it cannot be forged,
//! replayed for another step, or used by someone else.
//!
//! The app's webhook delivers push events to `/api/v1/git-providers/{id}/webhook`,
//! which starts a build for every service that uses this app, repository and
//! branch.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::Utc;
use rudder_core::{
    evaluate_hook, github_app_jwt, github_app_manifest, github_repo_full_name, is_private_host,
    validate_app_name, validate_github_login, verify_github_signature, AppManifest, HookInput,
    HookKind, HookOutcome,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::build_queue;
use crate::error::ApiError;
use crate::routes::agents::panel_url;
use crate::routes::catalog::{bad, conflict, core_error, is_unique, not_found};
use crate::routes::git::{provider_view, record_delivery, unauthorized, GitProviderView};
use crate::state::AppState;

/// GitHub will not accept a manifest code or an install redirect after an hour.
const STATE_TTL_SECS: i64 = 3600;
const GITHUB_WEB: &str = "https://github.com";
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

#[derive(Deserialize, ToSchema)]
pub struct StartGithubApp {
    /// Create the app under this organization instead of your own account.
    pub organization: Option<String>,
    /// Name of the app. Defaults to `Rudder-` and four random characters.
    pub name: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct GithubAppManifest {
    /// Where the browser POSTs the manifest. It already carries the `state`.
    pub action_url: String,
    /// The manifest as JSON text, for a form field named `manifest`.
    pub manifest: String,
    /// False when the panel's address is private, so GitHub cannot deliver pushes.
    pub webhook: bool,
}

#[derive(Deserialize, ToSchema)]
pub struct CompleteGithubApp {
    pub code: String,
    pub state: String,
}

#[derive(Serialize, ToSchema)]
pub struct GithubAppCreated {
    pub provider: GitProviderView,
    /// Send the browser here to install the new app.
    pub install_url: String,
}

#[derive(Deserialize, ToSchema)]
pub struct GithubAppInstalled {
    pub installation_id: String,
    /// From the install link. Without it, the panel looks for the pending app the installation belongs to.
    pub state: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct InstallLink {
    pub install_url: String,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/github-app/manifest", post(start))
        .route("/api/v1/github-app/complete", post(complete))
        .route("/api/v1/github-app/installed", post(installed))
        .route(
            "/api/v1/git-providers/{id}/install-link",
            post(install_link),
        )
        .route("/api/v1/git-providers/{id}/webhook", post(webhook))
}

#[derive(Serialize, Deserialize)]
struct FlowState {
    purpose: String,
    user: Uuid,
    provider: Uuid,
    exp: i64,
}

fn seal_state(
    state: &AppState,
    purpose: &str,
    user: Uuid,
    provider: Uuid,
) -> Result<String, ApiError> {
    let flow = FlowState {
        purpose: purpose.to_owned(),
        user,
        provider,
        exp: Utc::now().timestamp() + STATE_TTL_SECS,
    };
    let bytes = serde_json::to_vec(&flow).map_err(|_| crypto_error())?;
    let blob = state.cipher.encrypt(&bytes).map_err(|_| crypto_error())?;
    Ok(URL_SAFE_NO_PAD.encode(blob))
}

fn open_state(
    state: &AppState,
    token: &str,
    purpose: &str,
    user: Uuid,
) -> Result<FlowState, ApiError> {
    let stale = || {
        bad(
            "invalid_state",
            "this GitHub link is invalid or has expired; start again",
        )
    };
    let blob = URL_SAFE_NO_PAD.decode(token.trim()).map_err(|_| stale())?;
    let bytes = state.cipher.decrypt(&blob).map_err(|_| stale())?;
    let flow: FlowState = serde_json::from_slice(&bytes).map_err(|_| stale())?;
    if flow.purpose != purpose || flow.user != user || flow.exp < Utc::now().timestamp() {
        return Err(stale());
    }
    Ok(flow)
}

fn crypto_error() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "crypto_error",
        "could not seal the GitHub link",
    )
}

/// The address GitHub should send the browser back to: the configured public
/// URL, else the origin the browser called from, else the Host header.
fn browser_base(state: &AppState, headers: &HeaderMap) -> String {
    if state.config.public_url.is_some() {
        return panel_url(state, headers);
    }
    headers
        .get("origin")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| url::Url::parse(value).ok())
        .filter(|origin| matches!(origin.scheme(), "http" | "https") && origin.host().is_some())
        .map_or_else(
            || panel_url(state, headers),
            |origin| origin.origin().ascii_serialization(),
        )
}

fn github_client() -> Result<reqwest::Client, ApiError> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(format!("rudder/{}", rudder_core::VERSION))
        .build()
        .map_err(|_| github_unreachable())
}

fn github_unreachable() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "github",
        "could not reach GitHub; check the panel's internet access",
    )
}

/// Body of a GitHub reply, limited in size.
async fn read_json(response: reqwest::Response) -> Result<Value, ApiError> {
    let bytes = response.bytes().await.map_err(|_| github_unreachable())?;
    if bytes.len() > MAX_RESPONSE_BYTES {
        return Err(github_unreachable());
    }
    serde_json::from_slice(&bytes).map_err(|_| github_unreachable())
}

async fn start(
    State(state): State<AppState>,
    auth: AuthUser,
    headers: HeaderMap,
    Json(body): Json<StartGithubApp>,
) -> Result<Json<GithubAppManifest>, ApiError> {
    auth.require_write()?;
    let organization = body
        .organization
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(login) = organization {
        validate_github_login(login).map_err(core_error)?;
    }
    let name = match body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(name) => {
            validate_app_name(name).map_err(core_error)?;
            name.to_owned()
        }
        None => format!("Rudder-{}", hex::encode(rudder_core::random_bytes::<2>())),
    };
    let provider = Uuid::new_v4();
    let base = browser_base(&state, &headers);
    let public = url::Url::parse(&base)
        .ok()
        .and_then(|url| url.host_str().map(|host| !is_private_host(host)))
        .unwrap_or(false);
    let webhook_url = format!("{base}/api/v1/git-providers/{provider}/webhook");
    let manifest = github_app_manifest(&AppManifest {
        name: &name,
        base_url: &base,
        webhook_url: public.then_some(webhook_url.as_str()),
    });
    let sealed = seal_state(&state, "manifest", auth.id, provider)?;
    let action_url = match organization {
        Some(login) => {
            format!("{GITHUB_WEB}/organizations/{login}/settings/apps/new?state={sealed}")
        }
        None => format!("{GITHUB_WEB}/settings/apps/new?state={sealed}"),
    };
    Ok(Json(GithubAppManifest {
        action_url,
        manifest: manifest.to_string(),
        webhook: public,
    }))
}

/// What GitHub returns when a manifest code is exchanged.
struct Converted {
    app_id: String,
    slug: String,
    owner: Option<String>,
    private_key_pem: String,
    webhook_secret: Option<String>,
}

fn converted(value: &Value) -> Option<Converted> {
    let slug = value.get("slug")?.as_str()?;
    let slug_ok = !slug.is_empty()
        && slug.len() <= 63
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if !slug_ok {
        return None;
    }
    Some(Converted {
        app_id: value.get("id")?.as_u64()?.to_string(),
        slug: slug.to_owned(),
        owner: value
            .pointer("/owner/login")
            .and_then(Value::as_str)
            .map(str::to_owned),
        private_key_pem: value.get("pem")?.as_str()?.to_owned(),
        webhook_secret: value
            .get("webhook_secret")
            .and_then(Value::as_str)
            .filter(|secret| !secret.is_empty())
            .map(str::to_owned),
    })
}

async fn convert_manifest(state: &AppState, code: &str) -> Result<Converted, ApiError> {
    let url = format!(
        "{}/app-manifests/{code}/conversions",
        state.config.github_api_url
    );
    let response = github_client()?
        .post(url)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .send()
        .await
        .map_err(|_| github_unreachable())?;
    let status = response.status();
    if status == StatusCode::NOT_FOUND || status == StatusCode::UNPROCESSABLE_ENTITY {
        return Err(bad(
            "github_rejected",
            "GitHub did not accept this code; it may have been used or expired. Start again.",
        ));
    }
    if !status.is_success() {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "github",
            format!("GitHub answered {status}"),
        ));
    }
    let value = read_json(response).await?;
    let converted = converted(&value).ok_or_else(github_unreachable)?;
    github_app_jwt(
        &converted.app_id,
        &converted.private_key_pem,
        Utc::now().timestamp(),
    )
    .map_err(|_| {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "github",
            "GitHub returned a private key the panel cannot use",
        )
    })?;
    Ok(converted)
}

fn install_url(slug: &str, sealed: &str) -> String {
    format!("{GITHUB_WEB}/apps/{slug}/installations/new?state={sealed}")
}

async fn complete(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<CompleteGithubApp>,
) -> Result<(StatusCode, Json<GithubAppCreated>), ApiError> {
    auth.require_write()?;
    let flow = open_state(&state, &body.state, "manifest", auth.id)?;
    let code = body.code.trim();
    if code.is_empty()
        || code.len() > 64
        || !code
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(bad("invalid", "the GitHub code is invalid"));
    }
    let exists = sqlx::query_scalar!(
        r#"SELECT id FROM git_providers WHERE id = $1"#,
        flow.provider
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if exists.is_some() {
        return Err(conflict(
            "already_connected",
            "this GitHub App is already connected",
        ));
    }
    let app = convert_manifest(&state, code).await?;
    let mut config = json!({
        "app_id": app.app_id,
        "private_key_pem": app.private_key_pem,
        "slug": app.slug,
        "installation_id": "",
    });
    if let Some(secret) = &app.webhook_secret {
        config["webhook_secret"] = json!(secret);
    }
    if let Some(owner) = &app.owner {
        config["owner"] = json!(owner);
    }
    let encoded = serde_json::to_vec(&config).map_err(|_| crypto_error())?;
    let encrypted = state.cipher.encrypt(&encoded).map_err(|_| crypto_error())?;
    let mut name = app.slug.clone();
    let row = loop {
        let inserted = sqlx::query!(
            r#"
            INSERT INTO git_providers (id, name, kind, config_enc)
            VALUES ($1, $2, 'github_app', $3)
            RETURNING id, name, kind, created_at
            "#,
            flow.provider,
            name,
            encrypted
        )
        .fetch_one(&state.pool)
        .await;
        match inserted {
            Ok(row) => break row,
            Err(error) if is_unique(&error) && name == app.slug => {
                name = format!("{}-{}", app.slug, &flow.provider.simple().to_string()[..4]);
            }
            Err(error) if is_unique(&error) => {
                return Err(conflict(
                    "already_connected",
                    "this GitHub App is already connected",
                ));
            }
            Err(error) => return Err(ApiError::database(error)),
        }
    };
    audit::record(
        &state.pool,
        Some(auth.id),
        "git_provider.create",
        "git_provider",
        Some(&row.id.to_string()),
        json!({ "name": row.name, "kind": row.kind, "via": "manifest", "app": app.slug }),
        None,
    )
    .await;
    let provider = provider_view(
        &state.cipher,
        row.id,
        row.name,
        row.kind,
        row.created_at,
        &encrypted,
    );
    let sealed = seal_state(&state, "install", auth.id, row.id)?;
    Ok((
        StatusCode::CREATED,
        Json(GithubAppCreated {
            provider,
            install_url: install_url(&app.slug, &sealed),
        }),
    ))
}

/// A provider row with its decrypted config.
struct AppRow {
    id: Uuid,
    name: String,
    kind: String,
    created_at: chrono::DateTime<Utc>,
    config: Value,
}

async fn load_app(state: &AppState, id: Uuid) -> Result<AppRow, ApiError> {
    let row = sqlx::query!(
        r#"SELECT id, name, kind, config_enc, created_at FROM git_providers WHERE id = $1"#,
        id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("git provider not found"))?;
    if row.kind != "github_app" {
        return Err(bad("invalid_kind", "this provider is not a GitHub App"));
    }
    let bytes = state.cipher.decrypt(&row.config_enc).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "stored git provider could not be decrypted",
        )
    })?;
    let config = serde_json::from_slice(&bytes).map_err(|_| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "stored git provider could not be decoded",
        )
    })?;
    Ok(AppRow {
        id: row.id,
        name: row.name,
        kind: row.kind,
        created_at: row.created_at,
        config,
    })
}

fn config_text<'a>(config: &'a Value, key: &str) -> &'a str {
    config.get(key).and_then(Value::as_str).unwrap_or("")
}

async fn installed(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<GithubAppInstalled>,
) -> Result<Json<GitProviderView>, ApiError> {
    auth.require_write()?;
    let installation_id = body.installation_id.trim();
    if installation_id.is_empty()
        || installation_id.len() > 20
        || !installation_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(bad("invalid", "the GitHub installation id is invalid"));
    }
    // GitHub only returns `state` on a first install from our link. After an
    // install made on GitHub, or a change to an existing one, it does not, so
    // the pending apps are tried instead. Either way GitHub itself confirms,
    // with the app's own key, that the installation belongs to the app.
    let explicit = body
        .state
        .as_deref()
        .map(str::trim)
        .filter(|token| !token.is_empty());
    let candidates = match explicit {
        Some(token) => vec![open_state(&state, token, "install", auth.id)?.provider],
        None => pending_apps(&state).await?,
    };
    let mut failure = None;
    for id in candidates {
        match attach_installation(&state, &auth, id, installation_id).await {
            Ok(view) => return Ok(Json(view)),
            Err(error) => failure = Some(error),
        }
    }
    Err(match (explicit, failure) {
        (Some(_), Some(error)) => error,
        _ => bad(
            "no_pending_app",
            "no GitHub App that is waiting for an installation matches this one",
        ),
    })
}

/// GitHub Apps created here that have no installation yet, oldest first.
async fn pending_apps(state: &AppState) -> Result<Vec<Uuid>, ApiError> {
    let ids = sqlx::query_scalar!(
        r#"SELECT id FROM git_providers WHERE kind = 'github_app' ORDER BY created_at"#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let mut pending = Vec::new();
    for id in ids {
        if let Ok(app) = load_app(state, id).await {
            if config_text(&app.config, "installation_id").is_empty() {
                pending.push(id);
            }
        }
    }
    Ok(pending)
}

/// Checks with GitHub that the installation belongs to the app, then saves it.
async fn attach_installation(
    state: &AppState,
    auth: &AuthUser,
    provider_id: Uuid,
    installation_id: &str,
) -> Result<GitProviderView, ApiError> {
    let mut app = load_app(state, provider_id).await?;
    let app_id = config_text(&app.config, "app_id").to_owned();
    let jwt = github_app_jwt(
        &app_id,
        config_text(&app.config, "private_key_pem"),
        Utc::now().timestamp(),
    )
    .map_err(core_error)?;
    let url = format!(
        "{}/app/installations/{installation_id}",
        state.config.github_api_url
    );
    let response = github_client()?
        .get(url)
        .header("authorization", format!("Bearer {jwt}"))
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28")
        .send()
        .await
        .map_err(|_| github_unreachable())?;
    let status = response.status();
    if status == StatusCode::NOT_FOUND {
        return Err(bad(
            "invalid",
            "GitHub has no such installation of this app",
        ));
    }
    if !status.is_success() {
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "github",
            format!("GitHub answered {status}"),
        ));
    }
    let value = read_json(response).await?;
    let reported = value
        .get("app_id")
        .and_then(Value::as_u64)
        .map(|id| id.to_string());
    if reported.as_deref() != Some(app_id.as_str()) {
        return Err(bad("invalid", "that installation belongs to another app"));
    }
    app.config["installation_id"] = json!(installation_id);
    if let Some(account) = value.pointer("/account/login").and_then(Value::as_str) {
        app.config["account"] = json!(account);
    }
    let encoded = serde_json::to_vec(&app.config).map_err(|_| crypto_error())?;
    let encrypted = state.cipher.encrypt(&encoded).map_err(|_| crypto_error())?;
    sqlx::query!(
        r#"UPDATE git_providers SET config_enc = $2, updated_at = now() WHERE id = $1"#,
        app.id,
        encrypted
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "git_provider.install",
        "git_provider",
        Some(&app.id.to_string()),
        json!({
            "name": app.name,
            "installation_id": installation_id,
            "account": config_text(&app.config, "account"),
        }),
        None,
    )
    .await;
    Ok(provider_view(
        &state.cipher,
        app.id,
        app.name,
        app.kind,
        app.created_at,
        &encrypted,
    ))
}

async fn install_link(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<InstallLink>, ApiError> {
    auth.require_write()?;
    let app = load_app(&state, id).await?;
    let slug = config_text(&app.config, "slug");
    if slug.is_empty() {
        return Err(bad(
            "invalid_kind",
            "this app was not created from the panel; install it on GitHub",
        ));
    }
    let sealed = seal_state(&state, "install", auth.id, app.id)?;
    Ok(Json(InstallLink {
        install_url: install_url(slug, &sealed),
    }))
}

/// Push events from the app. One app serves many repositories, so the
/// repository name and branch pick the services to build.
async fn webhook(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let app = load_app(&state, id).await.map_err(|_| unauthorized())?;
    let secret = config_text(&app.config, "webhook_secret");
    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|value| value.to_str().ok());
    if secret.is_empty()
        || !signature
            .is_some_and(|header| verify_github_signature(secret.as_bytes(), &body, header))
    {
        return Err(unauthorized());
    }
    let event = headers
        .get("x-github-event")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let ignored = |detail: &str| -> Result<(StatusCode, Json<Value>), ApiError> {
        Ok((
            StatusCode::OK,
            Json(json!({ "ok": true, "deploy": false, "detail": detail })),
        ))
    };
    if event == "ping" {
        return ignored("ping");
    }
    if event != "push" {
        return ignored("event ignored");
    }
    let payload: Value =
        serde_json::from_slice(&body).map_err(|_| bad("invalid", "payload is not json"))?;
    if payload.get("deleted").and_then(Value::as_bool) == Some(true) {
        return ignored("branch deleted");
    }
    let Some(full_name) = payload
        .pointer("/repository/full_name")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase)
    else {
        return Err(bad("invalid", "payload has no repository"));
    };
    let services = sqlx::query!(
        r#"
        SELECT id, spec
        FROM services
        WHERE kind = 'application' AND spec -> 'git' ->> 'provider_id' = $1
        "#,
        id.to_string()
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let mut queued = Vec::new();
    for service in services {
        let git = service.spec.get("git");
        let repo = git
            .and_then(|git| git.get("repo"))
            .and_then(Value::as_str)
            .and_then(github_repo_full_name);
        if repo.as_deref() != Some(full_name.as_str()) {
            continue;
        }
        let branch = git
            .and_then(|git| git.get("branch"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let outcome = evaluate_hook(&HookInput {
            kind: HookKind::Github,
            event,
            body: &body,
            github_signature: signature,
            gitlab_token: None,
            secret: secret.as_bytes(),
            branch,
        });
        match outcome {
            HookOutcome::Deploy { git_sha } => {
                match build_queue::enqueue(&state, service.id, "webhook", git_sha).await {
                    Ok(ticket) => {
                        record_delivery(&state, service.id, "github", &body, true, "queued").await;
                        queued.push(json!({
                            "service_id": service.id,
                            "deployment_id": ticket.deployment_id,
                        }));
                    }
                    Err(error) => {
                        tracing::warn!(service = %service.id, error = %error.public_message(), "app webhook could not queue a build");
                        record_delivery(&state, service.id, "github", &body, false, "not queued")
                            .await;
                    }
                }
            }
            HookOutcome::Ignored(detail) => {
                record_delivery(&state, service.id, "github", &body, true, detail).await;
            }
            HookOutcome::Rejected(detail) => {
                record_delivery(&state, service.id, "github", &body, false, detail).await;
            }
            HookOutcome::Unauthorized => {}
        }
    }
    if queued.is_empty() {
        return ignored("no service builds this branch");
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "ok": true, "deploy": true, "builds": queued })),
    ))
}
