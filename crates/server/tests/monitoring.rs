//! Metrics rollups and charts, alerts, notification delivery, and Docker prune.
//!
//! Notification endpoints are in-test HTTP and SMTP servers. The Docker test runs
//! only with `RUDDER_DOCKER_TESTS=1`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, Uri};
use chrono::{DurationRound, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tower::ServiceExt;
use uuid::Uuid;

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("rudder-mon-{}", Uuid::new_v4()));
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

fn state(pool: PgPool, data_dir: &Path) -> rudder_server::AppState {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_DATA_DIR".into(), data_dir.display().to_string()),
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

    async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        bearer: Option<&str>,
    ) -> (StatusCode, Value) {
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
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn expect(
        &self,
        status: StatusCode,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Value {
        let (got, value) = self.call(method, path, body, None).await;
        assert_eq!(got, status, "{method} {path}: {value}");
        value
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (got, value) = self.call(method, path, body, None).await;
        assert!(got.is_success(), "{method} {path}: {got} {value}");
        value
    }

    /// Returns (server id, service id).
    async fn service(&self) -> (Uuid, Uuid) {
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
        let service = self
            .ok(
                "POST",
                &format!(
                    "/api/v1/environments/{}/services",
                    environment["id"].as_str().unwrap()
                ),
                Some(json!({ "name": "web", "kind": "application", "server_id": server["id"] })),
            )
            .await;
        (
            server["id"].as_str().unwrap().parse().unwrap(),
            service["id"].as_str().unwrap().parse().unwrap(),
        )
    }
}

fn sample(
    server: Uuid,
    service: Option<Uuid>,
    metric: &'static str,
    value: f64,
) -> rudder_server::Sample {
    rudder_server::Sample {
        server_id: server,
        service_id: service,
        source: if service.is_some() {
            "service"
        } else {
            "server"
        },
        metric,
        value,
    }
}

fn values(view: &Value, metric: &str) -> Vec<f64> {
    view["series"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["metric"] == metric)
        .unwrap()["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["avg"].as_f64().unwrap())
        .collect()
}

#[sqlx::test(migrations = "./migrations")]
async fn samples_roll_up_and_chart(pool: PgPool) {
    let dir = TempDir::new();
    let client = Client::owner(rudder_server::router(state(pool.clone(), dir.path()))).await;
    let (server, service) = client.service().await;

    let minute = Utc::now()
        .duration_trunc(chrono::Duration::minutes(1))
        .unwrap();
    let three_ago = minute - chrono::Duration::minutes(3);
    for (offset, cpu) in [(0, 10.0), (10, 20.0), (20, 30.0)] {
        let at = three_ago + chrono::Duration::seconds(offset);
        rudder_server::insert_samples(
            &pool,
            at,
            &[
                sample(server, None, "cpu_percent", cpu),
                sample(server, Some(service), "memory_bytes", 1000.0 * cpu),
            ],
        )
        .await
        .unwrap();
    }
    rudder_server::insert_samples(
        &pool,
        Utc::now(),
        &[sample(server, None, "cpu_percent", 55.0)],
    )
    .await
    .unwrap();
    sqlx::query(
        r#"
        INSERT INTO log_entries (logged_at, server_id, service_id, source, level, message, http_status)
        SELECT $1 + make_interval(secs => g), $2, $3, 'proxy', 'info', 'GET /', CASE WHEN g < 2 THEN 502 ELSE 200 END
        FROM generate_series(0, 4) g
        "#,
    )
    .bind(three_ago)
    .bind(server)
    .bind(service)
    .execute(&pool)
    .await
    .unwrap();

    let first = rudder_server::rollup_metrics(&pool, Utc::now())
        .await
        .unwrap();
    assert!(first.m1 >= 4, "{first:?}");
    let again = rudder_server::rollup_metrics(&pool, Utc::now())
        .await
        .unwrap();
    assert_eq!(again.m1, 0, "watermark prevents double counting");

    let row: (f64, f64, f64, i32) = sqlx::query_as(
        "SELECT avg_value, min_value, max_value, sample_count FROM metrics_rollups WHERE resolution = '1m' AND metric = 'cpu_percent' AND bucket_start = $1",
    )
    .bind(three_ago)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row, (20.0, 10.0, 30.0, 3));
    let requests: Vec<(String, f64)> = sqlx::query_as(
        "SELECT metric, avg_value FROM metrics_rollups WHERE source = 'proxy' AND resolution = '1m' ORDER BY metric",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        requests,
        [("http_5xx".into(), 2.0), ("requests".into(), 5.0)]
    );

    let raw = client
        .ok(
            "GET",
            &format!("/api/v1/metrics?server_id={server}&metrics=cpu_percent&range=1h"),
            None,
        )
        .await;
    assert_eq!(raw["resolution"], "raw");
    assert_eq!(values(&raw, "cpu_percent"), [10.0, 20.0, 30.0, 55.0]);
    let six = client
        .ok(
            "GET",
            &format!("/api/v1/metrics?service_id={service}&metrics=memory_bytes,requests,http_5xx&range=6h"),
            None,
        )
        .await;
    assert_eq!(six["resolution"], "1m");
    assert_eq!(values(&six, "memory_bytes"), [20_000.0]);
    assert_eq!(values(&six, "requests"), [5.0]);
    let fifteen = client
        .ok(
            "GET",
            &format!("/api/v1/metrics?service_id={service}&metrics=requests&range=15m"),
            None,
        )
        .await;
    assert_eq!(
        values(&fifteen, "requests"),
        [5.0],
        "proxy counts use 1m even for short ranges"
    );
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "GET",
            "/api/v1/metrics?metrics=cpu_percent",
            None,
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "GET",
            &format!("/api/v1/metrics?server_id={server}&metrics=cpu_percent&range=2y"),
            None,
        )
        .await;

    let overview = client.ok("GET", "/api/v1/metrics/overview", None).await;
    assert_eq!(overview["servers"][0]["cpu_percent"], 55.0);

    sqlx::query("SELECT rudder_ensure_daily_partition('metrics_samples', CURRENT_DATE - 5)")
        .execute(&pool)
        .await
        .unwrap();
    rudder_server::insert_samples(
        &pool,
        Utc::now() - chrono::Duration::days(5),
        &[sample(server, None, "cpu_percent", 1.0)],
    )
    .await
    .unwrap();
    let report = rudder_server::apply_metrics_retention(&pool).await.unwrap();
    assert!(report.deleted_rows >= 1, "{report:?}");
    assert!(
        report
            .dropped_partitions
            .iter()
            .any(|p| p.starts_with("metrics_samples_")),
        "{report:?}"
    );
}

#[derive(Clone, Default)]
struct Inbox {
    requests: Arc<Mutex<Vec<(String, HeaderMap, Bytes)>>>,
}

async fn receive(
    State(inbox): State<Inbox>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> StatusCode {
    inbox
        .requests
        .lock()
        .unwrap()
        .push((uri.path().to_owned(), headers, body));
    if uri.path().contains("broken") {
        StatusCode::INTERNAL_SERVER_ERROR
    } else {
        StatusCode::OK
    }
}

async fn serve_inbox() -> (String, Inbox) {
    let inbox = Inbox::default();
    let app = axum::Router::new()
        .fallback(axum::routing::post(receive))
        .with_state(inbox.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}"), inbox)
}

/// Accepts one message at a time and records each DATA section.
async fn serve_smtp() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let messages = Arc::new(Mutex::new(Vec::new()));
    let store = messages.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let store = store.clone();
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
                let mut data: Option<String> = None;
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(buffer) = data.as_mut() {
                        if line == "." {
                            store.lock().unwrap().push(data.take().unwrap());
                            write.write_all(b"250 queued\r\n").await.unwrap();
                        } else {
                            buffer.push_str(&line);
                            buffer.push('\n');
                        }
                        continue;
                    }
                    let upper = line.to_ascii_uppercase();
                    let reply: &[u8] = if upper.starts_with("EHLO") {
                        b"250-fake\r\n250 8BITMIME\r\n"
                    } else if upper.starts_with("DATA") {
                        data = Some(String::new());
                        b"354 go\r\n"
                    } else if upper.starts_with("QUIT") {
                        write.write_all(b"221 bye\r\n").await.unwrap();
                        return;
                    } else {
                        b"250 ok\r\n"
                    };
                    write.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, messages)
}

#[sqlx::test(migrations = "./migrations")]
async fn alerts_fire_resolve_and_notify_every_channel_kind(pool: PgPool) {
    let dir = TempDir::new();
    let app_state = state(pool.clone(), dir.path());
    let client = Client::owner(rudder_server::router(app_state.clone())).await;
    let (server, service) = client.service().await;
    let (base, inbox) = serve_inbox().await;
    let (smtp_port, mail) = serve_smtp().await;

    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/notification-channels",
            Some(json!({ "name": "bad", "config": { "kind": "discord", "webhook_url": "http://insecure.test/x" } })),
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/notification-channels",
            Some(json!({ "name": "bad", "config": { "kind": "pager" } })),
        )
        .await;

    let token = "123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_secret";
    let telegram = client
        .ok(
            "POST",
            "/api/v1/notification-channels",
            Some(json!({ "name": "tg", "config": {
                "kind": "telegram", "bot_token": token, "chat_id": "-100200", "api_url": base
            }})),
        )
        .await;
    let webhook = client
        .ok(
            "POST",
            "/api/v1/notification-channels",
            Some(json!({ "name": "hook", "config": {
                "kind": "webhook", "url": format!("{base}/hook"), "secret": "s3cret-value"
            }})),
        )
        .await;
    let smtp = client
        .ok(
            "POST",
            "/api/v1/notification-channels",
            Some(json!({ "name": "mail", "config": {
                "kind": "smtp", "host": "127.0.0.1", "port": smtp_port, "security": "none",
                "from": "Rudder <rudder@example.com>", "to": ["ops@example.com"]
            }})),
        )
        .await;
    let broken = client
        .ok(
            "POST",
            "/api/v1/notification-channels",
            Some(json!({ "name": "broken", "config": { "kind": "webhook", "url": format!("{base}/broken") } })),
        )
        .await;

    let listed = client
        .ok("GET", "/api/v1/notification-channels", None)
        .await;
    let listed_text = listed.to_string();
    assert!(!listed_text.contains("ABCDEFGHIJ"), "{listed_text}");
    assert!(!listed_text.contains("s3cret"), "{listed_text}");
    assert!(listed_text.contains("chat -100200"));

    let renamed = client
        .ok(
            "PUT",
            &format!(
                "/api/v1/notification-channels/{}",
                telegram["id"].as_str().unwrap()
            ),
            Some(json!({ "name": "telegram", "config": {
                "kind": "telegram", "bot_token": "", "chat_id": "-100200", "api_url": base
            }})),
        )
        .await;
    assert_eq!(renamed["name"], "telegram");

    let tested = client
        .ok(
            "POST",
            &format!(
                "/api/v1/notification-channels/{}/test",
                telegram["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    assert_eq!(tested["ok"], true, "{tested}");
    {
        let requests = inbox.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].0,
            format!("/bot{token}/sendMessage"),
            "secret kept on update"
        );
        let body: Value = serde_json::from_slice(&requests[0].2).unwrap();
        assert_eq!(body["chat_id"], "-100200");
    }

    let read_token = client
        .ok(
            "POST",
            "/api/v1/tokens",
            Some(json!({ "name": "ro", "scopes": ["read"] })),
        )
        .await;
    let read = read_token["token"].as_str().unwrap();
    let (status, _) = client
        .call(
            "POST",
            "/api/v1/alerts",
            Some(
                json!({ "name": "x", "kind": "server_cpu", "target_spec": { "server_id": server },
                "threshold": 50, "channel_ids": [] }),
            ),
            Some(read),
        )
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/alerts",
            Some(json!({ "name": "cpu", "kind": "server_cpu", "target_spec": {}, "threshold": 50, "channel_ids": [] })),
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/alerts",
            Some(
                json!({ "name": "cpu", "kind": "server_cpu", "target_spec": { "server_id": server },
                "threshold": 150, "channel_ids": [] }),
            ),
        )
        .await;
    let all_channels = json!([telegram["id"], webhook["id"], smtp["id"], broken["id"]]);
    let alert = client
        .ok(
            "POST",
            "/api/v1/alerts",
            Some(
                json!({ "name": "cpu", "kind": "server_cpu", "target_spec": { "server_id": server },
                "threshold": 50, "duration_secs": 0, "channel_ids": all_channels }),
            ),
        )
        .await;
    assert_eq!(alert["state"], "ok");
    client
        .ok(
            "POST",
            "/api/v1/alerts",
            Some(json!({ "name": "backups", "kind": "event",
                "target_spec": { "events": ["backup.*"], "min_severity": "warning" },
                "channel_ids": [webhook["id"]] })),
        )
        .await;
    client
        .ok(
            "POST",
            "/api/v1/alerts",
            Some(json!({ "name": "web down", "kind": "service_down",
                "target_spec": { "service_id": service }, "channel_ids": [] })),
        )
        .await;

    // Events that existed before any matching rule are marked delivered without sending.
    rudder_server::deliver_notifications(&pool, &app_state.cipher)
        .await
        .unwrap();
    inbox.requests.lock().unwrap().clear();

    rudder_server::insert_samples(
        &pool,
        Utc::now() - chrono::Duration::seconds(5),
        &[sample(server, None, "cpu_percent", 90.0)],
    )
    .await
    .unwrap();
    rudder_server::evaluate_alerts(&pool, Utc::now())
        .await
        .unwrap();
    let alerts = client.ok("GET", "/api/v1/alerts", None).await;
    let cpu = alerts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "cpu")
        .unwrap();
    assert_eq!(cpu["state"], "firing");
    assert_eq!(cpu["last_value"], 90.0);
    let down = alerts
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "web down")
        .unwrap();
    assert_eq!(down["state"], "ok", "service not expected to run");

    rudder_server::deliver_notifications(&pool, &app_state.cipher)
        .await
        .unwrap();
    {
        let requests = inbox.requests.lock().unwrap();
        let telegram_body: Value = requests
            .iter()
            .find(|(path, _, _)| path.ends_with("/sendMessage"))
            .map(|(_, _, body)| serde_json::from_slice(body).unwrap())
            .expect("telegram delivery");
        assert!(telegram_body["text"]
            .as_str()
            .unwrap()
            .starts_with("[CRITICAL] Alert firing: cpu"));
        let (_, headers, body) = requests
            .iter()
            .find(|(path, _, _)| path == "/hook")
            .expect("webhook delivery");
        let expected = rudder_server::sign_webhook("s3cret-value", body).unwrap();
        assert_eq!(headers["x-rudder-signature"], expected.as_str());
        assert_eq!(headers["x-rudder-event"], "alert.fired");
    }
    let messages = mail.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0].contains("Subject: Rudder: Alert firing: cpu"),
        "{}",
        messages[0]
    );

    let pending: (Option<chrono::DateTime<Utc>>, i32, Option<String>) = sqlx::query_as(
        "SELECT delivered_at, delivery_attempts, delivery_error FROM system_events WHERE kind = 'alert.fired'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(pending.0.is_none(), "the broken channel keeps it pending");
    assert_eq!(pending.1, 1);
    assert!(pending.2.unwrap().contains("500"));
    inbox.requests.lock().unwrap().clear();
    rudder_server::deliver_notifications(&pool, &app_state.cipher)
        .await
        .unwrap();
    {
        let requests = inbox.requests.lock().unwrap();
        assert!(
            requests.iter().all(|(path, _, _)| path == "/broken"),
            "retries only go to channels that failed"
        );
    }

    sqlx::query("DELETE FROM metrics_samples")
        .execute(&pool)
        .await
        .unwrap();
    rudder_server::insert_samples(
        &pool,
        Utc::now(),
        &[sample(server, None, "cpu_percent", 5.0)],
    )
    .await
    .unwrap();
    rudder_server::evaluate_alerts(&pool, Utc::now())
        .await
        .unwrap();
    let resolved: i64 =
        sqlx::query_scalar("SELECT count(*) FROM system_events WHERE kind = 'alert.resolved'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(resolved, 1);

    inbox.requests.lock().unwrap().clear();
    sqlx::query(
        "INSERT INTO system_events (kind, severity, title) VALUES ('backup.failed', 'critical', 'Backup failed'), ('backup.succeeded', 'info', 'Backup ok')",
    )
    .execute(&pool)
    .await
    .unwrap();
    rudder_server::deliver_notifications(&pool, &app_state.cipher)
        .await
        .unwrap();
    let hook_events: Vec<String> = inbox
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|(path, _, _)| path == "/hook")
        .map(|(_, headers, _)| headers["x-rudder-event"].to_str().unwrap().to_owned())
        .collect();
    assert!(
        hook_events.contains(&"backup.failed".to_owned()),
        "{hook_events:?}"
    );
    assert!(
        !hook_events.contains(&"backup.succeeded".to_owned()),
        "below min severity"
    );

    client
        .ok(
            "DELETE",
            &format!(
                "/api/v1/notification-channels/{}",
                broken["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    let ids: Vec<Uuid> = sqlx::query_scalar("SELECT channel_ids FROM alerts WHERE name = 'cpu'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(ids.len(), 3, "deleted channel removed from alerts");
}

#[sqlx::test(migrations = "./migrations")]
async fn monitoring_samples_real_containers_and_prunes(pool: PgPool) {
    if std::env::var("RUDDER_DOCKER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping: set RUDDER_DOCKER_TESTS=1 to run Docker tests");
        return;
    }
    let dir = TempDir::new();
    let app_state = state(pool.clone(), dir.path());
    let client = Client::owner(rudder_server::router(app_state.clone())).await;
    let (server, service) = client.service().await;
    let name = format!("rudder-mon-test-{}", &service.to_string()[..8]);
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
            &format!("rudder.service={service}"),
            "alpine:3.20",
            "sh",
            "-c",
            "while true; do :; done",
        ])
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");

    rudder_server::start_monitoring(&app_state);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let (server_metrics, service_cpu) = loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let server_metrics: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT metric FROM metrics_samples WHERE server_id = $1 AND service_id IS NULL ORDER BY metric",
        )
        .bind(server)
        .fetch_all(&pool)
        .await
        .unwrap();
        let cpu: Option<f64> = sqlx::query_scalar(
            "SELECT max(value) FROM metrics_samples WHERE service_id = $1 AND metric = 'cpu_percent'",
        )
        .bind(service)
        .fetch_one(&pool)
        .await
        .unwrap();
        if (server_metrics.len() >= 8 && cpu.is_some()) || tokio::time::Instant::now() > deadline {
            break (server_metrics, cpu);
        }
    };
    let _ = std::process::Command::new("docker")
        .args(["rm", "-f", &name])
        .output();
    assert!(
        server_metrics.contains(&"net_rx_bps".to_owned()),
        "{server_metrics:?}"
    );
    assert!(
        server_metrics.contains(&"disk_percent".to_owned()),
        "{server_metrics:?}"
    );
    let cpu = service_cpu.expect("service cpu sampled");
    assert!(cpu > 1.0, "busy loop should use CPU, got {cpu}");

    let pruned = client
        .ok("POST", &format!("/api/v1/servers/{server}/prune"), None)
        .await;
    assert!(pruned["reclaimed_bytes"].is_u64());
    let updated = client
        .ok(
            "PATCH",
            &format!("/api/v1/servers/{server}"),
            Some(json!({ "auto_prune": true })),
        )
        .await;
    assert_eq!(updated["auto_prune"], true);
    assert!(updated["last_pruned_at"].is_string());
}
