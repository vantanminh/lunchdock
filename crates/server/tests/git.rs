//! Git provider storage, webhook checks, and the build queue.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

fn state(pool: PgPool) -> rudder_server::AppState {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let data_dir = std::env::temp_dir().join(format!("rudder-git-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_PUBLIC_URL".into(), "https://panel.example".into()),
        ("RUDDER_DATA_DIR".into(), data_dir.display().to_string()),
    ]);
    let config = rudder_server::Config::from_map(&vars, |_| unreachable!()).unwrap();
    rudder_server::AppState::new(pool, config)
}

struct Reply {
    status: StatusCode,
    body: Value,
    cookie: Option<String>,
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let bytes = body.map(|value| value.to_string());
    call_raw(app, method, path, cookie, csrf, bytes.as_deref(), &[]).await
}

async fn call_raw(
    app: &axum::Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    body: Option<&str>,
    extra: &[(&str, &str)],
) -> Reply {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", format!("rudder_session={cookie}"));
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let request = if let Some(body) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    };
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .and_then(|pair| pair.strip_prefix("rudder_session="))
        .map(str::to_owned);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply {
        status,
        body,
        cookie,
    }
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let digest = Sha256::digest(key);
        key_block[..digest.len()].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0u8; BLOCK];
    let mut opad = [0u8; BLOCK];
    for (index, byte) in key_block.iter().enumerate() {
        ipad[index] = byte ^ 0x36;
        opad[index] = byte ^ 0x5c;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    hex::encode(outer.finalize())
}

async fn owner(app: &axum::Router) -> (String, String) {
    let created = call(
        app,
        "POST",
        "/api/v1/setup",
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "correct-horse-battery",
            "display_name": "Owner"
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK);
    (
        created.cookie.expect("session cookie"),
        created.body["csrf_token"].as_str().unwrap().to_owned(),
    )
}

#[sqlx::test(migrations = "./migrations")]
async fn webhook_enqueues_and_remote_build_fails_closed(pool: PgPool) {
    let state = state(pool);
    let app = rudder_server::router(state.clone());
    let (cookie, csrf) = owner(&app).await;
    let docs = call(&app, "GET", "/api/openapi.json", None, None, None).await;
    let document = docs.body.to_string();
    assert!(document.contains("/api/v1/git-providers"));
    assert!(document.contains("/api/v1/services/{id}/webhook"));

    let server = call(
        &app,
        "POST",
        "/api/v1/servers",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "local", "kind": "local" })),
    )
    .await;
    assert_eq!(server.status, StatusCode::CREATED);
    let server_id = server.body["id"].as_str().unwrap();
    let remote = call(
        &app,
        "POST",
        "/api/v1/servers",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "edge", "kind": "remote" })),
    )
    .await;
    assert_eq!(remote.status, StatusCode::CREATED);
    let remote_id = remote.body["id"].as_str().unwrap();

    let project = call(
        &app,
        "POST",
        "/api/v1/projects",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "web" })),
    )
    .await;
    let project_id = project.body["id"].as_str().unwrap();
    let environment = call(
        &app,
        "POST",
        &format!("/api/v1/projects/{project_id}/environments"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "prod" })),
    )
    .await;
    let environment_id = environment.body["id"].as_str().unwrap();

    let token = "github-pat-secret-value";
    let provider = call(
        &app,
        "POST",
        "/api/v1/git-providers",
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "github",
            "kind": "github_pat",
            "token": token
        })),
    )
    .await;
    assert_eq!(provider.status, StatusCode::CREATED);
    assert!(!provider.body.to_string().contains(token));
    let provider_id = provider.body["id"].as_str().unwrap();
    let listed = call(
        &app,
        "GET",
        "/api/v1/git-providers",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert!(!listed.body.to_string().contains(token));

    let service = call(
        &app,
        "POST",
        &format!("/api/v1/environments/{environment_id}/services"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "web",
            "kind": "application",
            "server_id": server_id
        })),
    )
    .await;
    let service_id = service.body["id"].as_str().unwrap();
    let source = call(
        &app,
        "PUT",
        &format!("/api/v1/services/{service_id}/source"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "provider_id": provider_id,
            "repo": "https://github.com/acme/web.git",
            "branch": "main",
            "strategy": "dockerfile"
        })),
    )
    .await;
    assert_eq!(source.status, StatusCode::OK);
    let secret = source.body["webhook_secret"].as_str().unwrap().to_owned();
    assert!(!source.body.to_string().contains(token));
    let loaded = call(
        &app,
        "GET",
        &format!("/api/v1/services/{service_id}"),
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert!(!loaded.body.to_string().contains(&secret));
    assert!(!loaded.body.to_string().contains(token));

    let payload = r#"{"ref":"refs/heads/main","after":"abcdef1"}"#;
    let bad = call_raw(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/webhook"),
        None,
        None,
        Some(payload),
        &[
            ("x-github-event", "push"),
            (
                "x-hub-signature-256",
                "sha256=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ),
        ],
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);

    let signature = format!(
        "sha256={}",
        hmac_sha256(secret.as_bytes(), payload.as_bytes())
    );
    let accepted = call_raw(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/webhook"),
        None,
        None,
        Some(payload),
        &[
            ("x-github-event", "push"),
            ("x-hub-signature-256", &signature),
        ],
    )
    .await;
    assert_eq!(accepted.status, StatusCode::ACCEPTED);
    assert_eq!(accepted.body["deploy"], true);
    let queue_id = accepted.body["queue_id"].as_str().unwrap();

    let other = r#"{"ref":"refs/heads/dev","after":"abcdef1"}"#;
    let other_sig = format!(
        "sha256={}",
        hmac_sha256(secret.as_bytes(), other.as_bytes())
    );
    let filtered = call_raw(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/webhook"),
        None,
        None,
        Some(other),
        &[
            ("x-github-event", "push"),
            ("x-hub-signature-256", &other_sig),
        ],
    )
    .await;
    assert_eq!(filtered.status, StatusCode::OK);
    assert_eq!(filtered.body["deploy"], false);

    let cancel = call(
        &app,
        "POST",
        &format!("/api/v1/build-queue/{queue_id}/cancel"),
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(cancel.status, StatusCode::NO_CONTENT);

    let remote_service = call(
        &app,
        "POST",
        &format!("/api/v1/environments/{environment_id}/services"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "api",
            "kind": "application",
            "server_id": remote_id
        })),
    )
    .await;
    let remote_service_id = remote_service.body["id"].as_str().unwrap();
    let remote_source = call(
        &app,
        "PUT",
        &format!("/api/v1/services/{remote_service_id}/source"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "provider_id": provider_id,
            "repo": "https://github.com/acme/api.git",
            "branch": "main",
            "strategy": "nixpacks"
        })),
    )
    .await;
    assert_eq!(remote_source.status, StatusCode::OK);
    let queued = call(
        &app,
        "POST",
        &format!("/api/v1/services/{remote_service_id}/build"),
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(queued.status, StatusCode::ACCEPTED);
    assert!(rudder_server::build_once(&state).await.unwrap());
    let deployments = call(
        &app,
        "GET",
        &format!("/api/v1/services/{remote_service_id}/deployments"),
        Some(&cookie),
        None,
        None,
    )
    .await;
    let rendered = deployments.body.to_string();
    assert!(rendered.contains("failed"));
    assert!(rendered.contains("agent"));
    assert!(!rendered.contains(token));

    let viewer = call(
        &app,
        "POST",
        "/api/v1/users",
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "email": "viewer@example.com",
            "password": "viewer-password-ok",
            "role": "viewer"
        })),
    )
    .await;
    assert_eq!(viewer.status, StatusCode::CREATED);
    let login = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        None,
        Some(json!({
            "email": "viewer@example.com",
            "password": "viewer-password-ok"
        })),
    )
    .await;
    let denied = call(
        &app,
        "POST",
        "/api/v1/git-providers",
        login.cookie.as_deref(),
        login.body["csrf_token"].as_str(),
        Some(json!({
            "name": "nope",
            "kind": "github_pat",
            "token": "another-secret-token"
        })),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
}
