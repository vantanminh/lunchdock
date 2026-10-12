//! What a stack action does while it runs: live log, cancel, time limit, rebuild
//! actions and registry logins. A fake `docker` stands in for the engine, so this
//! runs wherever the other API tests run.
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

/// Tests share one `PATH` and one fake `docker`, so they take turns.
static TURN: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static ORIGINAL_PATH: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Install a `docker` that runs `script`; it also appends its argv to `calls.log`.
fn fake_docker(dir: &Path, script: &str) -> PathBuf {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let log = bin.join("calls.log");
    let path = bin.join("docker");
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\necho \"config=$DOCKER_CONFIG\" >> '{}'\n{script}\n",
            log.display(),
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    let old = ORIGINAL_PATH.get_or_init(|| std::env::var("PATH").unwrap_or_default());
    std::env::set_var("PATH", format!("{}:{old}", bin.display()));
    log
}

fn app(pool: PgPool, data_dir: &Path, timeout_secs: Option<u64>) -> axum::Router {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let mut vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_PUBLIC_URL".into(), "https://panel.example".into()),
        ("RUDDER_DATA_DIR".into(), data_dir.display().to_string()),
    ]);
    if let Some(secs) = timeout_secs {
        vars.insert("RUDDER_STACK_TIMEOUT_SECS".into(), secs.to_string());
    }
    let config = rudder_server::Config::from_map(&vars, |_| unreachable!()).unwrap();
    rudder_server::router(rudder_server::AppState::new(pool, config))
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

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (status, body) = self.call(method, path, body).await;
        assert!(status.is_success(), "{method} {path} -> {status} {body}");
        body
    }

    async fn expect(
        &self,
        want: StatusCode,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Value {
        let (status, body) = self.call(method, path, body).await;
        assert_eq!(status, want, "{method} {path}: {body}");
        body
    }

    /// A stack with two image services and one that builds, on a local server.
    async fn stack(&self) -> String {
        let local = self
            .ok(
                "POST",
                "/api/v1/servers",
                Some(json!({ "name": "local", "kind": "local" })),
            )
            .await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let project = self
            .ok("POST", "/api/v1/projects", Some(json!({ "name": "apps" })))
            .await;
        let environment = self
            .ok(
                "POST",
                &format!(
                    "/api/v1/projects/{}/environments",
                    project["id"].as_str().unwrap()
                ),
                Some(json!({ "name": "prod" })),
            )
            .await;
        let created = self
            .expect(
                StatusCode::CREATED,
                "POST",
                &format!(
                    "/api/v1/environments/{}/stacks",
                    environment["id"].as_str().unwrap()
                ),
                Some(json!({
                    "name": "shop", "server_id": local, "source": "inline",
                    "compose_yaml": COMPOSE, "main_service": "web"
                })),
            )
            .await;
        created["service"]["id"].as_str().unwrap().to_owned()
    }

    async fn deployment(&self, service_id: &str) -> Value {
        let list = self
            .ok(
                "GET",
                &format!("/api/v1/services/{service_id}/deployments"),
                None,
            )
            .await;
        list.as_array().unwrap().first().cloned().unwrap()
    }

    async fn wait_until(
        &self,
        service_id: &str,
        what: &str,
        done: impl Fn(&Value) -> bool,
    ) -> Value {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let deployment = self.deployment(service_id).await;
            if done(&deployment) {
                return deployment;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "waited for {what}: {deployment}"
            );
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
}

const COMPOSE: &str = r#"
services:
  web:
    image: ghcr.io/acme/web:1
    environment:
      GREETING: ${GREETING:-hello}
      TOKEN: ${TOKEN}
  api:
    build: ./api
  cache:
    image: redis:7
"#;

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rudder-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[sqlx::test(migrations = "./migrations")]
async fn the_log_grows_while_the_action_runs_and_can_be_cancelled(pool: PgPool) {
    let _turn = TURN.lock().await;
    let dir = temp_dir("stack-live");
    fake_docker(
        &dir,
        r#"echo "step one"; echo "token is hunter22-secret"; sleep 2; echo "step two"; sleep 120"#,
    );
    let client = Client::owner(app(pool, &dir, None)).await;
    let id = client.stack().await;
    client
        .ok(
            "PUT",
            &format!("/api/v1/services/{id}/env"),
            Some(json!({ "key": "TOKEN", "value": "hunter22-secret", "is_build_secret": false })),
        )
        .await;
    let started = client
        .expect(
            StatusCode::ACCEPTED,
            "POST",
            &format!("/api/v1/services/{id}/stack/up"),
            None,
        )
        .await;
    assert_eq!(started["status"], "running");

    // The first lines are visible while the action is still running, secrets masked.
    let live = client
        .wait_until(&id, "live output", |d| {
            d["logs"].as_str().unwrap_or("").contains("step one")
        })
        .await;
    assert_eq!(live["status"], "running");
    let logs = live["logs"].as_str().unwrap();
    assert!(logs.contains("$ docker compose up -d"), "{logs}");
    assert!(logs.contains("token is ***"), "{logs}");
    assert!(!logs.contains("hunter22"), "{logs}");

    // Nothing else can start, and cancelling is allowed to the writer who started it.
    client
        .expect(
            StatusCode::CONFLICT,
            "POST",
            &format!("/api/v1/services/{id}/stack/restart"),
            None,
        )
        .await;
    client
        .expect(
            StatusCode::ACCEPTED,
            "POST",
            &format!("/api/v1/services/{id}/stack/cancel"),
            None,
        )
        .await;
    let cancelled = client
        .wait_until(&id, "cancel", |d| d["status"] != "running")
        .await;
    assert_eq!(cancelled["status"], "cancelled", "{cancelled}");
    assert_eq!(cancelled["error"], "cancelled");
    assert!(cancelled["finished_at"].is_string());
    let stack = client
        .ok("GET", &format!("/api/v1/services/{id}/stack"), None)
        .await;
    assert_eq!(stack["service"]["status"], "stopped");
    // With nothing running there is nothing to cancel, and the stack accepts work again.
    client
        .expect(
            StatusCode::CONFLICT,
            "POST",
            &format!("/api/v1/services/{id}/stack/cancel"),
            None,
        )
        .await;
    client
        .expect(
            StatusCode::ACCEPTED,
            "POST",
            &format!("/api/v1/services/{id}/stack/pull"),
            None,
        )
        .await;
    client
        .expect(
            StatusCode::ACCEPTED,
            "POST",
            &format!("/api/v1/services/{id}/stack/cancel"),
            None,
        )
        .await;
    client
        .wait_until(&id, "the second cancel", |d| d["status"] != "running")
        .await;
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test(migrations = "./migrations")]
async fn an_action_that_runs_too_long_is_stopped(pool: PgPool) {
    let _turn = TURN.lock().await;
    let dir = temp_dir("stack-timeout");
    fake_docker(&dir, r#"echo "building"; sleep 120"#);
    let client = Client::owner(app(pool, &dir, Some(2))).await;
    let id = client.stack().await;
    client
        .ok("POST", &format!("/api/v1/services/{id}/stack/up"), None)
        .await;
    let ended = client
        .wait_until(&id, "the time limit", |d| d["status"] != "running")
        .await;
    assert_eq!(ended["status"], "failed", "{ended}");
    assert!(
        ended["error"].as_str().unwrap().contains("stopped after"),
        "{ended}"
    );
    assert!(ended["logs"].as_str().unwrap().contains("building"));
    let stack = client
        .ok("GET", &format!("/api/v1/services/{id}/stack"), None)
        .await;
    assert_eq!(stack["service"]["status"], "failed");
    let _ = std::fs::remove_dir_all(dir);
}

#[sqlx::test(migrations = "./migrations")]
async fn rebuild_update_and_registry_logins_reach_docker_compose(pool: PgPool) {
    let _turn = TURN.lock().await;
    let dir = temp_dir("stack-flags");
    let calls = fake_docker(
        &dir,
        r#"case " $* " in *" --pull "*) echo "pull access denied for ghcr.io/acme/web, repository does not exist or may require authorization" >&2; exit 1 ;; esac; echo ok"#,
    );
    let client = Client::owner(app(pool, &dir, None)).await;
    let id = client.stack().await;

    let view = client
        .ok("GET", &format!("/api/v1/services/{id}/stack"), None)
        .await;
    let api = view["services"]
        .as_array()
        .unwrap()
        .iter()
        .find(|service| service["name"] == "api")
        .unwrap()
        .clone();
    assert_eq!(api["build"], true);
    assert_eq!(view["build_on_deploy"], false);
    // TOKEN has no fallback, GREETING has one.
    assert_eq!(view["missing_variables"], json!(["TOKEN"]));
    let greeting = view["variable_details"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variable| variable["name"] == "GREETING")
        .unwrap()
        .clone();
    assert_eq!(greeting["has_default"], true);
    assert_eq!(greeting["set"], false);

    // The ghcr.io login saved on the Registries page is used for this stack's images.
    client
        .ok(
            "POST",
            "/api/v1/registries",
            Some(json!({
                "name": "github", "url": "ghcr.io", "username": "octo", "password": "ghp_registrytoken"
            })),
        )
        .await;

    let up = client
        .ok("POST", &format!("/api/v1/services/{id}/stack/up"), None)
        .await;
    let finished = client
        .wait_until(&id, "up", |d| {
            d["id"] == up["id"] && d["status"] != "running"
        })
        .await;
    assert_eq!(finished["status"], "succeeded", "{finished}");

    let rebuild = client
        .ok(
            "POST",
            &format!("/api/v1/services/{id}/stack/rebuild"),
            None,
        )
        .await;
    assert_eq!(rebuild["trigger"], "manual:rebuild");
    let finished = client
        .wait_until(&id, "rebuild", |d| {
            d["id"] == rebuild["id"] && d["status"] != "running"
        })
        .await;
    assert_eq!(finished["status"], "succeeded", "{finished}");
    assert!(finished["logs"]
        .as_str()
        .unwrap()
        .contains("$ docker compose up -d --build"));

    // Deploy builds too once the stack says so.
    client
        .ok(
            "PUT",
            &format!("/api/v1/services/{id}/stack"),
            Some(json!({
                "source": "inline", "compose_yaml": COMPOSE, "main_service": "web",
                "build_on_deploy": true
            })),
        )
        .await;
    let up = client
        .ok("POST", &format!("/api/v1/services/{id}/stack/up"), None)
        .await;
    let finished = client
        .wait_until(&id, "up with build", |d| {
            d["id"] == up["id"] && d["status"] != "running"
        })
        .await;
    assert!(finished["logs"]
        .as_str()
        .unwrap()
        .contains("$ docker compose up -d --build"));

    // "Pull and deploy" is refused by the fake registry; the error says where to add a login.
    let update = client
        .ok("POST", &format!("/api/v1/services/{id}/stack/update"), None)
        .await;
    let failed = client
        .wait_until(&id, "update", |d| {
            d["id"] == update["id"] && d["status"] != "running"
        })
        .await;
    assert_eq!(failed["status"], "failed", "{failed}");
    let error = failed["error"].as_str().unwrap();
    assert!(error.contains("pull access denied"), "{error}");
    assert!(error.contains("Registries"), "{error}");
    assert!(error.contains("ghcr.io"), "{error}");

    let log = std::fs::read_to_string(calls).unwrap();
    assert!(log.contains("up -d --remove-orphans"), "{log}");
    assert!(log.contains("up -d --build --remove-orphans"), "{log}");
    assert!(
        log.contains("up -d --pull always --remove-orphans"),
        "{log}"
    );
    // Every run with images got a Docker config for the saved login, and none was left behind.
    let configs: Vec<&str> = log
        .lines()
        .filter_map(|line| line.strip_prefix("config="))
        .filter(|path| !path.is_empty())
        .collect();
    assert!(!configs.is_empty(), "{log}");
    assert!(configs.iter().all(|path| !Path::new(path).exists()));
    let _ = std::fs::remove_dir_all(dir);
}
