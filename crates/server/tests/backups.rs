//! Databases, S3 destinations, backup jobs, runs, and restores.
//!
//! `api_without_docker` runs everywhere. `docker_backup_round_trip` starts real
//! Postgres, Redis, and S3-compatible (RustFS) containers and only runs with `RUDDER_DOCKER_TESTS=1`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

fn app(pool: PgPool, data_dir: &Path) -> axum::Router {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_PUBLIC_URL".into(), "https://panel.example".into()),
        ("RUDDER_DATA_DIR".into(), data_dir.display().to_string()),
    ]);
    let config = rudder_server::Config::from_map(&vars, |_| unreachable!()).unwrap();
    rudder_server::router(rudder_server::AppState::new(pool, config))
}

struct Reply {
    status: StatusCode,
    body: Value,
    raw: Vec<u8>,
    cookie: Option<String>,
}

struct Client {
    app: axum::Router,
    cookie: String,
    csrf: String,
}

async fn send(
    app: &axum::Router,
    method: &str,
    path: &str,
    session: Option<(&str, &str)>,
    body: Option<Value>,
) -> Reply {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some((cookie, csrf)) = session {
        builder = builder
            .header("cookie", format!("rudder_session={cookie}"))
            .header("x-csrf-token", csrf);
    }
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
        None => builder.body(Body::empty()).unwrap(),
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
    let raw = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let body = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    Reply {
        status,
        body,
        raw,
        cookie,
    }
}

impl Client {
    async fn owner(app: axum::Router) -> Self {
        let created = send(
            &app,
            "POST",
            "/api/v1/setup",
            None,
            Some(json!({
                "email": "owner@example.com",
                "password": "correct-horse-battery",
                "display_name": "Owner"
            })),
        )
        .await;
        assert_eq!(created.status, StatusCode::OK);
        Self {
            cookie: created.cookie.expect("session cookie"),
            csrf: created.body["csrf_token"].as_str().unwrap().to_owned(),
            app,
        }
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> Reply {
        send(
            &self.app,
            method,
            path,
            Some((&self.cookie, &self.csrf)),
            body,
        )
        .await
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let reply = self.call(method, path, body).await;
        assert!(
            reply.status.is_success(),
            "{method} {path} -> {} {}",
            reply.status,
            reply.body
        );
        reply.body
    }

    async fn environment(&self, server_kind: &str) -> (String, String) {
        let server = self
            .ok(
                "POST",
                "/api/v1/servers",
                Some(json!({ "name": server_kind, "kind": server_kind })),
            )
            .await;
        let project = self
            .ok("POST", "/api/v1/projects", Some(json!({ "name": "data" })))
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
        (
            server["id"].as_str().unwrap().to_owned(),
            environment["id"].as_str().unwrap().to_owned(),
        )
    }

    async fn wait_run(&self, run_id: &str, timeout: Duration) -> Value {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let run = self
                .ok("GET", &format!("/api/v1/backup-runs/{run_id}"), None)
                .await;
            let status = run["status"].as_str().unwrap_or_default().to_owned();
            if !matches!(status.as_str(), "queued" | "running") {
                return run;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "run {run_id} still {status}: {}",
                run["logs"]
            );
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rudder-{label}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[sqlx::test(migrations = "./migrations")]
async fn api_without_docker(pool: PgPool) {
    let data_dir = temp_dir("backups");
    let client = Client::owner(app(pool.clone(), &data_dir)).await;

    let engines = client.ok("GET", "/api/v1/database-engines", None).await;
    let names: Vec<&str> = engines
        .as_array()
        .unwrap()
        .iter()
        .map(|engine| engine["engine"].as_str().unwrap())
        .collect();
    for engine in ["postgres", "mysql", "mariadb", "mongo", "redis"] {
        assert!(names.contains(&engine), "{engine} missing from {names:?}");
    }

    let (server_id, environment_id) = client.environment("remote").await;
    let rejected = client
        .call(
            "POST",
            &format!("/api/v1/environments/{environment_id}/databases"),
            Some(json!({
                "name": "pg", "engine": "postgres", "version": "9", "server_id": server_id
            })),
        )
        .await;
    assert_eq!(
        rejected.status,
        StatusCode::BAD_REQUEST,
        "{}",
        rejected.body
    );

    let remote = client
        .call(
            "POST",
            &format!("/api/v1/environments/{environment_id}/databases"),
            Some(json!({
                "name": "pg", "engine": "postgres", "version": "16", "server_id": server_id
            })),
        )
        .await;
    assert_eq!(remote.status, StatusCode::CONFLICT, "{}", remote.body);
    assert_eq!(remote.body["error"]["code"], "remote_unavailable");

    let services = client
        .ok(
            "GET",
            &format!("/api/v1/environments/{environment_id}/services"),
            None,
        )
        .await;
    let service = services
        .as_array()
        .unwrap()
        .iter()
        .find(|service| service["name"] == "pg")
        .expect("database row stored for the remote server")
        .clone();
    let service_id = service["id"].as_str().unwrap().to_owned();
    assert_eq!(service["kind"], "database");

    let connection = client
        .ok(
            "GET",
            &format!("/api/v1/services/{service_id}/connection"),
            None,
        )
        .await;
    let url = connection["internal_url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("postgres://app:"), "{url}");
    let password = connection["password"].as_str().unwrap().to_owned();
    assert_eq!(password.len(), 32);
    assert!(url.contains(&password));

    let env = client
        .call("GET", &format!("/api/v1/services/{service_id}/env"), None)
        .await;
    assert_eq!(env.status, StatusCode::OK);
    let env_text = String::from_utf8_lossy(&env.raw).to_string();
    assert!(env_text.contains("POSTGRES_PASSWORD"));
    assert!(!env_text.contains(&password), "env list leaked a secret");
    let audit: Vec<String> = sqlx::query_scalar("SELECT metadata::TEXT FROM audit_log")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(audit.iter().all(|row| !row.contains(&password)));

    let lifecycle = client
        .call(
            "POST",
            &format!("/api/v1/services/{service_id}/start"),
            None,
        )
        .await;
    assert_eq!(lifecycle.status, StatusCode::CONFLICT);

    let bad_endpoint = client
        .call(
            "POST",
            "/api/v1/s3-destinations",
            Some(json!({
                "name": "bad", "endpoint": "ftp://nope", "bucket": "b",
                "access_key_id": "AKIA", "secret_access_key": "s"
            })),
        )
        .await;
    assert_eq!(bad_endpoint.status, StatusCode::BAD_REQUEST);
    let destination = client
        .ok(
            "POST",
            "/api/v1/s3-destinations",
            Some(json!({
                "name": "minio", "endpoint": "http://127.0.0.1:9", "region": "us-east-1",
                "bucket": "backups", "access_key_id": "AKIAEXAMPLE1234",
                "secret_access_key": "super-secret-key-value", "path_style": true
            })),
        )
        .await;
    let destination_id = destination["id"].as_str().unwrap().to_owned();
    assert!(destination["access_key_hint"]
        .as_str()
        .unwrap()
        .ends_with("1234"));
    let listed = client.call("GET", "/api/v1/s3-destinations", None).await;
    let listed_text = String::from_utf8_lossy(&listed.raw).to_string();
    assert!(!listed_text.contains("super-secret-key-value"));
    assert!(!listed_text.contains("AKIAEXAMPLE1234"));
    let probe = client
        .ok(
            "POST",
            &format!("/api/v1/s3-destinations/{destination_id}/test"),
            None,
        )
        .await;
    assert_eq!(probe["ok"], false);
    assert!(!probe["message"]
        .as_str()
        .unwrap()
        .contains("super-secret-key-value"));

    let job_body = |extra: Value| {
        let mut body = json!({
            "name": "pg-nightly",
            "target_kind": "database",
            "service_id": service_id,
            "destination_ids": [destination_id],
            "schedule_cron": "@daily",
            "timezone": "Europe/Berlin",
            "compression": "zstd",
            "compression_level": 3,
            "retries": 0,
            "retention": { "keep_last": 3 }
        });
        for (key, value) in extra.as_object().unwrap() {
            body[key] = value.clone();
        }
        body
    };
    for (extra, label) in [
        (
            json!({ "tool_options": { "extra_args": ["--file=/etc/passwd"] } }),
            "denied flag",
        ),
        (json!({ "schedule_cron": "61 * * * *" }), "bad cron"),
        (json!({ "timezone": "Mars/Base" }), "bad timezone"),
        (json!({ "compression_level": 40 }), "bad level"),
        (json!({ "path_prefix": "../escape" }), "bad prefix"),
        (json!({ "destination_ids": [] }), "no destination"),
    ] {
        let reply = client
            .call("POST", "/api/v1/backup-jobs", Some(job_body(extra)))
            .await;
        assert_eq!(
            reply.status,
            StatusCode::BAD_REQUEST,
            "{label}: {}",
            reply.body
        );
    }
    let job = client
        .ok(
            "POST",
            "/api/v1/backup-jobs",
            Some(job_body(
                json!({ "encryption_passphrase": "correct horse" }),
            )),
        )
        .await;
    let job_id = job["id"].as_str().unwrap().to_owned();
    assert_eq!(job["encrypted"], true);
    assert!(job["next_run_at"].is_string());
    assert!(!job.to_string().contains("correct horse"));

    let blocked = client
        .call(
            "DELETE",
            &format!("/api/v1/s3-destinations/{destination_id}"),
            None,
        )
        .await;
    assert_eq!(blocked.status, StatusCode::CONFLICT);

    let accepted = client
        .ok("POST", &format!("/api/v1/backup-jobs/{job_id}/run"), None)
        .await;
    let run_id = accepted["run_id"].as_str().unwrap().to_owned();
    let run = client.wait_run(&run_id, Duration::from_secs(30)).await;
    assert_eq!(run["status"], "failed", "{run}");
    assert!(run["error"].as_str().unwrap().contains("agent"));
    let runs = client
        .ok("GET", &format!("/api/v1/backup-jobs/{job_id}/runs"), None)
        .await;
    assert_eq!(runs.as_array().unwrap().len(), 1);
    let events = client.ok("GET", "/api/v1/events", None).await;
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["kind"] == "backup.failed"),
        "{events}"
    );

    let presets = client.ok("GET", "/api/v1/backup-presets", None).await;
    assert!(presets.to_string().contains("@daily"));
    let docs = send(&client.app, "GET", "/api/openapi.json", None, None).await;
    let document = docs.body.to_string();
    for path in [
        "/api/v1/s3-destinations",
        "/api/v1/backup-jobs/{id}/run",
        "/api/v1/backup-runs/{id}/restore",
        "/api/v1/environments/{id}/databases",
    ] {
        assert!(document.contains(path), "{path} missing from OpenAPI");
    }

    client
        .ok("DELETE", &format!("/api/v1/backup-jobs/{job_id}"), None)
        .await;
    client
        .ok(
            "DELETE",
            &format!("/api/v1/s3-destinations/{destination_id}"),
            None,
        )
        .await;
}

fn docker(args: &[&str]) -> String {
    let output = Command::new("docker").args(args).output().unwrap();
    assert!(
        output.status.success(),
        "docker {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args(["rm", "-f", "-v", &self.0])
            .output();
    }
}

/// S3-compatible server for tests. MinIO no longer publishes images or binaries,
/// so this uses RustFS and creates the bucket with rclone.
async fn start_s3() -> (Container, String) {
    let name = format!(
        "rudder-s3-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    docker(&[
        "run",
        "-d",
        "--name",
        &name,
        "-p",
        "127.0.0.1::9000",
        "-e",
        "RUSTFS_ACCESS_KEY=rudderadmin",
        "-e",
        "RUSTFS_SECRET_KEY=rudder-secret-key",
        "rustfs/rustfs:1.0.1",
    ]);
    let guard = Container(name.clone());
    let port = docker(&["port", &name, "9000/tcp"]);
    let port = port
        .lines()
        .next()
        .unwrap()
        .rsplit(':')
        .next()
        .unwrap()
        .to_owned();
    let endpoint = format!("http://127.0.0.1:{port}");
    for _ in 0..60 {
        let made = Command::new("docker")
            .args([
                "run",
                "--rm",
                "--network",
                "host",
                "rclone/rclone:1",
                "mkdir",
                ":s3:backups",
                "--s3-provider",
                "Other",
                "--s3-endpoint",
                &endpoint,
                "--s3-access-key-id",
                "rudderadmin",
                "--s3-secret-access-key",
                "rudder-secret-key",
            ])
            .output()
            .unwrap();
        if made.status.success() {
            return (guard, endpoint);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("S3 test server never accepted the bucket");
}

fn exec_ok(container: &str, args: &[&str]) -> Option<String> {
    let mut argv = vec!["exec", container];
    argv.extend_from_slice(args);
    let output = Command::new("docker").args(&argv).output().unwrap();
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

async fn wait_for(container: &str, args: &[&str]) {
    for _ in 0..120 {
        if exec_ok(container, args).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("{container} never became ready");
}

fn psql(container: &str, sql: &str) -> Option<String> {
    exec_ok(container, &["psql", "-U", "app", "-d", "app", "-tAc", sql])
}

#[sqlx::test(migrations = "./migrations")]
async fn docker_backup_round_trip(pool: PgPool) {
    if std::env::var("RUDDER_DOCKER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping: set RUDDER_DOCKER_TESTS=1 to run Docker backup tests");
        return;
    }
    let data_dir = temp_dir("docker-backups");
    let client = Client::owner(app(pool.clone(), &data_dir)).await;
    let (_s3, endpoint) = start_s3().await;
    let (server_id, environment_id) = client.environment("local").await;

    let destination = client
        .ok(
            "POST",
            "/api/v1/s3-destinations",
            Some(json!({
                "name": "s3-test", "endpoint": endpoint, "region": "us-east-1",
                "bucket": "backups", "access_key_id": "rudderadmin",
                "secret_access_key": "rudder-secret-key", "path_style": true
            })),
        )
        .await;
    let destination_id = destination["id"].as_str().unwrap().to_owned();
    let mut probe = Value::Null;
    for _ in 0..60 {
        probe = client
            .ok(
                "POST",
                &format!("/api/v1/s3-destinations/{destination_id}/test"),
                None,
            )
            .await;
        if probe["ok"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(probe["ok"], true, "{probe}");

    let created = client
        .ok(
            "POST",
            &format!("/api/v1/environments/{environment_id}/databases"),
            Some(json!({
                "name": "pg", "engine": "postgres", "version": "16", "server_id": server_id,
                "memory_bytes": 268435456_i64
            })),
        )
        .await;
    let pg_id = created["id"].as_str().unwrap().to_owned();
    let pg = created["spec"]["container"].as_str().unwrap().to_owned();
    let _pg_guard = Container(pg.clone());
    assert_eq!(created["status"], "running", "{created}");
    wait_for(&pg, &["pg_isready", "-U", "app", "-d", "app"]).await;
    let mut seeded = None;
    for _ in 0..40 {
        seeded = psql(
            &pg,
            "CREATE TABLE notes (id INT PRIMARY KEY, body TEXT); \
             INSERT INTO notes VALUES (1, 'kept'), (2, 'also kept');",
        );
        if seeded.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(seeded.is_some(), "could not seed postgres");

    let logs = client
        .ok(
            "GET",
            &format!("/api/v1/services/{pg_id}/logs?tail=50"),
            None,
        )
        .await;
    let connection = client
        .ok("GET", &format!("/api/v1/services/{pg_id}/connection"), None)
        .await;
    let password = connection["password"].as_str().unwrap();
    assert!(!logs.to_string().contains(password));

    let job = client
        .ok(
            "POST",
            "/api/v1/backup-jobs",
            Some(json!({
                "name": "pg-dump", "target_kind": "database", "service_id": pg_id,
                "destination_ids": [destination_id], "compression": "zstd",
                "compression_level": 5, "encryption_passphrase": "correct horse",
                "verify_after_upload": true, "retries": 0,
                "retention": { "keep_last": 1 }, "path_prefix": "tests/pg"
            })),
        )
        .await;
    let job_id = job["id"].as_str().unwrap().to_owned();
    let first = client
        .ok("POST", &format!("/api/v1/backup-jobs/{job_id}/run"), None)
        .await;
    let first_id = first["run_id"].as_str().unwrap().to_owned();
    let run = client.wait_run(&first_id, Duration::from_secs(120)).await;
    assert_eq!(run["status"], "succeeded", "{}", run["logs"]);
    assert!(run["size_bytes"].as_i64().unwrap() > 0);
    assert_eq!(run["checksum"].as_str().unwrap().len(), 64);
    assert!(run["object_keys"][0]
        .as_str()
        .unwrap()
        .starts_with("tests/pg/"));
    assert!(!run["logs"].as_str().unwrap().contains(password));

    let download = client
        .call(
            "GET",
            &format!("/api/v1/backup-runs/{first_id}/download"),
            None,
        )
        .await;
    assert_eq!(download.status, StatusCode::OK);
    assert_eq!(
        download.raw.len() as i64,
        run["size_bytes"].as_i64().unwrap()
    );
    assert_eq!(&download.raw[..8], b"RDRENC01");

    psql(&pg, "DROP TABLE notes;").expect("drop table");
    let restore = client
        .ok(
            "POST",
            &format!("/api/v1/backup-runs/{first_id}/restore"),
            Some(json!({})),
        )
        .await;
    let restore_run = client
        .wait_run(
            restore["run_id"].as_str().unwrap(),
            Duration::from_secs(120),
        )
        .await;
    assert_eq!(
        restore_run["status"], "succeeded",
        "{}",
        restore_run["logs"]
    );
    assert_eq!(
        psql(&pg, "SELECT string_agg(body, ',' ORDER BY id) FROM notes").as_deref(),
        Some("kept,also kept")
    );

    let second = client
        .ok("POST", &format!("/api/v1/backup-jobs/{job_id}/run"), None)
        .await;
    let second_run = client
        .wait_run(second["run_id"].as_str().unwrap(), Duration::from_secs(120))
        .await;
    assert_eq!(second_run["status"], "succeeded", "{}", second_run["logs"]);
    let expired = client
        .ok("GET", &format!("/api/v1/backup-runs/{first_id}"), None)
        .await;
    assert_eq!(expired["status"], "expired", "{expired}");

    let redis = client
        .ok(
            "POST",
            &format!("/api/v1/environments/{environment_id}/databases"),
            Some(json!({
                "name": "cache", "engine": "redis", "version": "7.4", "server_id": server_id
            })),
        )
        .await;
    let redis_id = redis["id"].as_str().unwrap().to_owned();
    let redis_container = redis["spec"]["container"].as_str().unwrap().to_owned();
    let _redis_guard = Container(redis_container.clone());
    let redis_conn = client
        .ok(
            "GET",
            &format!("/api/v1/services/{redis_id}/connection"),
            None,
        )
        .await;
    let redis_password = redis_conn["password"].as_str().unwrap().to_owned();
    wait_for(
        &redis_container,
        &[
            "redis-cli",
            "-a",
            &redis_password,
            "--no-auth-warning",
            "ping",
        ],
    )
    .await;
    exec_ok(
        &redis_container,
        &[
            "redis-cli",
            "-a",
            &redis_password,
            "--no-auth-warning",
            "set",
            "greeting",
            "hello",
        ],
    )
    .expect("redis set");
    let redis_job = client
        .ok(
            "POST",
            "/api/v1/backup-jobs",
            Some(json!({
                "name": "redis-rdb", "target_kind": "database", "service_id": redis_id,
                "destination_ids": [destination_id], "compression": "gzip", "retries": 0
            })),
        )
        .await;
    let redis_run = client
        .ok(
            "POST",
            &format!(
                "/api/v1/backup-jobs/{}/run",
                redis_job["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    let redis_done = client
        .wait_run(
            redis_run["run_id"].as_str().unwrap(),
            Duration::from_secs(120),
        )
        .await;
    assert_eq!(redis_done["status"], "succeeded", "{}", redis_done["logs"]);
    assert!(redis_done["size_bytes"].as_i64().unwrap() > 0);
    let redis_cli = |args: &[&str]| {
        let mut argv = vec!["redis-cli", "-a", &redis_password, "--no-auth-warning"];
        argv.extend_from_slice(args);
        exec_ok(&redis_container, &argv)
    };
    redis_cli(&["set", "greeting", "changed"]).expect("redis overwrite");
    redis_cli(&["set", "extra", "after-backup"]).expect("redis extra key");
    let redis_restore = client
        .ok(
            "POST",
            &format!(
                "/api/v1/backup-runs/{}/restore",
                redis_run["run_id"].as_str().unwrap()
            ),
            Some(json!({})),
        )
        .await;
    let redis_restored = client
        .wait_run(
            redis_restore["run_id"].as_str().unwrap(),
            Duration::from_secs(120),
        )
        .await;
    assert_eq!(
        redis_restored["status"], "succeeded",
        "{}",
        redis_restored["logs"]
    );
    let mut greeting = None;
    for _ in 0..40 {
        greeting = redis_cli(&["get", "greeting"]);
        if greeting.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(greeting.as_deref(), Some("hello"));
    assert_eq!(redis_cli(&["exists", "extra"]).as_deref(), Some("0"));

    let volume = created["spec"]["volume"].as_str().unwrap().to_owned();
    let volume_job = client
        .ok(
            "POST",
            "/api/v1/backup-jobs",
            Some(json!({
                "name": "pg-volume", "target_kind": "volume", "server_id": server_id,
                "service_id": pg_id, "target_spec": { "volume": volume },
                "destination_ids": [destination_id], "compression": "zstd", "retries": 0,
                "consistency": { "stop_service": true },
                "tool_options": { "exclude_globs": ["**/postmaster.pid"] }
            })),
        )
        .await;
    let volume_run = client
        .ok(
            "POST",
            &format!(
                "/api/v1/backup-jobs/{}/run",
                volume_job["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    let volume_run_id = volume_run["run_id"].as_str().unwrap().to_owned();
    let volume_done = client
        .wait_run(&volume_run_id, Duration::from_secs(180))
        .await;
    assert_eq!(
        volume_done["status"], "succeeded",
        "{}",
        volume_done["logs"]
    );
    let pg_state = client
        .ok("GET", &format!("/api/v1/services/{pg_id}"), None)
        .await;
    assert_eq!(
        pg_state["status"], "running",
        "service restarted after volume backup"
    );

    let restored_volume = format!(
        "rudder-test-restore-{}",
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let volume_restore = client
        .ok(
            "POST",
            &format!("/api/v1/backup-runs/{volume_run_id}/restore"),
            Some(json!({ "target_volume": restored_volume })),
        )
        .await;
    let volume_restore_done = client
        .wait_run(
            volume_restore["run_id"].as_str().unwrap(),
            Duration::from_secs(180),
        )
        .await;
    assert_eq!(
        volume_restore_done["status"], "succeeded",
        "{}",
        volume_restore_done["logs"]
    );
    let listing = docker(&[
        "run",
        "--rm",
        "-v",
        &format!("{restored_volume}:/data:ro"),
        "alpine:3.20",
        "ls",
        "/data",
    ]);
    assert!(listing.contains("PG_VERSION"), "{listing}");
    assert!(!listing.contains("postmaster.pid"), "{listing}");
    let _ = Command::new("docker")
        .args(["volume", "rm", "-f", &restored_volume])
        .output();

    for id in [&pg_id, &redis_id] {
        client
            .ok(
                "DELETE",
                &format!("/api/v1/services/{id}/database?purge_volume=true"),
                None,
            )
            .await;
    }
}

async fn backup_and_restore(
    client: &Client,
    service_id: &str,
    destination_id: &str,
    break_data: impl Fn(),
) {
    let job = client
        .ok(
            "POST",
            "/api/v1/backup-jobs",
            Some(json!({
                "name": format!("job-{}", &service_id[..8]), "target_kind": "database",
                "service_id": service_id, "destination_ids": [destination_id],
                "compression": "gzip", "compression_level": 6, "retries": 0
            })),
        )
        .await;
    let run = client
        .ok(
            "POST",
            &format!("/api/v1/backup-jobs/{}/run", job["id"].as_str().unwrap()),
            None,
        )
        .await;
    let run_id = run["run_id"].as_str().unwrap().to_owned();
    let done = client.wait_run(&run_id, Duration::from_secs(180)).await;
    assert_eq!(done["status"], "succeeded", "{}", done["logs"]);
    break_data();
    let restore = client
        .ok(
            "POST",
            &format!("/api/v1/backup-runs/{run_id}/restore"),
            Some(json!({})),
        )
        .await;
    let restored = client
        .wait_run(
            restore["run_id"].as_str().unwrap(),
            Duration::from_secs(180),
        )
        .await;
    assert_eq!(restored["status"], "succeeded", "{}", restored["logs"]);
}

#[sqlx::test(migrations = "./migrations")]
async fn docker_mariadb_mongo_round_trip(pool: PgPool) {
    if std::env::var("RUDDER_DOCKER_TESTS").as_deref() != Ok("1") {
        eprintln!("skipping: set RUDDER_DOCKER_TESTS=1 to run Docker backup tests");
        return;
    }
    let data_dir = temp_dir("docker-engines");
    let client = Client::owner(app(pool.clone(), &data_dir)).await;
    let (_s3, endpoint) = start_s3().await;
    let (server_id, environment_id) = client.environment("local").await;
    let destination = client
        .ok(
            "POST",
            "/api/v1/s3-destinations",
            Some(json!({
                "name": "s3-test", "endpoint": endpoint, "region": "us-east-1",
                "bucket": "backups", "access_key_id": "rudderadmin",
                "secret_access_key": "rudder-secret-key", "path_style": true
            })),
        )
        .await;
    let destination_id = destination["id"].as_str().unwrap().to_owned();

    let create = |name: &'static str, engine: &'static str, version: &'static str| {
        let client = &client;
        let environment_id = &environment_id;
        let server_id = &server_id;
        async move {
            client
                .ok(
                    "POST",
                    &format!("/api/v1/environments/{environment_id}/databases"),
                    Some(json!({
                        "name": name, "engine": engine, "version": version,
                        "server_id": server_id
                    })),
                )
                .await
        }
    };

    let maria = create("maria", "mariadb", "11.4").await;
    let maria_id = maria["id"].as_str().unwrap().to_owned();
    let maria_container = maria["spec"]["container"].as_str().unwrap().to_owned();
    let _maria_guard = Container(maria_container.clone());
    let maria_conn = client
        .ok(
            "GET",
            &format!("/api/v1/services/{maria_id}/connection"),
            None,
        )
        .await;
    let maria_password = maria_conn["password"].as_str().unwrap().to_owned();
    let mariadb = |sql: &str| {
        let output = Command::new("docker")
            .args([
                "exec",
                "-e",
                &format!("MYSQL_PWD={maria_password}"),
                &maria_container,
                "mariadb",
                "-uapp",
                "app",
                "-N",
                "-e",
                sql,
            ])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let mut ready = false;
    for _ in 0..120 {
        if mariadb(
            "CREATE TABLE IF NOT EXISTS notes (id INT PRIMARY KEY, body TEXT); \
                    REPLACE INTO notes VALUES (1, 'maria');",
        )
        .is_some()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(ready, "mariadb never accepted connections");
    backup_and_restore(&client, &maria_id, &destination_id, || {
        mariadb("DROP TABLE notes;").expect("drop");
    })
    .await;
    assert_eq!(mariadb("SELECT body FROM notes").as_deref(), Some("maria"));

    let mongo = create("docs", "mongo", "7").await;
    let mongo_id = mongo["id"].as_str().unwrap().to_owned();
    let mongo_container = mongo["spec"]["container"].as_str().unwrap().to_owned();
    let _mongo_guard = Container(mongo_container.clone());
    let mongo_conn = client
        .ok(
            "GET",
            &format!("/api/v1/services/{mongo_id}/connection"),
            None,
        )
        .await;
    let mongo_url = mongo_conn["internal_url"].as_str().unwrap().replace(
        &format!("@{}:", mongo_conn["host"].as_str().unwrap()),
        "@127.0.0.1:",
    );
    let mongosh = |script: &str| {
        let output = Command::new("docker")
            .args([
                "exec",
                &mongo_container,
                "mongosh",
                "--quiet",
                &mongo_url,
                "--eval",
                script,
            ])
            .output()
            .unwrap();
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let mut ready = false;
    for _ in 0..120 {
        if mongosh("db.notes.insertOne({ _id: 1, body: 'mongo' })").is_some() {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(ready, "mongo never accepted connections");
    backup_and_restore(&client, &mongo_id, &destination_id, || {
        mongosh("db.notes.drop()").expect("drop");
    })
    .await;
    assert_eq!(
        mongosh("db.notes.findOne({ _id: 1 }).body").as_deref(),
        Some("mongo")
    );

    for id in [&maria_id, &mongo_id] {
        client
            .ok(
                "DELETE",
                &format!("/api/v1/services/{id}/database?purge_volume=true"),
                None,
            )
            .await;
    }
}
