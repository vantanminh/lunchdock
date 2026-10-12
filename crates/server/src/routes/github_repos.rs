//! Repositories, branches and compose files a GitHub App installation can read, so a source
//! is picked from a list instead of typed, and a typo or a missing grant shows up when the
//! source is saved instead of at the first deploy.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Json;
use rudder_core::{github_repo_full_name, validate_branch};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::build_queue::installation_token;
use crate::error::ApiError;
use crate::routes::catalog::{bad, not_found};
use crate::routes::github_app::{config_text, load_app};
use crate::state::AppState;

/// Pages of 100 read from GitHub per list, so a huge account cannot stall the panel.
const MAX_PAGES: u32 = 5;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_COMPOSE_FILES: usize = 50;

#[derive(Serialize, ToSchema)]
pub struct GithubRepoView {
    /// `owner/name`.
    pub full_name: String,
    /// HTTPS clone address to use as the repository.
    pub url: String,
    pub private: bool,
    pub default_branch: String,
}

#[derive(Serialize, ToSchema)]
pub struct GithubBranchesView {
    pub default_branch: String,
    pub branches: Vec<String>,
}

#[derive(Serialize, ToSchema)]
pub struct GithubComposeFilesView {
    /// Paths of compose files in the repository, shortest first.
    pub files: Vec<String>,
}

#[derive(Deserialize)]
pub struct RepoQuery {
    /// Repository URL or `owner/name`.
    pub repo: String,
}

#[derive(Deserialize)]
pub struct ComposeQuery {
    pub repo: String,
    pub branch: String,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/git-providers/{id}/repositories", get(repositories))
        .route("/api/v1/git-providers/{id}/branches", get(branches))
        .route(
            "/api/v1/git-providers/{id}/compose-files",
            get(compose_files),
        )
}

/// An installation token and the API it works against.
struct Github {
    api_url: String,
    token: String,
    client: reqwest::Client,
}

enum Fetch {
    Found(Value),
    Missing,
}

impl Github {
    async fn for_provider(state: &AppState, provider: Uuid) -> Result<Self, ApiError> {
        let app = load_app(state, provider).await?;
        let installation = config_text(&app.config, "installation_id");
        if installation.is_empty() {
            return Err(bad(
                "invalid",
                "install the GitHub App on GitHub before using it as a source",
            ));
        }
        let token = installation_token(
            &state.config.github_api_url,
            config_text(&app.config, "app_id"),
            installation,
            config_text(&app.config, "private_key_pem"),
        )
        .await
        .map_err(|message| ApiError::new(StatusCode::BAD_GATEWAY, "github", message))?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(format!("rudder/{}", rudder_core::VERSION))
            .build()
            .map_err(|_| unreachable_github())?;
        Ok(Self {
            api_url: state.config.github_api_url.clone(),
            token,
            client,
        })
    }

    /// `GET path` as the installation. A 404 or 403 means the app cannot see it.
    async fn get(&self, path: &str) -> Result<Fetch, ApiError> {
        let response = self
            .client
            .get(format!("{}{path}", self.api_url))
            .header("authorization", format!("Bearer {}", self.token))
            .header("accept", "application/vnd.github+json")
            .header("x-github-api-version", "2022-11-28")
            .send()
            .await
            .map_err(|_| unreachable_github())?;
        let status = response.status();
        if matches!(status, StatusCode::NOT_FOUND | StatusCode::FORBIDDEN) {
            return Ok(Fetch::Missing);
        }
        if !status.is_success() {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "github",
                format!("GitHub answered {status}"),
            ));
        }
        let bytes = response.bytes().await.map_err(|_| unreachable_github())?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(unreachable_github());
        }
        serde_json::from_slice(&bytes)
            .map(Fetch::Found)
            .map_err(|_| unreachable_github())
    }
}

fn unreachable_github() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "github",
        "could not reach GitHub; check the panel's internet access",
    )
}

/// `owner/name` of a repository given as a URL or as `owner/name`.
fn repo_name(repo: &str) -> Result<String, ApiError> {
    let repo = repo.trim();
    let name = if repo.contains("://") || repo.starts_with("git@") {
        github_repo_full_name(repo)
    } else {
        github_repo_full_name(&format!("https://github.com/{repo}"))
    };
    name.ok_or_else(|| bad("invalid", "that is not a github.com repository"))
}

/// A branch name as one path segment.
fn encode_branch(branch: &str) -> Result<String, ApiError> {
    validate_branch(branch).map_err(crate::routes::catalog::core_error)?;
    Ok(branch.replace('/', "%2F"))
}

async fn repositories(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(provider): Path<Uuid>,
) -> Result<Json<Vec<GithubRepoView>>, ApiError> {
    let github = Github::for_provider(&state, provider).await?;
    let mut repos = Vec::new();
    for page in 1..=MAX_PAGES {
        let path = format!("/installation/repositories?per_page=100&page={page}");
        let Fetch::Found(body) = github.get(&path).await? else {
            break;
        };
        let items = body
            .get("repositories")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let count = items.len();
        for item in items {
            let text = |key: &str| item.get(key).and_then(Value::as_str).unwrap_or("");
            if text("full_name").is_empty() || text("clone_url").is_empty() {
                continue;
            }
            repos.push(GithubRepoView {
                full_name: text("full_name").to_owned(),
                url: text("clone_url").to_owned(),
                private: item
                    .get("private")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                default_branch: text("default_branch").to_owned(),
            });
        }
        if count < 100 {
            break;
        }
    }
    repos.sort_by_key(|repo| repo.full_name.to_ascii_lowercase());
    Ok(Json(repos))
}

async fn branches(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(provider): Path<Uuid>,
    Query(query): Query<RepoQuery>,
) -> Result<Json<GithubBranchesView>, ApiError> {
    let name = repo_name(&query.repo)?;
    let github = Github::for_provider(&state, provider).await?;
    let Fetch::Found(repo) = github.get(&format!("/repos/{name}")).await? else {
        return Err(not_accessible(&name));
    };
    let default_branch = repo
        .get("default_branch")
        .and_then(Value::as_str)
        .unwrap_or("main")
        .to_owned();
    let mut branches = Vec::new();
    for page in 1..=MAX_PAGES {
        let path = format!("/repos/{name}/branches?per_page=100&page={page}");
        let Fetch::Found(body) = github.get(&path).await? else {
            break;
        };
        let items = body.as_array().cloned().unwrap_or_default();
        let count = items.len();
        branches.extend(
            items
                .iter()
                .filter_map(|item| item.get("name").and_then(Value::as_str))
                .map(str::to_owned),
        );
        if count < 100 {
            break;
        }
    }
    branches.sort();
    if let Some(index) = branches.iter().position(|branch| *branch == default_branch) {
        let default = branches.remove(index);
        branches.insert(0, default);
    }
    Ok(Json(GithubBranchesView {
        default_branch,
        branches,
    }))
}

/// Compose files look like `compose.yaml`, `docker-compose.prod.yml`, `compose.prod.yml`.
fn is_compose_file(path: &str) -> bool {
    let file = path.rsplit('/').next().unwrap_or(path);
    let Some(stem) = file
        .strip_suffix(".yml")
        .or_else(|| file.strip_suffix(".yaml"))
    else {
        return false;
    };
    ["compose", "docker-compose"].iter().any(|base| {
        stem == *base
            || stem.starts_with(&format!("{base}."))
            || stem.starts_with(&format!("{base}-"))
    }) || stem.ends_with(".compose")
}

async fn compose_files(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(provider): Path<Uuid>,
    Query(query): Query<ComposeQuery>,
) -> Result<Json<GithubComposeFilesView>, ApiError> {
    let name = repo_name(&query.repo)?;
    let branch = encode_branch(&query.branch)?;
    let github = Github::for_provider(&state, provider).await?;
    let Fetch::Found(tree) = github
        .get(&format!("/repos/{name}/git/trees/{branch}?recursive=1"))
        .await?
    else {
        return Err(not_found(format!(
            "branch {} was not found in {name}",
            query.branch
        )));
    };
    let mut files: Vec<String> = tree
        .get("tree")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("blob"))
                .filter_map(|entry| entry.get("path").and_then(Value::as_str))
                .filter(|path| is_compose_file(path))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    files.sort_by(|a, b| {
        a.matches('/')
            .count()
            .cmp(&b.matches('/').count())
            .then_with(|| a.cmp(b))
    });
    files.truncate(MAX_COMPOSE_FILES);
    Ok(Json(GithubComposeFilesView { files }))
}

fn not_accessible(name: &str) -> ApiError {
    bad(
        "repo_not_accessible",
        format!(
            "the GitHub App cannot read {name}; check the spelling or add the repository to the app's installation on GitHub"
        ),
    )
}

/// Check, when a source is saved, that the GitHub App behind `provider` can read the repository
/// and the branch. Anything else (another kind of provider, a repository outside github.com, a
/// GitHub that cannot be reached) is let through; the first deploy reports those.
pub(crate) async fn check_source_access(
    state: &AppState,
    provider: Uuid,
    repo: &str,
    branch: &str,
) -> Result<(), ApiError> {
    let Ok(app) = load_app(state, provider).await else {
        return Ok(());
    };
    if config_text(&app.config, "installation_id").is_empty() {
        return Ok(());
    }
    let Some(name) = github_repo_full_name(repo) else {
        return Ok(());
    };
    let Ok(github) = Github::for_provider(state, provider).await else {
        return Ok(());
    };
    let encoded = encode_branch(branch)?;
    match github.get(&format!("/repos/{name}")).await {
        Ok(Fetch::Missing) => return Err(not_accessible(&name)),
        Ok(Fetch::Found(_)) => {}
        Err(_) => return Ok(()),
    }
    if let Ok(Fetch::Missing) = github
        .get(&format!("/repos/{name}/branches/{encoded}"))
        .await
    {
        return Err(bad(
            "branch_not_found",
            format!("branch {branch} was not found in {name}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_file_names() {
        for yes in [
            "compose.yml",
            "compose.yaml",
            "docker-compose.yml",
            "deploy/docker-compose.prod.yaml",
            "ops/compose-prod.yml",
            "ops/app.compose.yml",
        ] {
            assert!(is_compose_file(yes), "{yes}");
        }
        for no in [
            "README.md",
            "Dockerfile",
            "ci.yml",
            "composer.yml",
            "compose.json",
        ] {
            assert!(!is_compose_file(no), "{no}");
        }
    }

    #[test]
    fn repo_names_from_urls_and_slugs() {
        assert_eq!(repo_name("Acme/Web").unwrap(), "acme/web");
        assert_eq!(
            repo_name("https://github.com/acme/web.git").unwrap(),
            "acme/web"
        );
        assert!(repo_name("https://gitlab.com/acme/web").is_err());
        assert!(repo_name("acme").is_err());
    }
}
