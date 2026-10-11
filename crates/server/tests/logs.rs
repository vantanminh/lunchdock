//! Central logs: ingest, search, export, retention, live tail, and the terminal.
//!
//! The terminal test needs Docker and runs only with `RUDDER_DOCKER_TESTS=1`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tower::ServiceExt;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("rudder-logs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn app(pool: PgPool, data_dir: &Path) -> axum::Router {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_DATA_DIR".into(), data_dir.display().to_string()),
    ]);
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

    async fn raw(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        bearer: Option<&str>,
    ) -> (StatusCode, bytes::Bytes) {
        let mut builder = Request::builder().method(method).uri(path);
        builder = match bearer {
            Some(token) => builder.header("authorization", format!("Bearer {token}")),
            None => builder
                .header("cookie", format!("rudder_session={}", self.cookie))
                .header("x-csrf-token", &self.csrf),
        };
        let request = match body {
            Some(body) => builder
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        (
            status,
            response.into_body().collect().await.unwrap().to_bytes(),
        )
    }

    async fn expect(
        &self,
        status: StatusCode,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Value {
        let (got, bytes) = self.raw(method, path, body, None).await;
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(got, status, "{method} {path}: {value}");
        value
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (got, bytes) = self.raw(method, path, body, None).await;
        let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert!(got.is_success(), "{method} {path}: {got} {value}");
        value
    }

    /// Returns (server id, project id, service id).
    async fn service(&self, kind: &str) -> (String, String, String) {
        let server = self
            .ok(
                "POST",
                "/api/v1/servers",
                Some(json!({ "name": "local", "kind": "local" })),
            )
            .await;
        let project = self
            .ok("POST", "/api/v1/projects", Some(json!({ "name": "apps" })))
            .await;
        let project_id = project["id"].as_str().unwrap().to_owned();
        let environment = self
            .ok(
                "POST",
                &format!("/api/v1/projects/{project_id}/environments"),
                Some(json!({ "name": "prod" })),
            )
            .await;
        let service = self
            .ok(
                "POST",
                &format!(
                    "/api/v1/environments/{}/services",
                    environment["id"].as_str().unwrap()
                ),
                Some(json!({ "name": "web", "kind": kind, "server_id": server["id"] })),
            )
            .await;
        (
            server["id"].as_str().unwrap().to_owned(),
            project_id,
            service["id"].as_str().unwrap().to_owned(),
        )
    }
}

fn entries(page: &Value) -> Vec<Value> {
    page["entries"].as_array().unwrap().clone()
}

#[sqlx::test(migrations = "./migrations")]
async fn ingest_search_export_and_retention_settings(pool: PgPool) {
    let dir = TempDir::new();
    let client = Client::owner(app(pool, dir.path())).await;
    let (server_id, project_id, service_id) = client.service("application").await;

    let accepted = client
        .ok(
            "POST",
            "/api/v1/logs/ingest",
            Some(json!({ "entries": [
                { "source": "app", "service_id": service_id, "server_id": server_id,
                  "message": "ERROR payment failed for order 100% sure" },
                { "source": "proxy", "service_id": service_id,
                  "message": r#"{"RequestMethod":"GET","RequestHost":"a.test","RequestPath":"/x","DownstreamStatus":502}"# },
                { "source": "panel", "level": "warning", "message": "disk almost full",
                  "fields": { "disk": "/" } },
            ]})),
        )
        .await;
    assert_eq!(accepted["accepted"], 3);

    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/logs/ingest",
            Some(json!({ "entries": [{ "source": "kernel", "message": "x" }] })),
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/logs/ingest",
            Some(json!({ "entries": [{ "source": "app", "message": "x",
                "timestamp": "2001-01-01T00:00:00Z" }] })),
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/logs/ingest",
            Some(json!({ "entries": [{ "source": "app", "message": "x",
                "service_id": "00000000-0000-0000-0000-000000000000" }] })),
        )
        .await;

    let token = client
        .ok(
            "POST",
            "/api/v1/tokens",
            Some(json!({ "name": "reader", "scopes": ["read"] })),
        )
        .await;
    let read = token["token"].as_str().unwrap();
    let (status, _) = client
        .raw(
            "POST",
            "/api/v1/logs/ingest",
            Some(json!({ "entries": [{ "source": "app", "message": "x" }] })),
            Some(read),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = client.raw("GET", "/api/v1/logs", None, Some(read)).await;
    assert_eq!(status, StatusCode::OK);

    let all = client.ok("GET", "/api/v1/logs", None).await;
    assert_eq!(entries(&all).len(), 3);
    assert!(all["next_before_id"].is_null());

    let by_service = client
        .ok(
            "GET",
            &format!("/api/v1/logs?service_id={service_id}"),
            None,
        )
        .await;
    assert_eq!(entries(&by_service).len(), 2);
    assert!(entries(&by_service)
        .iter()
        .all(|row| row["project_id"] == project_id.as_str()));

    let errors = client.ok("GET", "/api/v1/logs?level=error", None).await;
    assert_eq!(entries(&errors).len(), 2);
    let warn = client
        .ok("GET", "/api/v1/logs?level=warn&source=panel", None)
        .await;
    assert_eq!(entries(&warn)[0]["fields"]["disk"], "/");

    let five = client.ok("GET", "/api/v1/logs?status=5xx", None).await;
    assert_eq!(entries(&five).len(), 1);
    assert_eq!(entries(&five)[0]["message"], "GET a.test/x 502");
    assert_eq!(entries(&five)[0]["http_status"], 502);

    let percent = client.ok("GET", "/api/v1/logs?q=100%25", None).await;
    assert_eq!(entries(&percent).len(), 1);
    let underscore = client.ok("GET", "/api/v1/logs?q=_", None).await;
    assert_eq!(entries(&underscore).len(), 0);
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "GET",
            "/api/v1/logs?status=6xx",
            None,
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "GET",
            "/api/v1/logs?before_id=4",
            None,
        )
        .await;

    let first = client.ok("GET", "/api/v1/logs?limit=2", None).await;
    assert_eq!(entries(&first).len(), 2);
    let cursor_time = first["next_before_time"].as_str().unwrap();
    let cursor_id = &first["next_before_id"];
    let second = client
        .ok(
            "GET",
            &format!(
                "/api/v1/logs?limit=2&before_time={}&before_id={cursor_id}",
                cursor_time.replace('+', "%2B")
            ),
            None,
        )
        .await;
    assert_eq!(entries(&second).len(), 1);
    let mut ids: Vec<i64> = entries(&first)
        .iter()
        .chain(entries(&second).iter())
        .map(|row| row["id"].as_i64().unwrap())
        .collect();
    ids.dedup();
    assert_eq!(ids.len(), 3);

    let (status, ndjson) = client.raw("GET", "/api/v1/logs/export", None, None).await;
    assert_eq!(status, StatusCode::OK);
    let lines: Vec<Value> = std::str::from_utf8(&ndjson)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 3);
    let (status, csv) = client
        .raw(
            "GET",
            "/api/v1/logs/export?format=csv&source=proxy",
            None,
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let csv = String::from_utf8(csv.to_vec()).unwrap();
    let mut csv_lines = csv.lines();
    assert!(csv_lines
        .next()
        .unwrap()
        .starts_with("logged_at,source,level"));
    assert!(csv_lines.next().unwrap().contains("\"GET a.test/x 502\""));
    assert!(csv_lines.next().is_none());

    let retention = client.ok("GET", "/api/v1/settings/retention", None).await;
    assert_eq!(retention["log_days"]["app"], 14);
    let updated = client
        .ok(
            "PUT",
            "/api/v1/settings/retention",
            Some(json!({ "log_days": { "proxy": 3 } })),
        )
        .await;
    assert_eq!(updated["log_days"]["proxy"], 3);
    assert_eq!(updated["log_days"]["app"], 14);
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "PUT",
            "/api/v1/settings/retention",
            Some(json!({ "log_days": { "proxy": 0 } })),
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "PUT",
            "/api/v1/settings/retention",
            Some(json!({ "log_days": { "kernel": 3 } })),
        )
        .await;
}

#[sqlx::test(migrations = "./migrations")]
async fn retention_deletes_rows_and_drops_old_partitions(pool: PgPool) {
    sqlx::query("SELECT rudder_ensure_daily_partition('log_entries', CURRENT_DATE - 40)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        r#"
        INSERT INTO log_entries (logged_at, source, level, message) VALUES
            (now() - INTERVAL '40 days', 'app', 'info', 'ancient'),
            (now() - INTERVAL '5 days', 'proxy', 'info', 'old proxy'),
            (now() - INTERVAL '5 days', 'app', 'info', 'recent app'),
            (now(), 'proxy', 'info', 'fresh proxy')
        "#,
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(r#"UPDATE retention_settings SET log_days = log_days || '{"proxy": 2}'::jsonb"#)
        .execute(&pool)
        .await
        .unwrap();

    let report = rudder_server::apply_log_retention(&pool).await.unwrap();
    assert!(report.deleted_rows >= 2, "{report:?}");
    assert_eq!(report.dropped_partitions.len(), 1, "{report:?}");
    assert!(report.dropped_partitions[0].starts_with("log_entries_"));

    let left: Vec<String> = sqlx::query_scalar("SELECT message FROM log_entries ORDER BY message")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(left, ["fresh proxy", "recent app"]);
    let future: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class WHERE relname = 'log_entries_' || to_char(CURRENT_DATE + 2, 'YYYYMMDD')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(future, 1);
}

async fn serve(router: axum::Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    addr
}

fn ws_request(
    addr: std::net::SocketAddr,
    path: &str,
    cookie: &str,
    origin: Option<&str>,
) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    let mut request = format!("ws://{addr}{path}").into_client_request().unwrap();
    request.headers_mut().insert(
        "cookie",
        format!("rudder_session={cookie}").parse().unwrap(),
    );
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("origin", origin.parse().unwrap());
    }
    request
}

#[sqlx::test(migrations = "./migrations")]
async fn live_tail_streams_matching_lines(pool: PgPool) {
    let dir = TempDir::new();
    let router = app(pool, dir.path());
    let client = Client::owner(router.clone()).await;
    let addr = serve(router).await;

    let denied = tokio_tungstenite::connect_async(ws_request(
        addr,
        "/api/v1/logs/tail",
        &client.cookie,
        Some("https://evil.test"),
    ))
    .await;
    assert!(denied.is_err(), "cross-origin upgrade must fail");
    let anonymous = format!("ws://{addr}/api/v1/logs/tail")
        .into_client_request()
        .unwrap();
    assert!(tokio_tungstenite::connect_async(anonymous).await.is_err());

    let (mut socket, _) = tokio_tungstenite::connect_async(ws_request(
        addr,
        "/api/v1/logs/tail?source=app&level=error",
        &client.cookie,
        Some(&format!("http://{addr}")),
    ))
    .await
    .unwrap();

    client
        .ok(
            "POST",
            "/api/v1/logs/ingest",
            Some(json!({ "entries": [
                { "source": "panel", "message": "ERROR not for this tail" },
                { "source": "app", "message": "INFO filtered by level" },
                { "source": "app", "message": "ERROR boom" },
            ]})),
        )
        .await;

    let message = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .expect("tail message")
        .unwrap()
        .unwrap();
    let Message::Text(text) = message else {
        panic!("expected text, got {message:?}");
    };
    let value: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["type"], "entry");
    assert_eq!(value["entry"]["message"], "ERROR boom");
    assert!(
        tokio::time::timeout(Duration::from_millis(300), socket.next())
            .await
            .is_err(),
        "only the matching line is sent"
    );
    socket.close(None).await.unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn terminal_runs_a_shell_in_the_container(pool: PgPool) {
    if std::env::var("RUDDER_DOCKER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping: set RUDDER_DOCKER_TESTS=1 to run Docker tests");
        return;
    }
    let dir = TempDir::new();
    let router = app(pool.clone(), dir.path());
    let client = Client::owner(router.clone()).await;
    let (_, _, service_id) = client.service("application").await;

    let name = format!("rudder-term-test-{}", &service_id[..8]);
    let started = std::process::Command::new("docker")
        .args([
            "run",
            "-d",
            "--rm",
            "--name",
            &name,
            "alpine:3.20",
            "sleep",
            "120",
        ])
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    let container_id = String::from_utf8(started.stdout).unwrap().trim().to_owned();
    sqlx::query("UPDATE services SET current_container_id = $1 WHERE id = $2::uuid")
        .bind(&container_id)
        .bind(&service_id)
        .execute(&pool)
        .await
        .unwrap();

    let addr = serve(router).await;
    let (mut socket, _) = tokio_tungstenite::connect_async(ws_request(
        addr,
        &format!("/api/v1/services/{service_id}/terminal?cols=100&rows=30"),
        &client.cookie,
        None,
    ))
    .await
    .unwrap();
    socket
        .send(Message::Text(
            json!({ "type": "resize", "cols": 120, "rows": 40 })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket
        .send(Message::Text(
            json!({ "type": "input", "data": "echo rudder-$((40+2)); stty size\n" })
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let mut output = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !(output.contains("rudder-42") && output.contains("40 120")) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match tokio::time::timeout(remaining, socket.next()).await {
            Ok(Some(Ok(Message::Binary(bytes)))) => {
                output.push_str(&String::from_utf8_lossy(&bytes))
            }
            Ok(Some(Ok(_))) => {}
            other => panic!("terminal ended early: {other:?}; output so far: {output:?}"),
        }
    }
    socket.close(None).await.ok();
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", &name])
        .output();

    let audited: i64 =
        sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE action = 'service.terminal'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(audited, 1);
}

#[sqlx::test(migrations = "./migrations")]
async fn collector_follows_managed_containers_and_redacts_env(pool: PgPool) {
    if std::env::var("RUDDER_DOCKER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping: set RUDDER_DOCKER_TESTS=1 to run Docker tests");
        return;
    }
    let dir = TempDir::new();
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_DATA_DIR".into(), dir.path().display().to_string()),
    ]);
    let config = rudder_server::Config::from_map(&vars, |_| unreachable!()).unwrap();
    let state = rudder_server::AppState::new(pool.clone(), config);
    let client = Client::owner(rudder_server::router(state.clone())).await;
    let (_, _, service_id) = client.service("application").await;
    client
        .ok(
            "PUT",
            &format!("/api/v1/services/{service_id}/env"),
            Some(json!({ "key": "API_KEY", "value": "hunter2-very-secret", "is_build_secret": false })),
        )
        .await;

    let name = format!("rudder-collect-test-{}", &service_id[..8]);
    let started = std::process::Command::new("docker")
        .args([
            "run",
            "-d",
            "--rm",
            "--name",
            &name,
            "--label",
            "rudder.managed=true",
            "--label",
            &format!("rudder.service={service_id}"),
            "alpine:3.20",
            "sh",
            "-c",
            "echo 'ERROR key hunter2-very-secret leaked'; echo hello >&2; sleep 60",
        ])
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");

    rudder_server::start_log_pipeline(&state);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let rows = loop {
        let rows: Vec<(String, String, Value)> = sqlx::query_as(
            "SELECT message, level, fields FROM log_entries WHERE service_id = $1::uuid ORDER BY id",
        )
        .bind(&service_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        if rows.len() >= 2 || tokio::time::Instant::now() > deadline {
            break rows;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", &name])
        .output();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let error = rows
        .iter()
        .find(|row| row.1 == "error")
        .expect("error line");
    assert!(!error.0.contains("hunter2"), "{}", error.0);
    assert!(error.0.contains("***"), "{}", error.0);
    assert_eq!(error.2["stream"], "stdout");
    assert_eq!(error.2["container"], name.as_str());
    let stderr = rows
        .iter()
        .find(|row| row.0 == "hello")
        .expect("stderr line");
    assert_eq!(stderr.2["stream"], "stderr");
}
