//! Load guard API: pausing builds holds the queue, settings are validated and
//! stored, and cancelling clears queued builds.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

fn state(pool: PgPool) -> rudder_server::AppState {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_PUBLIC_URL".into(), "https://panel.example".into()),
    ]);
    let config = rudder_server::Config::from_map(&vars, |_| unreachable!()).unwrap();
    rudder_server::AppState::new(pool, config)
}

struct Client {
    app: axum::Router,
    cookie: String,
    csrf: String,
}

impl Client {
    async fn owner(app: axum::Router) -> Self {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/setup")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({
                    "email": "owner@example.com",
                    "password": "correct-horse-battery",
                    "display_name": "Owner"
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response
            .headers()
            .get("set-cookie")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split(';').next())
            .and_then(|pair| pair.strip_prefix("rudder_session="))
            .unwrap()
            .to_owned();
        let body: Value =
            serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        Self {
            app,
            cookie,
            csrf: body["csrf_token"].as_str().unwrap().to_owned(),
        }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(path)
            .header("cookie", format!("rudder_session={}", self.cookie))
            .header("x-csrf-token", &self.csrf);
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }
}

/// A local server, a service with a git source, and one queued build. Returns
/// `(deployment id, queue id)`.
async fn queued_build(pool: &PgPool) -> (Uuid, Uuid) {
    let server: Uuid = sqlx::query_scalar(
        "INSERT INTO servers (name, kind) VALUES ('main', 'local') RETURNING id",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    let project: Uuid =
        sqlx::query_scalar("INSERT INTO projects (name) VALUES ('shop') RETURNING id")
            .fetch_one(pool)
            .await
            .unwrap();
    let environment: Uuid = sqlx::query_scalar(
        "INSERT INTO environments (project_id, name) VALUES ($1, 'production') RETURNING id",
    )
    .bind(project)
    .fetch_one(pool)
    .await
    .unwrap();
    let service: Uuid = sqlx::query_scalar(
        "INSERT INTO services (environment_id, server_id, name, kind, spec)
         VALUES ($1, $2, 'web', 'application', '{\"git\": {}}') RETURNING id",
    )
    .bind(environment)
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap();
    let deployment: Uuid = sqlx::query_scalar(
        "INSERT INTO deployments (service_id, status, trigger) VALUES ($1, 'queued', 'push') RETURNING id",
    )
    .bind(service)
    .fetch_one(pool)
    .await
    .unwrap();
    let queue: Uuid = sqlx::query_scalar(
        "INSERT INTO build_queue (deployment_id, server_id, status) VALUES ($1, $2, 'queued') RETURNING id",
    )
    .bind(deployment)
    .bind(server)
    .fetch_one(pool)
    .await
    .unwrap();
    (deployment, queue)
}

#[sqlx::test(migrations = "./migrations")]
async fn paused_builds_wait_and_resume(pool: PgPool) {
    let app_state = state(pool.clone());
    let client = Client::owner(rudder_server::router(app_state.clone())).await;
    let (_, queue) = queued_build(&pool).await;

    let (status, view) = client.call("GET", "/api/v1/load", None).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["guard"]["level"], "ok");
    assert_eq!(view["guard"]["settings"]["enabled"], true);
    assert_eq!(view["builds"]["queued"], 1);
    assert_eq!(view["builds"]["hold"], Value::Null);
    assert_eq!(view["builds"]["concurrency"], 1);

    let (status, view) = client
        .call(
            "POST",
            "/api/v1/load/pause",
            Some(json!({ "paused": true })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["builds"]["hold"], "paused");
    assert_eq!(view["guard"]["holding"], true);
    let paused: bool = sqlx::query_scalar("SELECT builds_paused FROM panel_settings")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(paused);

    // The queue does not claim the build while builds are paused.
    assert!(!rudder_server::build_once(&app_state).await.unwrap());
    let still: String = sqlx::query_scalar("SELECT status FROM build_queue WHERE id = $1")
        .bind(queue)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(still, "queued");

    let (status, view) = client
        .call(
            "POST",
            "/api/v1/load/pause",
            Some(json!({ "paused": false })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["builds"]["hold"], Value::Null);

    let events: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM system_events ORDER BY created_at")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(events, vec!["guard.paused", "guard.resumed"]);
}

#[sqlx::test(migrations = "./migrations")]
async fn settings_are_validated_and_saved(pool: PgPool) {
    let client = Client::owner(rudder_server::router(state(pool.clone()))).await;
    let (status, _) = client
        .call(
            "PUT",
            "/api/v1/load/settings",
            Some(json!({ "enabled": true, "memory_reserve_percent": 2, "cpu_percent": 90 })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = client
        .call(
            "PUT",
            "/api/v1/load/settings",
            Some(json!({
                "enabled": true,
                "memory_reserve_percent": 20,
                "cpu_percent": 85,
                "build_concurrency": 0
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, view) = client
        .call(
            "PUT",
            "/api/v1/load/settings",
            Some(json!({
                "enabled": false,
                "memory_reserve_percent": 20,
                "cpu_percent": 85,
                "build_concurrency": 2
            })),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["guard"]["settings"]["enabled"], false);
    assert_eq!(view["guard"]["settings"]["memory_reserve_percent"], 20);
    assert_eq!(view["guard"]["settings"]["cpu_percent"], 85);
    assert_eq!(view["guard"]["critical_memory_percent"], 10.0);
    assert_eq!(view["builds"]["concurrency"], 2);
    let row: (bool, i16, i16, i32) = sqlx::query_as(
        "SELECT guard_enabled, guard_memory_reserve_percent, guard_cpu_percent, build_concurrency_default FROM panel_settings",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (false, 20, 85, 2));

    let audit: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE action = 'load.settings'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audit, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn cancelling_clears_queued_builds(pool: PgPool) {
    let client = Client::owner(rudder_server::router(state(pool.clone()))).await;
    let (deployment, queue) = queued_build(&pool).await;
    let (status, body) = client
        .call("POST", "/api/v1/load/cancel-builds", None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["queued"], 1);
    assert_eq!(body["running"], 0);
    let queue_status: String = sqlx::query_scalar("SELECT status FROM build_queue WHERE id = $1")
        .bind(queue)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(queue_status, "cancelled");
    let (status, error): (String, Option<String>) =
        sqlx::query_as("SELECT status, error FROM deployments WHERE id = $1")
            .bind(deployment)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(status, "cancelled");
    assert!(error.unwrap().contains("owner@example.com"));
}

#[sqlx::test(migrations = "./migrations")]
async fn viewers_can_look_but_not_act(pool: PgPool) {
    let client = Client::owner(rudder_server::router(state(pool.clone()))).await;
    let (status, _) = client
        .call(
            "POST",
            "/api/v1/users",
            Some(json!({
                "email": "viewer@example.com",
                "password": "viewer-password-ok",
                "role": "viewer"
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED);
    let login = Request::builder()
        .method("POST")
        .uri("/api/v1/login")
        .header("content-type", "application/json")
        .body(Body::from(
            json!({ "email": "viewer@example.com", "password": "viewer-password-ok" }).to_string(),
        ))
        .unwrap();
    let response = client.app.clone().oneshot(login).await.unwrap();
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .and_then(|pair| pair.strip_prefix("rudder_session="))
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let viewer = Client {
        app: client.app.clone(),
        cookie,
        csrf: body["csrf_token"].as_str().unwrap().to_owned(),
    };
    let (status, _) = viewer.call("GET", "/api/v1/load", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = viewer
        .call(
            "POST",
            "/api/v1/load/pause",
            Some(json!({ "paused": true })),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = viewer
        .call("POST", "/api/v1/load/cancel-builds", None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
