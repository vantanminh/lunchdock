//! Servers, projects, environments, services, and encrypted env keys.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

fn app(pool: PgPool) -> axum::Router {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_PUBLIC_URL".into(), "https://panel.example".into()),
    ]);
    let config = rudder_server::Config::from_map(&vars, |_| unreachable!()).unwrap();
    rudder_server::router(rudder_server::AppState::new(pool, config))
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
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", format!("rudder_session={cookie}"));
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    let request = if let Some(body) = body {
        builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
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

async fn owner(app: &axum::Router) -> (String, String) {
    let created = call(
        app,
        "POST",
        "/api/v1/setup",
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "correct-horse-battery"
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK);
    let cookie = created.cookie.unwrap();
    let csrf = created.body["csrf_token"].as_str().unwrap().to_owned();
    (cookie, csrf)
}

/// A local server, project, environment and application service; returns the service id.
async fn a_service(app: &axum::Router, cookie: &str, csrf: &str) -> String {
    let server = call(
        app,
        "POST",
        "/api/v1/servers",
        Some(cookie),
        Some(csrf),
        Some(json!({ "name": "local", "kind": "local" })),
    )
    .await;
    let project = call(
        app,
        "POST",
        "/api/v1/projects",
        Some(cookie),
        Some(csrf),
        Some(json!({ "name": "blog" })),
    )
    .await;
    let environment = call(
        app,
        "POST",
        &format!(
            "/api/v1/projects/{}/environments",
            project.body["id"].as_str().unwrap()
        ),
        Some(cookie),
        Some(csrf),
        Some(json!({ "name": "prod" })),
    )
    .await;
    let service = call(
        app,
        "POST",
        &format!(
            "/api/v1/environments/{}/services",
            environment.body["id"].as_str().unwrap()
        ),
        Some(cookie),
        Some(csrf),
        Some(json!({
            "name": "web",
            "kind": "application",
            "server_id": server.body["id"],
            "spec": { "image": "ghcr.io/acme/web:1" }
        })),
    )
    .await;
    service.body["id"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrations = "./migrations")]
async fn servers_projects_and_services_round_trip(pool: PgPool) {
    let app = app(pool);
    let (cookie, csrf) = owner(&app).await;

    let local = call(
        &app,
        "POST",
        "/api/v1/servers",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "local", "kind": "local" })),
    )
    .await;
    assert_eq!(local.status, StatusCode::CREATED);
    assert!(local.body["join_token"].is_null());
    let local_id = local.body["id"].as_str().unwrap().to_owned();

    let duplicate = call(
        &app,
        "POST",
        "/api/v1/servers",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "other", "kind": "local" })),
    )
    .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);

    let remote = call(
        &app,
        "POST",
        "/api/v1/servers",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "edge", "kind": "remote", "public_ip": "203.0.113.10" })),
    )
    .await;
    assert_eq!(remote.status, StatusCode::CREATED);
    let token = remote.body["join_token"].as_str().unwrap();
    assert!(token.starts_with("join_"));
    assert!(remote.body["join_command"]
        .as_str()
        .unwrap()
        .contains("https://panel.example"));
    let remote_id = remote.body["id"].as_str().unwrap();

    let listed = call(&app, "GET", "/api/v1/servers", Some(&cookie), None, None).await;
    assert_eq!(listed.status, StatusCode::OK);
    assert!(!listed.body.to_string().contains(token));

    let bad_name = call(
        &app,
        "POST",
        "/api/v1/projects",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "Bad Name" })),
    )
    .await;
    assert_eq!(bad_name.status, StatusCode::BAD_REQUEST);

    let project = call(
        &app,
        "POST",
        "/api/v1/projects",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "blog", "description": "notes" })),
    )
    .await;
    assert_eq!(project.status, StatusCode::CREATED);
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
    assert_eq!(environment.status, StatusCode::CREATED);
    let environment_id = environment.body["id"].as_str().unwrap();

    let service = call(
        &app,
        "POST",
        &format!("/api/v1/environments/{environment_id}/services"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "web",
            "kind": "application",
            "server_id": local_id,
            "spec": { "image": "ghcr.io/acme/web:1" }
        })),
    )
    .await;
    assert_eq!(service.status, StatusCode::CREATED);
    let service_id = service.body["id"].as_str().unwrap();
    assert_eq!(service.body["spec"]["image"], "ghcr.io/acme/web:1");

    let all = call(&app, "GET", "/api/v1/services", Some(&cookie), None, None).await;
    assert_eq!(all.status, StatusCode::OK);
    assert_eq!(all.body[0]["id"], service_id);
    assert_eq!(all.body[0]["environment_name"], "prod");
    assert!(all.body[0]["project_name"].is_string());

    let stored = call(
        &app,
        "PUT",
        &format!("/api/v1/services/{service_id}/env"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "key": "DATABASE_URL",
            "value": "postgres://secret",
            "is_build_secret": false
        })),
    )
    .await;
    assert_eq!(stored.status, StatusCode::NO_CONTENT);
    let env = call(
        &app,
        "GET",
        &format!("/api/v1/services/{service_id}/env"),
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(env.status, StatusCode::OK);
    assert_eq!(env.body[0]["key"], "DATABASE_URL");
    assert!(!env.body.to_string().contains("postgres://secret"));

    let removed_local = call(
        &app,
        "DELETE",
        &format!("/api/v1/servers/{local_id}"),
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(removed_local.status, StatusCode::CONFLICT);
    let removed_remote = call(
        &app,
        "DELETE",
        &format!("/api/v1/servers/{remote_id}"),
        Some(&cookie),
        Some(&csrf),
        None,
    )
    .await;
    assert_eq!(removed_remote.status, StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "./migrations")]
async fn env_import_previews_applies_and_verifies(pool: PgPool) {
    let app = app(pool);
    let (cookie, csrf) = owner(&app).await;
    let service_id = a_service(&app, &cookie, &csrf).await;
    let import = format!("/api/v1/services/{service_id}/env/import");
    let list = format!("/api/v1/services/{service_id}/env");

    let preview = call(
        &app,
        "POST",
        &import,
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "text": "A=1\nB='two words'\n", "dry_run": true })),
    )
    .await;
    assert_eq!(preview.status, StatusCode::OK);
    assert_eq!(preview.body["applied"], false);
    assert_eq!(preview.body["entries"][0]["key"], "A");
    assert_eq!(preview.body["entries"][0]["status"], "new");
    assert_eq!(preview.body["entries"][1]["status"], "new");
    assert_eq!(preview.body["entries"][1]["length"], 9);
    let none = call(&app, "GET", &list, Some(&cookie), None, None).await;
    assert_eq!(none.body.as_array().unwrap().len(), 0);

    let applied = call(
        &app,
        "POST",
        &import,
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "text": "A=1\nB='two words'\n" })),
    )
    .await;
    assert_eq!(applied.status, StatusCode::OK);
    assert_eq!(applied.body["applied"], true);

    let again = call(
        &app,
        "POST",
        &import,
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "text": "A=1\nB=changed\nC=3", "dry_run": true })),
    )
    .await;
    let statuses: Vec<&str> = again.body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["unchanged", "changed", "new"]);

    let broken = call(
        &app,
        "POST",
        &import,
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "text": "D=4\nnot a variable line\n" })),
    )
    .await;
    assert_eq!(broken.status, StatusCode::BAD_REQUEST);
    assert_eq!(broken.body["error"]["code"], "invalid_env");
    let listed = call(&app, "GET", &list, Some(&cookie), None, None).await;
    let keys: Vec<&str> = listed
        .body
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["A", "B"]);
    assert_eq!(listed.body[1]["length"], 9);
    assert!(listed.body[1]["updated_at"].is_string());
    assert!(!listed.body.to_string().contains("two words"));

    let verify = |value: &'static str| {
        let app = app.clone();
        let cookie = cookie.clone();
        let csrf = csrf.clone();
        let path = format!("/api/v1/services/{service_id}/env/B/verify");
        async move {
            call(
                &app,
                "POST",
                &path,
                Some(&cookie),
                Some(&csrf),
                Some(json!({ "value": value })),
            )
            .await
        }
    };
    let good = verify("two words").await;
    assert_eq!(good.status, StatusCode::OK);
    assert_eq!(good.body["matches"], true);
    assert_eq!(verify("two word").await.body["matches"], false);
    assert!(!good.body.to_string().contains("two words"));
    let missing = call(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/env/NOPE/verify"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "value": "x" })),
    )
    .await;
    assert_eq!(missing.status, StatusCode::NOT_FOUND);
}

#[sqlx::test(migrations = "./migrations")]
async fn viewers_cannot_create_projects(pool: PgPool) {
    let app = app(pool);
    let (cookie, csrf) = owner(&app).await;
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
    let viewer_cookie = login.cookie.unwrap();
    let viewer_csrf = login.body["csrf_token"].as_str().unwrap();
    let denied = call(
        &app,
        "POST",
        "/api/v1/projects",
        Some(&viewer_cookie),
        Some(viewer_csrf),
        Some(json!({ "name": "nope" })),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
}
