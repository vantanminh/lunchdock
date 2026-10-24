//! Pull request previews.
//!
//! A service that opts in gets a copy for each pull request opened against its
//! branch. The copy builds the pull request's head branch and receives a preview
//! address. Closing the pull request removes the copy. Forks are ignored: the
//! head repository must be the same one the service tracks.

use rudder_core::{preview_hostname, validate_branch};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::build_queue;
use crate::error::ApiError;
use crate::nodes::driver_for;
use crate::publish;
use crate::routes::catalog::{bad, is_unique};
use crate::stacks::{self, StackAction, StackPreview, StackSpec};
use crate::state::AppState;

/// What the GitHub App webhook should answer for a `pull_request` event.
pub async fn handle(
    state: &AppState,
    provider_id: Uuid,
    payload: &Value,
) -> Result<Value, ApiError> {
    let action = payload.get("action").and_then(Value::as_str).unwrap_or("");
    if !matches!(action, "opened" | "synchronize" | "reopened" | "closed") {
        return Ok(json!({ "ok": true, "deploy": false, "detail": "pull request action ignored" }));
    }
    let number = payload
        .get("number")
        .and_then(Value::as_i64)
        .filter(|number| *number > 0)
        .ok_or_else(|| bad("invalid", "pull request has no number"))?;
    let base = text(payload, "/pull_request/base/ref")
        .ok_or_else(|| bad("invalid", "pull request has no base branch"))?;
    let head = text(payload, "/pull_request/head/ref")
        .ok_or_else(|| bad("invalid", "pull request has no head branch"))?;
    let sha = text(payload, "/pull_request/head/sha");
    let repo = text(payload, "/repository/full_name")
        .map(|name| name.to_ascii_lowercase())
        .ok_or_else(|| bad("invalid", "payload has no repository"))?;
    let head_repo =
        text(payload, "/pull_request/head/repo/full_name").map(|name| name.to_ascii_lowercase());
    if head_repo.as_deref() != Some(repo.as_str()) {
        return Ok(json!({
            "ok": true,
            "deploy": false,
            "detail": "fork pull requests are not previewed"
        }));
    }
    if validate_branch(&head).is_err() || validate_branch(&base).is_err() {
        return Ok(json!({ "ok": true, "deploy": false, "detail": "branch name is not usable" }));
    }
    let services = sqlx::query!(
        r#"
        SELECT id, name, kind, environment_id, server_id, spec
        FROM services
        WHERE kind IN ('application', 'compose')
          AND spec -> 'git' ->> 'provider_id' = $1
          AND COALESCE(spec ->> 'preview_pull_requests', 'false') = 'true'
        "#,
        provider_id.to_string()
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let mut builds = Vec::new();
    for service in services {
        let git_repo = service
            .spec
            .pointer("/git/repo")
            .and_then(Value::as_str)
            .and_then(rudder_core::github_repo_full_name);
        if git_repo.as_deref() != Some(repo.as_str()) {
            continue;
        }
        let branch = service
            .spec
            .pointer("/git/branch")
            .and_then(Value::as_str)
            .unwrap_or("");
        if branch != base {
            continue;
        }
        let outcome = if action == "closed" {
            remove_preview(state, service.id, number).await
        } else {
            deploy_preview(
                state,
                &Parent {
                    id: service.id,
                    name: service.name,
                    kind: service.kind,
                    environment_id: service.environment_id,
                    server_id: service.server_id,
                    spec: service.spec,
                },
                number,
                &head,
                sha.as_deref(),
            )
            .await
        };
        match outcome {
            Ok(Some(build)) => builds.push(build),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    service = %service.id,
                    error = %error.public_message(),
                    "pull request preview was not updated"
                );
            }
        }
    }
    if builds.is_empty() {
        return Ok(json!({
            "ok": true,
            "deploy": false,
            "detail": "no service previews this pull request"
        }));
    }
    Ok(json!({ "ok": true, "deploy": true, "builds": builds }))
}

struct Parent {
    id: Uuid,
    name: String,
    kind: String,
    environment_id: Uuid,
    server_id: Uuid,
    spec: Value,
}

async fn deploy_preview(
    state: &AppState,
    parent: &Parent,
    number: i64,
    head: &str,
    sha: Option<&str>,
) -> Result<Option<Value>, ApiError> {
    let existing = find_preview(state, parent.id, number).await?;
    let id = match existing {
        Some(id) => {
            point_at_head(state, id, &parent.kind, head).await?;
            id
        }
        None => create_preview(state, parent, number, head).await?,
    };
    let build = match parent.kind.as_str() {
        "compose" => {
            let deployment =
                stacks::start_action(state, id, StackAction::Rebuild, "preview").await?;
            json!({ "service_id": id, "deployment_id": deployment.id })
        }
        _ => {
            let ticket = build_queue::enqueue(state, id, "preview", sha.map(str::to_owned)).await?;
            json!({ "service_id": id, "deployment_id": ticket.deployment_id })
        }
    };
    Ok(Some(build))
}

async fn create_preview(
    state: &AppState,
    parent: &Parent,
    number: i64,
    head: &str,
) -> Result<Uuid, ApiError> {
    let name = preview_name(&parent.name, number);
    let spec = match parent.kind.as_str() {
        "compose" => compose_spec(&parent.spec, parent.id, number, head, &name)?,
        _ => app_spec(&parent.spec, parent.id, number, head),
    };
    let inserted = sqlx::query_scalar!(
        r#"
        INSERT INTO services (environment_id, server_id, name, kind, spec, source_type)
        VALUES ($1, $2, $3, $4, $5, 'git')
        RETURNING id
        "#,
        parent.environment_id,
        parent.server_id,
        name,
        parent.kind,
        spec
    )
    .fetch_one(&state.pool)
    .await;
    let id = match inserted {
        Ok(id) => id,
        Err(error) if is_unique(&error) => {
            return Err(bad(
                "name_taken",
                "a service with the preview name already exists",
            ))
        }
        Err(error) => return Err(ApiError::database(error)),
    };
    sqlx::query!(
        r#"
        INSERT INTO env_vars (service_id, key, value_enc, is_build_secret, updated_at)
        SELECT $2, key, value_enc, is_build_secret, updated_at
        FROM env_vars
        WHERE service_id = $1
        "#,
        parent.id,
        id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    add_preview_domain(state, id, &name, parent.id).await?;
    Ok(id)
}

fn app_spec(parent: &Value, parent_id: Uuid, number: i64, head: &str) -> Value {
    let mut spec = parent.clone();
    if let Some(git) = spec.get_mut("git") {
        git["branch"] = json!(head);
    }
    spec["preview_pull_requests"] = json!(false);
    spec["preview"] = json!({ "parent_id": parent_id, "number": number });
    spec
}

fn compose_spec(
    parent: &Value,
    parent_id: Uuid,
    number: i64,
    head: &str,
    name: &str,
) -> Result<Value, ApiError> {
    let mut spec: StackSpec = serde_json::from_value(parent.clone()).unwrap_or_default();
    if let Some(git) = spec.git.as_mut() {
        git.branch = head.to_owned();
    }
    spec.preview_pull_requests = false;
    spec.deploy_on_push = false;
    spec.preview = Some(StackPreview { parent_id, number });
    spec.project = stacks::project_name(name);
    spec.compose_yaml.clear();
    spec.generated_yaml.clear();
    spec.compose_path.clear();
    spec.services.clear();
    spec.containers.clear();
    spec.warnings.clear();
    Ok(stacks::spec_json(&spec))
}

async fn point_at_head(state: &AppState, id: Uuid, kind: &str, head: &str) -> Result<(), ApiError> {
    if kind == "compose" {
        let mut stack = stacks::load(state, id).await?;
        if let Some(git) = stack.spec.git.as_mut() {
            git.branch = head.to_owned();
        }
        stacks::save(state, id, &stack.spec).await?;
        return Ok(());
    }
    sqlx::query!(
        r#"
        UPDATE services
        SET spec = jsonb_set(spec, '{git,branch}', to_jsonb($2::text), true), updated_at = now()
        WHERE id = $1
        "#,
        id,
        head
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(())
}

async fn find_preview(
    state: &AppState,
    parent: Uuid,
    number: i64,
) -> Result<Option<Uuid>, ApiError> {
    sqlx::query_scalar!(
        r#"
        SELECT id
        FROM services
        WHERE spec -> 'preview' ->> 'parent_id' = $1
          AND spec -> 'preview' ->> 'number' = $2
        LIMIT 1
        "#,
        parent.to_string(),
        number.to_string()
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)
}

async fn remove_preview(
    state: &AppState,
    parent: Uuid,
    number: i64,
) -> Result<Option<Value>, ApiError> {
    let Some(id) = find_preview(state, parent, number).await? else {
        return Ok(None);
    };
    let row = sqlx::query!(r#"SELECT kind, server_id FROM services WHERE id = $1"#, id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.kind == "compose" {
        stacks::destroy(state, id, false).await?;
    } else {
        remove_app_containers(state, id, row.server_id).await;
        sqlx::query!(r#"DELETE FROM services WHERE id = $1"#, id)
            .execute(&state.pool)
            .await
            .map_err(ApiError::database)?;
        if let Err(error) = publish::publish_server(state, row.server_id).await {
            tracing::warn!(message = %error.public_message(), "routes were not republished");
        }
    }
    Ok(Some(json!({ "service_id": id, "removed": true })))
}

async fn remove_app_containers(state: &AppState, id: Uuid, server_id: Uuid) {
    let Ok(driver) = driver_for(state, server_id).await else {
        return;
    };
    let label = id.to_string();
    let Ok(list) = driver.list_containers().await else {
        return;
    };
    for container in list {
        if container.labels.get("rudder.service") == Some(&label) {
            let _ = driver.remove_container(&container.id, true).await;
        }
    }
}

async fn add_preview_domain(
    state: &AppState,
    service_id: Uuid,
    name: &str,
    parent_id: Uuid,
) -> Result<(), ApiError> {
    let server = sqlx::query!(
        r#"
        SELECT s.server_id, servers.public_ip
        FROM services s
        JOIN servers ON servers.id = s.server_id
        WHERE s.id = $1
        "#,
        service_id
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let port = sqlx::query_scalar!(
        r#"SELECT target_port FROM domains WHERE service_id = $1 ORDER BY created_at LIMIT 1"#,
        parent_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .unwrap_or(80);
    let nonce = hex::encode(rudder_core::random_bytes::<4>());
    let host = preview_hostname(name, &nonce, server.public_ip.as_deref().unwrap_or(""))
        .map_err(|error| bad("invalid", error.to_string()))?;
    let compose_service = match stacks::domain_target(state, service_id, None).await {
        Ok(service) => service,
        Err(error) => {
            tracing::warn!(message = %error.public_message(), "preview has no domain yet");
            return Ok(());
        }
    };
    sqlx::query!(
        r#"
        INSERT INTO domains (
            service_id, host, path_prefix, target_port, https_redirect, ssl_mode, is_preview,
            compose_service
        )
        VALUES ($1, $2, '/', $3, TRUE, 'letsencrypt', TRUE, $4)
        "#,
        service_id,
        host,
        port,
        compose_service
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    publish::publish_server(state, server.server_id).await?;
    Ok(())
}

/// `{name}-pr-{number}`, shortened so it stays a valid service name.
pub fn preview_name(parent: &str, number: i64) -> String {
    let suffix = format!("-pr-{number}");
    let room = 63usize.saturating_sub(suffix.len()).max(1);
    let mut base: String = parent.chars().take(room).collect();
    while base.ends_with('-') {
        base.pop();
    }
    if base.is_empty() {
        base.push('p');
    }
    format!("{base}{suffix}")
}

fn text(value: &Value, pointer: &str) -> Option<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_names_stay_within_the_service_limit() {
        let long = "a".repeat(63);
        let name = preview_name(&long, 128);
        assert!(name.len() <= 63);
        assert!(name.ends_with("-pr-128"));
        rudder_core::validate_name(&name).unwrap();
        assert_eq!(preview_name("web", 7), "web-pr-7");
    }
}
