//! Runs the real `rudder-cache` binary in front of an in-process origin.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, Uri};
use axum::response::IntoResponse;
use serde_json::json;

struct Killed(Child);

impl Drop for Killed {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[derive(Clone, Default)]
struct Origin {
    hits: Arc<AtomicU64>,
    failing: Arc<std::sync::atomic::AtomicBool>,
    encodings: Arc<Mutex<Vec<String>>>,
}

async fn origin(State(origin): State<Origin>, uri: Uri, headers: HeaderMap) -> impl IntoResponse {
    let count = origin.hits.fetch_add(1, Ordering::SeqCst) + 1;
    if uri.path() == "/flaky" && origin.failing.load(Ordering::SeqCst) {
        return axum::http::Response::builder()
            .status(503)
            .body(axum::body::Body::from("down"))
            .unwrap();
    }
    origin.encodings.lock().unwrap().push(
        headers
            .get("accept-encoding")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("-")
            .to_owned(),
    );
    let mut response = axum::http::Response::builder().status(200);
    response = match uri.path() {
        "/private" => response.header("cache-control", "private, max-age=60"),
        "/cookie" => response.header("set-cookie", "a=b"),
        path if path.ends_with(".css") => response.header("cache-control", "public, max-age=60"),
        _ => response,
    };
    response
        .body(axum::body::Body::from(format!("{} {count}", uri.path())))
        .unwrap()
}

fn free_port() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

fn rules(version: u64, upstream: &str, hosts: &[&str]) -> String {
    let sites: Vec<_> = hosts
        .iter()
        .map(
            |host| json!({ "host": host, "path_prefix": "/", "upstream": upstream, "policy": "p" }),
        )
        .collect();
    json!({
        "version": version,
        "sites": sites,
        "policies": [{
            "id": "p",
            "name": "Test",
            "rules": [
                { "name": "admin", "match": { "path_prefixes": ["/admin"] }, "action": { "mode": "bypass" } },
                { "name": "flaky", "match": { "path_prefixes": ["/flaky"] },
                  "action": { "ttl_secs": 1, "respect_origin": false, "stale_if_error_secs": 60 } },
                { "name": "swr", "match": { "path_prefixes": ["/swr"] },
                  "action": { "ttl_secs": 1, "respect_origin": false, "stale_while_revalidate_secs": 60 } },
                { "name": "pages", "match": {}, "action": { "ttl_secs": 30, "bypass_cookies": ["session"] } }
            ]
        }]
    })
    .to_string()
}

fn write(path: &Path, text: &str) {
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, text).unwrap();
    std::fs::rename(temp, path).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn caches_purges_reloads_and_reports() {
    let state = Origin::default();
    let origin_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let upstream = origin_listener.local_addr().unwrap().to_string();
    let app = axum::Router::new()
        .fallback(origin)
        .with_state(state.clone());
    tokio::spawn(async move { axum::serve(origin_listener, app).await.unwrap() });

    let dir = std::env::temp_dir().join(format!("rudder-cache-it-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rules_path = dir.join("rules.json");
    write(&rules_path, &rules(1, &upstream, &["site.test"]));
    let listen = free_port();
    let admin = free_port();
    let _child = Killed(
        Command::new(env!("CARGO_BIN_EXE_rudder-cache"))
            .args(["--rules", rules_path.to_str().unwrap()])
            .args(["--listen", &listen.to_string()])
            .args(["--admin-listen", &admin.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let http = reqwest::Client::new();
    let admin_url = format!("http://{admin}");
    let mut up = false;
    for _ in 0..100 {
        if let Ok(response) = http.get(format!("{admin_url}/health")).send().await {
            up = response.status() == 200;
            if up {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(up, "cache admin did not come up");
    let token = std::fs::read_to_string(dir.join("admin.token")).unwrap();
    let proxy = format!("http://{listen}");
    let get = |path: &str, host: &str| {
        http.get(format!("{proxy}{path}"))
            .header("host", host)
            .build()
            .unwrap()
    };
    let send = |request: reqwest::Request| {
        let http = http.clone();
        async move {
            let response = http.execute(request).await.unwrap();
            let status = response.status().as_u16();
            let cache = response
                .headers()
                .get("x-cache")
                .map(|value| value.to_str().unwrap().to_owned())
                .unwrap_or_default();
            (status, cache, response.text().await.unwrap())
        }
    };

    let miss = send(get("/app.css", "site.test")).await;
    assert_eq!(miss, (200, "MISS".into(), "/app.css 1".into()));
    let hit = send(get("/app.css", "site.test")).await;
    assert_eq!(hit, (200, "HIT".into(), "/app.css 1".into()));

    let mut with_cookie = get("/page", "site.test");
    with_cookie
        .headers_mut()
        .insert("cookie", "theme=dark; session=abc".parse().unwrap());
    assert_eq!(send(with_cookie).await.1, "BYPASS");
    assert_eq!(send(get("/admin/x", "site.test")).await.1, "BYPASS");
    let post = http
        .post(format!("{proxy}/page"))
        .header("host", "site.test")
        .build()
        .unwrap();
    assert_eq!(send(post).await.1, "BYPASS");

    let private_first = send(get("/private", "site.test")).await;
    let private_second = send(get("/private", "site.test")).await;
    assert_eq!(private_first.1, "MISS");
    assert_ne!(
        private_first.2, private_second.2,
        "private responses are not stored"
    );
    assert_ne!(
        send(get("/cookie", "site.test")).await.2,
        send(get("/cookie", "site.test")).await.2,
        "responses with Set-Cookie are not stored"
    );

    let mut brotli = get("/app.css", "site.test");
    brotli
        .headers_mut()
        .insert("accept-encoding", "gzip, br".parse().unwrap());
    assert_eq!(send(brotli.try_clone().unwrap()).await.1, "MISS");
    assert_eq!(send(brotli).await.1, "HIT");
    assert!(state
        .encodings
        .lock()
        .unwrap()
        .iter()
        .any(|value| value == "br"));

    assert_eq!(send(get("/", "unknown.test")).await.0, 404);

    let fresh = send(get("/flaky", "site.test")).await;
    assert_eq!(fresh.1, "MISS");
    let swr_first = send(get("/swr", "site.test")).await;
    tokio::time::sleep(Duration::from_millis(2100)).await;
    state.failing.store(true, Ordering::SeqCst);
    let stale = send(get("/flaky", "site.test")).await;
    assert_eq!(
        (stale.0, stale.2.as_str()),
        (200, fresh.2.as_str()),
        "stale-if-error serves the old copy"
    );
    assert_eq!(stale.1, "STALE");
    state.failing.store(false, Ordering::SeqCst);
    let swr_stale = send(get("/swr", "site.test")).await;
    assert_eq!(
        (swr_stale.1.as_str(), swr_stale.2.as_str()),
        ("STALE", swr_first.2.as_str())
    );
    let mut refreshed = None;
    for _ in 0..40 {
        let next = send(get("/swr", "site.test")).await;
        if next.1 == "HIT" && next.2 != swr_first.2 {
            refreshed = Some(next);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        refreshed.is_some(),
        "background revalidation stored a new copy"
    );

    let unauthorized = http
        .post(format!("{admin_url}/purge"))
        .json(&json!({ "host": "site.test", "all": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
    let purged: serde_json::Value = http
        .post(format!("{admin_url}/purge"))
        .bearer_auth(&token)
        .json(&json!({ "host": "site.test", "paths": ["/app.css"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        purged["removed"], 2,
        "both encodings of the path are removed"
    );
    assert_eq!(send(get("/app.css", "site.test")).await.1, "MISS");
    let invalid = http
        .post(format!("{admin_url}/purge"))
        .bearer_auth(&token)
        .json(&json!({ "host": "site.test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 400);

    let stats: serde_json::Value = http
        .get(format!("{admin_url}/stats"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let host = &stats["hosts"][0];
    assert_eq!(host["host"], "site.test");
    assert!(host["hits"].as_u64().unwrap() >= 2);
    assert!(host["bypass"].as_u64().unwrap() >= 3);
    assert_eq!(stats["rules_version"], 1);
    let metrics = http
        .get(format!("{admin_url}/metrics"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("rudder_cache_requests_total{host=\"site.test\",outcome=\"hit\"}"));
    assert!(metrics.contains("# TYPE rudder_cache_objects gauge"));

    write(
        &rules_path,
        &rules(2, &upstream, &["site.test", "other.test"]),
    );
    let mut reloaded = false;
    for _ in 0..60 {
        if send(get("/x", "other.test")).await.0 == 200 {
            reloaded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(reloaded, "new site was not picked up");

    write(&rules_path, "{ not json");
    let forced = http
        .post(format!("{admin_url}/reload"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(forced.status(), 422);
    assert_eq!(
        send(get("/x", "other.test")).await.0,
        200,
        "bad rules keep the previous document"
    );

    write(&rules_path, &rules(3, &upstream, &["other.test"]));
    let applied: serde_json::Value = http
        .post(format!("{admin_url}/reload"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(applied["changed"].is_boolean());
    assert_eq!(send(get("/app.css", "site.test")).await.0, 404);
    std::fs::remove_dir_all(dir).unwrap();
}
