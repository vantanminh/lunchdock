//! Cache policies, domain cache routing, and purge/stats forwarding to a cache admin API.
//!
//! The cache admin listener is a small in-test server, so no Docker or cache binary is needed.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

fn app(pool: PgPool, data_dir: &Path, cache_admin: &str) -> axum::Router {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let vars = HashMap::from([
        ("DATABASE_URL".into(), url),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_DATA_DIR".into(), data_dir.display().to_string()),
        ("RUDDER_CACHE_ADMIN_URL".into(), cache_admin.to_owned()),
        ("RUDDER_HOST_PROC".into(), data_dir.display().to_string()),
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

    async fn expect(
        &self,
        status: StatusCode,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Value {
        let (got, body_out) = self.call(method, path, body).await;
        assert_eq!(got, status, "{method} {path}: {body_out}");
        body_out
    }
}

#[derive(Clone)]
struct FakeCache {
    token_file: PathBuf,
    purges: Arc<Mutex<Vec<Value>>>,
}

fn authorized(fake: &FakeCache, headers: &HeaderMap) -> bool {
    let token = std::fs::read_to_string(&fake.token_file).unwrap_or_default();
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == format!("Bearer {}", token.trim()))
}

async fn fake_purge(
    State(fake): State<FakeCache>,
    headers: HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> (StatusCode, axum::Json<Value>) {
    if !authorized(&fake, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": "wrong admin token" })),
        );
    }
    fake.purges.lock().unwrap().push(body);
    (StatusCode::OK, axum::Json(json!({ "removed": 3 })))
}

async fn fake_stats(
    State(fake): State<FakeCache>,
    headers: HeaderMap,
) -> (StatusCode, axum::Json<Value>) {
    if !authorized(&fake, &headers) {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({ "error": "wrong admin token" })),
        );
    }
    let host = |name: &str, hits: u64| {
        json!({ "host": name, "hits": hits, "misses": 1, "stale": 0, "bypass": 2,
                "bytes_served": 100, "objects": 4, "stored_bytes": 4096 })
    };
    (
        StatusCode::OK,
        axum::Json(json!({
            "version": "0.1.0", "rules_version": 7, "objects": 9, "stored_bytes": 8192,
            "memory_max_bytes": 268435456, "evictions": 0, "reloads": 2,
            "hosts": [host("www.example.com", 5), host("other.example.net", 1)]
        })),
    )
}

fn rules_json(data_dir: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(data_dir.join("cache/rules.json")).unwrap())
        .unwrap()
}

#[sqlx::test(migrations = "./migrations")]
async fn policies_routing_purge_and_stats(pool: PgPool) {
    let data_dir = std::env::temp_dir().join(format!("rudder-cache-api-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let fake = FakeCache {
        token_file: data_dir.join("cache/admin.token"),
        purges: Arc::default(),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_url = format!("http://{}", listener.local_addr().unwrap());
    let fake_app = axum::Router::new()
        .route("/purge", axum::routing::post(fake_purge))
        .route("/stats", axum::routing::get(fake_stats))
        .with_state(fake.clone());
    tokio::spawn(async move { axum::serve(listener, fake_app).await.unwrap() });

    let client = Client::owner(app(pool, &data_dir, &admin_url)).await;
    let server = client
        .expect(
            StatusCode::CREATED,
            "POST",
            "/api/v1/servers",
            Some(json!({ "name": "local", "kind": "local" })),
        )
        .await;
    let project = client
        .expect(
            StatusCode::CREATED,
            "POST",
            "/api/v1/projects",
            Some(json!({ "name": "web" })),
        )
        .await;
    let environment = client
        .expect(
            StatusCode::CREATED,
            "POST",
            &format!(
                "/api/v1/projects/{}/environments",
                project["id"].as_str().unwrap()
            ),
            Some(json!({ "name": "prod" })),
        )
        .await;
    let service = client
        .expect(
            StatusCode::CREATED,
            "POST",
            &format!(
                "/api/v1/environments/{}/services",
                environment["id"].as_str().unwrap()
            ),
            Some(json!({
                "name": "web",
                "kind": "application",
                "server_id": server["id"],
                "spec": { "image": "nginx:1.27" }
            })),
        )
        .await;
    let service_id = service["id"].as_str().unwrap();
    let domain = client
        .expect(
            StatusCode::CREATED,
            "POST",
            &format!("/api/v1/services/{service_id}/domains"),
            Some(json!({ "host": "www.example.com", "target_port": 8080, "ssl_mode": "none" })),
        )
        .await;
    let domain_id = domain["id"].as_str().unwrap();

    let nothing_cached = client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            &format!("/api/v1/services/{service_id}/cache/purge"),
            Some(json!({ "all": true })),
        )
        .await;
    assert!(nothing_cached["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .contains("no cached"));

    let presets = client
        .expect(StatusCode::OK, "GET", "/api/v1/cache-presets", None)
        .await;
    let slugs: Vec<&str> = presets
        .as_array()
        .unwrap()
        .iter()
        .map(|preset| preset["slug"].as_str().unwrap())
        .collect();
    assert!(slugs.contains(&"wordpress") && slugs.contains(&"nextjs"));

    let policy = client
        .expect(
            StatusCode::CREATED,
            "POST",
            "/api/v1/cache-policies",
            Some(json!({ "name": "Blog", "preset": "wordpress", "memory_max_bytes": 67108864 })),
        )
        .await;
    let policy_id = policy["id"].as_str().unwrap();
    assert_eq!(policy["rules"].as_array().unwrap().len(), 3);
    assert_eq!(policy["rules"][0]["action"]["mode"], "bypass");
    client
        .expect(
            StatusCode::CONFLICT,
            "POST",
            "/api/v1/cache-policies",
            Some(json!({ "name": "Blog", "preset": "ghost" })),
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/cache-policies",
            Some(json!({ "name": "Tiny", "memory_max_bytes": 10 })),
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            "/api/v1/cache-policies",
            Some(json!({ "name": "Bad", "rules": [{ "match": { "path_prefixes": ["no-slash"] }, "action": {} }] })),
        )
        .await;

    let enabled = client
        .expect(
            StatusCode::OK,
            "PUT",
            &format!("/api/v1/domains/{domain_id}/cache"),
            Some(json!({ "enabled": true, "policy_id": policy_id })),
        )
        .await;
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["policy_name"], "Blog");
    let routes = std::fs::read_to_string(data_dir.join("traefik/dynamic/routes.yml")).unwrap();
    assert!(routes.contains("http://rudder-cache:8080"), "{routes}");
    let document = rules_json(&data_dir);
    assert_eq!(document["sites"][0]["host"], "www.example.com");
    assert_eq!(document["sites"][0]["upstream"], "web:8080");
    assert_eq!(document["sites"][0]["policy"], policy_id);
    assert_eq!(document["memory_max_bytes"], 67108864);
    assert_eq!(
        document["policies"][0]["rules"].as_array().unwrap().len(),
        3
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&fake.token_file)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let view = client
        .expect(
            StatusCode::OK,
            "GET",
            &format!("/api/v1/services/{service_id}/cache"),
            None,
        )
        .await;
    assert_eq!(view["domains"][0]["enabled"], true);
    assert!(view["stats_error"].is_null(), "{view}");
    assert_eq!(
        view["stats"].as_array().unwrap().len(),
        1,
        "only this service's hosts"
    );
    assert_eq!(view["stats"][0]["hits"], 5);

    let purged = client
        .expect(
            StatusCode::OK,
            "POST",
            &format!("/api/v1/services/{service_id}/cache/purge"),
            Some(json!({ "all": true })),
        )
        .await;
    assert_eq!(purged["removed"], 3);
    client
        .expect(
            StatusCode::OK,
            "POST",
            &format!("/api/v1/services/{service_id}/cache/purge"),
            Some(json!({ "paths": ["/hello/?a=1"] })),
        )
        .await;
    client
        .expect(
            StatusCode::BAD_REQUEST,
            "POST",
            &format!("/api/v1/services/{service_id}/cache/purge"),
            Some(json!({ "paths": ["relative"] })),
        )
        .await;
    {
        let purges = fake.purges.lock().unwrap();
        assert_eq!(purges.len(), 2);
        assert_eq!(purges[0]["host"], "www.example.com");
        assert_eq!(purges[0]["all"], true);
        assert_eq!(purges[1]["paths"][0], "/hello/?a=1");
    }

    let stats = client
        .expect(
            StatusCode::OK,
            "GET",
            &format!(
                "/api/v1/servers/{}/cache/stats",
                server["id"].as_str().unwrap()
            ),
            None,
        )
        .await;
    assert_eq!(stats["rules_version"], 7);

    let updated = client
        .expect(
            StatusCode::OK,
            "PUT",
            &format!("/api/v1/cache-policies/{policy_id}"),
            Some(json!({
                "name": "Blog",
                "rules": [{ "name": "all", "match": {}, "action": { "ttl_secs": 60 } }]
            })),
        )
        .await;
    assert_eq!(updated["domains"], 1);
    assert_eq!(
        rules_json(&data_dir)["policies"][0]["rules"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        rules_json(&data_dir)["memory_max_bytes"],
        268435456,
        "budget cleared falls back"
    );

    client
        .expect(
            StatusCode::NO_CONTENT,
            "DELETE",
            &format!("/api/v1/cache-policies/{policy_id}"),
            None,
        )
        .await;
    let fallback = rules_json(&data_dir);
    assert_eq!(fallback["sites"][0]["policy"], "default");
    assert_eq!(fallback["policies"][0]["name"], "Honor origin headers");

    client
        .expect(
            StatusCode::OK,
            "PUT",
            &format!("/api/v1/domains/{domain_id}/cache"),
            Some(json!({ "enabled": false, "policy_id": null })),
        )
        .await;
    let routes = std::fs::read_to_string(data_dir.join("traefik/dynamic/routes.yml")).unwrap();
    assert!(!routes.contains("rudder-cache"));
    assert!(rules_json(&data_dir)["sites"]
        .as_array()
        .unwrap()
        .is_empty());

    std::fs::write(&fake.token_file, "rotated").unwrap();
    client
        .expect(
            StatusCode::OK,
            "PUT",
            &format!("/api/v1/domains/{domain_id}/cache"),
            Some(json!({ "enabled": true, "policy_id": null })),
        )
        .await;
    assert_eq!(
        std::fs::read_to_string(&fake.token_file).unwrap(),
        "rotated",
        "existing token kept"
    );
    std::fs::remove_dir_all(&data_dir).unwrap();
}

#[sqlx::test(migrations = "./migrations")]
async fn cache_memory_settings_and_history(pool: PgPool) {
    let data_dir =
        std::env::temp_dir().join(format!("rudder-cache-settings-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let client = Client::owner(app(pool.clone(), &data_dir, "http://127.0.0.1:9")).await;
    let settings = client
        .expect(StatusCode::OK, "GET", "/api/v1/cache/settings", None)
        .await;
    assert_eq!(settings["memory_percent"], 15);
    assert!(settings["memory_cap_bytes"].is_null());
    assert!(settings["effective_cap_bytes"].as_i64().unwrap() <= 384 * 1024 * 1024);

    let updated = client
        .expect(
            StatusCode::OK,
            "PUT",
            "/api/v1/cache/settings",
            Some(json!({
                "memory_percent": 10,
                "memory_cap_bytes": 32 * 1024 * 1024
            })),
        )
        .await;
    assert_eq!(updated["memory_percent"], 10);
    assert_eq!(updated["effective_cap_bytes"], 32 * 1024 * 1024);

    let (status, _) = client
        .call(
            "PUT",
            "/api/v1/cache/settings",
            Some(json!({"memory_percent": 90, "memory_cap_bytes": null})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let server_id: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO servers (name, kind) VALUES ('edge', 'local') RETURNING id",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    for (minutes, hits, misses) in [(2, 10_i64, 10_i64), (0, 30, 20)] {
        sqlx::query(
            r#"
            INSERT INTO cache_stat_samples (
                server_id, sampled_at, objects, stored_bytes, memory_max_bytes, evictions,
                hits, misses, stale, bypass, bytes_served
            )
            VALUES ($1, now() - ($2 * interval '1 minute'), 2, 1000, 32000000, 1, $3, $4, 0, 0, $3)
            "#,
        )
        .bind(server_id)
        .bind(minutes)
        .bind(hits)
        .bind(misses)
        .execute(&pool)
        .await
        .unwrap();
    }
    let history = client
        .expect(
            StatusCode::OK,
            "GET",
            &format!("/api/v1/cache/history?server_id={server_id}&hours=1"),
            None,
        )
        .await;
    assert_eq!(history[0]["name"], "edge");
    assert_eq!(history[0]["points"][1]["hits"], 20);
    assert_eq!(history[0]["points"][1]["misses"], 10);
    let ratio = history[0]["points"][1]["hit_ratio"].as_f64().unwrap();
    assert!((ratio - (20.0 / 30.0)).abs() < 0.001, "{ratio}");
    std::fs::remove_dir_all(&data_dir).unwrap();
}
