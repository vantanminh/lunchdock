//! The `rudder` CLI against a real panel listener: login, services, deploy, logs, and cache purge.
//!
//! Docker is not available to the panel here, so deploys fail; the test checks that the
//! CLI waits for them and reports the failure. The cache admin API is a small in-test server.

use std::collections::HashMap;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use sqlx::PgPool;

#[derive(Clone, Default)]
struct Output(Arc<Mutex<Vec<u8>>>);

impl Output {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}

impl Write for Output {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct Panel {
    base: String,
    http: reqwest::Client,
    cookie: String,
    csrf: String,
    _dir: std::path::PathBuf,
}

impl Panel {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut request = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .header("cookie", format!("rudder_session={}", self.cookie))
            .header("x-csrf-token", &self.csrf);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }

    async fn ok(&self, method: &str, path: &str, body: Value) -> Value {
        let (status, value) = self.call(method, path, Some(body)).await;
        assert!(status < 300, "{method} {path}: {status} {value}");
        value
    }

    async fn token(&self, scopes: &[&str]) -> String {
        let created = self
            .ok(
                "POST",
                "/api/v1/tokens",
                json!({ "name": format!("cli {}", scopes.join(" ")), "scopes": scopes }),
            )
            .await;
        created["token"].as_str().unwrap().to_owned()
    }
}

fn docker_tests() -> bool {
    std::env::var("RUDDER_DOCKER_TESTS").as_deref() == Ok("1")
}

async fn start(pool: PgPool, cache_admin: &str) -> Panel {
    let dir = std::env::temp_dir().join(format!("rudder-cli-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let mut vars = HashMap::from([
        ("DATABASE_URL".to_owned(), url),
        ("RUDDER_MASTER_KEY".to_owned(), "ee".repeat(32)),
        ("RUDDER_COOKIE_SECURE".to_owned(), "false".to_owned()),
        ("RUDDER_DATA_DIR".to_owned(), dir.display().to_string()),
        ("RUDDER_CACHE_ADMIN_URL".to_owned(), cache_admin.to_owned()),
        ("RUDDER_UPDATE_CHECK_SECS".to_owned(), "0".to_owned()),
    ]);
    if !docker_tests() {
        vars.insert(
            "RUDDER_DOCKER_SOCKET".to_owned(),
            dir.join("no-docker.sock").display().to_string(),
        );
    }
    let config = rudder_server::Config::from_map(&vars, |_| unreachable!()).unwrap();
    let app = rudder_server::router(rudder_server::AppState::new(pool, config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let http = reqwest::Client::new();
    let response = http
        .post(format!("{base}/api/v1/setup"))
        .json(&json!({
            "email": "owner@example.com",
            "password": "correct-horse-battery",
            "display_name": "Owner"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .and_then(|v| v.strip_prefix("rudder_session="))
        .unwrap()
        .to_owned();
    let body: Value = response.json().await.unwrap();
    Panel {
        base,
        http,
        cookie,
        csrf: body["csrf_token"].as_str().unwrap().to_owned(),
        _dir: dir,
    }
}

fn context(
    config: &std::path::Path,
    token: Option<String>,
    url: Option<String>,
) -> rudder_cli::Context {
    rudder_cli::Context {
        config_path: config.to_path_buf(),
        url,
        token,
        read_token: Box::new(|| Err(std::io::Error::other("no stdin in tests"))),
        poll: Duration::from_millis(100),
    }
}

async fn cli(ctx: &rudder_cli::Context, args: &[&str]) -> Result<String, String> {
    let mut out = Output::default();
    let result = rudder_cli::run(args.iter().map(|a| a.to_string()).collect(), ctx, &mut out).await;
    match result {
        Ok(()) => Ok(out.text()),
        Err(error) => Err(format!("{error:#}")),
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn cli_logs_in_deploys_reads_logs_and_purges(pool: PgPool) {
    let purges: Arc<Mutex<Vec<Value>>> = Arc::default();
    let seen = purges.clone();
    let admin = axum::Router::new().route(
        "/purge",
        axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
            let seen = seen.clone();
            async move {
                seen.lock().unwrap().push(body);
                axum::Json(json!({ "removed": 3 }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, admin).await.unwrap() });

    let panel = start(pool, &admin_url).await;
    let write_token = panel.token(&["read", "write"]).await;
    let read_token = panel.token(&["read"]).await;

    let server = panel
        .ok(
            "POST",
            "/api/v1/servers",
            json!({ "name": "local", "kind": "local" }),
        )
        .await;
    let project = panel
        .ok("POST", "/api/v1/projects", json!({ "name": "shop" }))
        .await;
    let environment = panel
        .ok(
            "POST",
            &format!(
                "/api/v1/projects/{}/environments",
                project["id"].as_str().unwrap()
            ),
            json!({ "name": "prod" }),
        )
        .await;
    let env_id = environment["id"].as_str().unwrap();
    let web = panel
        .ok(
            "POST",
            &format!("/api/v1/environments/{env_id}/services"),
            json!({ "name": "web", "kind": "application", "server_id": server["id"], "spec": { "image": "nginx:1.27-alpine" } }),
        )
        .await;
    let web_id = web["id"].as_str().unwrap().to_owned();
    let blog = panel
        .ok(
            "POST",
            &format!("/api/v1/environments/{env_id}/stacks"),
            json!({ "name": "blog", "server_id": server["id"], "source": "inline", "compose_yaml": "services:\n  web:\n    image: nginx:1.27-alpine\n" }),
        )
        .await;
    let blog_id = blog["service"]["id"].as_str().unwrap().to_owned();

    let dir = std::env::temp_dir().join(format!("rudder-cli-config-{}", uuid::Uuid::new_v4()));
    let config = dir.join("config.json");

    let anonymous = context(&config, None, None);
    let error = cli(&anonymous, &["services"]).await.unwrap_err();
    assert!(error.contains("not logged in"), "{error}");
    let error = cli(
        &context(&config, Some(write_token.clone()), None),
        &["login"],
    )
    .await
    .unwrap_err();
    assert!(error.contains("--url"), "{error}");
    let error = cli(
        &context(&config, Some("rdr_wrong".into()), None),
        &["login", "--url", &panel.base],
    )
    .await
    .unwrap_err();
    assert!(error.contains("rejected the token"), "{error}");
    assert!(!config.exists());
    let error = cli(
        &anonymous,
        &["login", "--url", &panel.base, "--token", "rdr_x"],
    )
    .await
    .unwrap_err();
    assert!(error.contains("RUDDER_TOKEN"), "{error}");

    let login = cli(
        &context(&config, Some(write_token.clone()), None),
        &["login", "--url", &format!("{}/", panel.base)],
    )
    .await
    .unwrap();
    assert!(login.contains("as owner@example.com (admin)"), "{login}");
    assert!(!login.contains(&write_token));
    let saved: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(saved["url"], panel.base.as_str());
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&config).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    let saved_ctx = context(&config, None, None);
    let listing = cli(&saved_ctx, &["services"]).await.unwrap();
    assert!(listing.contains("shop/prod/web"), "{listing}");
    assert!(listing.contains("shop/prod/blog"), "{listing}");
    assert!(listing.contains(&web_id));

    let error = cli(&saved_ctx, &["deploy", "missing"]).await.unwrap_err();
    assert!(error.contains("no service matches"), "{error}");
    let error = cli(
        &context(&config, Some(read_token.clone()), None),
        &["deploy", "shop/web"],
    )
    .await
    .unwrap_err();
    assert!(error.contains("not allowed"), "{error}");
    if docker_tests() {
        let deployed = cli(&saved_ctx, &["deploy", "shop/web"]).await.unwrap();
        assert!(
            deployed.contains("Deploying shop/prod/web (nginx:1.27-alpine)"),
            "{deployed}"
        );
        assert!(deployed.contains("succeeded"), "{deployed}");
        let stack = cli(
            &saved_ctx,
            &["deploy", "shop/prod/blog", "--timeout", "120"],
        )
        .await
        .unwrap();
        assert!(stack.contains("Starting stack shop/prod/blog"), "{stack}");
        assert!(stack.contains("succeeded"), "{stack}");
    } else {
        let error = cli(&saved_ctx, &["deploy", "shop/web"]).await.unwrap_err();
        assert!(error.contains("docker"), "{error}");
        let error = cli(&saved_ctx, &["deploy", "shop/prod/blog"])
            .await
            .unwrap_err();
        assert!(error.contains("docker"), "{error}");
    }
    let error = cli(
        &saved_ctx,
        &["deploy", "shop/web", "--image", "Not An Image!"],
    )
    .await
    .unwrap_err();
    assert!(!error.is_empty());
    let (_, deployments) = panel
        .call(
            "GET",
            &format!("/api/v1/services/{web_id}/deployments"),
            None,
        )
        .await;
    assert!(!deployments.as_array().unwrap().is_empty());

    let ingest = |message: &str, level: &str| json!({ "entries": [{ "source": "app", "level": level, "message": message, "service_id": web_id }] });
    panel
        .ok("POST", "/api/v1/logs/ingest", ingest("GET / 200", "info"))
        .await;
    panel
        .ok(
            "POST",
            "/api/v1/logs/ingest",
            ingest("database timeout", "error"),
        )
        .await;
    let mut lines = String::new();
    for _ in 0..50 {
        lines = cli(&saved_ctx, &["logs", "web", "-n", "10"]).await.unwrap();
        if lines.contains("database timeout") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let get = lines.find("GET / 200").expect(&lines);
    let timeout = lines.find("database timeout").expect(&lines);
    assert!(get < timeout, "oldest first: {lines}");
    assert!(lines.contains("ERROR [app] database timeout"), "{lines}");
    let errors = cli(&saved_ctx, &["logs", "web", "--level", "error"])
        .await
        .unwrap();
    assert!(
        errors.contains("database timeout") && !errors.contains("GET /"),
        "{errors}"
    );
    let error = cli(&saved_ctx, &["logs", "web", "--since", "soon"])
        .await
        .unwrap_err();
    assert!(error.contains("--since"), "{error}");

    let follow_out = Output::default();
    let follow = {
        let config = config.clone();
        let mut out = follow_out.clone();
        tokio::spawn(async move {
            let ctx = context(&config, None, None);
            let _ = rudder_cli::run(
                vec![
                    "logs".into(),
                    "web".into(),
                    "-f".into(),
                    "-n".into(),
                    "1".into(),
                ],
                &ctx,
                &mut out,
            )
            .await;
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    panel
        .ok(
            "POST",
            "/api/v1/logs/ingest",
            ingest("followed line", "warn"),
        )
        .await;
    let mut followed = false;
    for _ in 0..50 {
        if follow_out.text().contains("WARN  [app] followed line") {
            followed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    follow.abort();
    assert!(followed, "{}", follow_out.text());
    assert_eq!(follow_out.text().matches("database timeout").count(), 1);

    let domain = panel
        .ok(
            "POST",
            &format!("/api/v1/services/{web_id}/domains"),
            json!({ "host": "shop.example.com", "target_port": 80, "ssl_mode": "none" }),
        )
        .await;
    let policy = panel
        .ok(
            "POST",
            "/api/v1/cache-policies",
            json!({ "name": "Static", "preset": "static-assets" }),
        )
        .await;
    panel
        .ok(
            "PUT",
            &format!("/api/v1/domains/{}/cache", domain["id"].as_str().unwrap()),
            json!({ "enabled": true, "policy_id": policy["id"] }),
        )
        .await;
    let error = cli(&saved_ctx, &["cache", "purge", "web"])
        .await
        .unwrap_err();
    assert!(error.contains("--all"), "{error}");
    let error = cli(
        &context(&config, Some(read_token), None),
        &["cache", "purge", "web", "--all"],
    )
    .await
    .unwrap_err();
    assert!(error.contains("not allowed"), "{error}");
    let purged = cli(
        &saved_ctx,
        &[
            "cache", "purge", "web", "--path", "/a.css", "--path", "/b.js",
        ],
    )
    .await
    .unwrap();
    assert!(
        purged.contains("Purged 3 cached objects for shop/prod/web"),
        "{purged}"
    );
    {
        let purges = purges.lock().unwrap();
        assert_eq!(purges.len(), 1);
        assert_eq!(purges[0]["host"], "shop.example.com");
        assert_eq!(purges[0]["paths"], json!(["/a.css", "/b.js"]));
    }

    let override_url = cli(
        &context(&config, None, Some("http://127.0.0.1:9".into())),
        &["services"],
    )
    .await
    .unwrap_err();
    assert!(override_url.contains("127.0.0.1:9"), "{override_url}");

    if docker_tests() {
        for service in [web_id.as_str(), blog_id.as_str()] {
            let (status, body) = panel
                .call("DELETE", &format!("/api/v1/services/{service}"), None)
                .await;
            assert_eq!(status, 204, "{body}");
        }
    }

    let out = cli(&saved_ctx, &["logout"]).await.unwrap();
    assert!(out.contains("Removed"));
    assert!(!config.exists());
    std::fs::remove_dir_all(dir).ok();
}
