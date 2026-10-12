//! One-click GitHub App setup against a fake GitHub API: the manifest, the
//! code exchange, the installation check, the state guards, and the app-level
//! push webhook that starts builds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Request, StatusCode};
use axum::routing::{get, post};
use axum::Json;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

const APP_ID: u64 = 12345;
const WEBHOOK_SECRET: &str = "app-webhook-secret";

/// What the fake GitHub saw, so tests can check the calls the panel made.
#[derive(Default)]
struct Seen {
    codes: Vec<String>,
    jwts: Vec<String>,
}

type Shared = Arc<Mutex<Seen>>;

async fn convert(
    State(seen): State<Shared>,
    Path(code): Path<String>,
) -> (StatusCode, Json<Value>) {
    seen.lock().unwrap().codes.push(code.clone());
    if code != "goodcode" {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not Found" })),
        );
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "id": APP_ID,
            "slug": "rudder-test",
            "name": "Rudder-test",
            "owner": { "login": "acme" },
            "client_id": "Iv1.client",
            "client_secret": "client-secret-value",
            "webhook_secret": WEBHOOK_SECRET,
            "pem": TEST_APP_KEY,
        })),
    )
}

async fn installation(
    State(seen): State<Shared>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    let jwt = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_owned();
    seen.lock().unwrap().jwts.push(jwt);
    match id.as_str() {
        "424242" => (
            StatusCode::OK,
            Json(json!({ "id": 424242, "app_id": APP_ID, "account": { "login": "acme" } })),
        ),
        "999" => (
            StatusCode::OK,
            Json(json!({ "id": 999, "app_id": 777, "account": { "login": "other" } })),
        ),
        _ => (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not Found" })),
        ),
    }
}

const INSTALLATION_TOKEN: &str = "ghs_faketoken";

async fn access_token(Path(id): Path<String>) -> (StatusCode, Json<Value>) {
    if id == "424242" {
        (
            StatusCode::CREATED,
            Json(json!({ "token": INSTALLATION_TOKEN })),
        )
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({ "message": "Not Found" })),
        )
    }
}

fn missing() -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "message": "Not Found" })),
    )
}

fn has_token(headers: &HeaderMap) -> bool {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        == Some(&format!("Bearer {INSTALLATION_TOKEN}"))
}

async fn installation_repositories(headers: HeaderMap) -> (StatusCode, Json<Value>) {
    if !has_token(&headers) {
        return missing();
    }
    let repo = |name: &str, private: bool| {
        json!({
            "full_name": name,
            "clone_url": format!("https://github.com/{name}.git"),
            "private": private,
            "default_branch": "main",
        })
    };
    (
        StatusCode::OK,
        Json(json!({
            "total_count": 2,
            "repositories": [repo("acme/web", true), repo("acme/stack", false)]
        })),
    )
}

async fn repository(
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if has_token(&headers) && owner == "acme" && matches!(name.as_str(), "web" | "stack" | "api") {
        (StatusCode::OK, Json(json!({ "default_branch": "main" })))
    } else {
        missing()
    }
}

async fn repository_branches(
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if has_token(&headers) && owner == "acme" && name == "stack" {
        (
            StatusCode::OK,
            Json(json!([{ "name": "develop" }, { "name": "main" }, { "name": "feature/x" }])),
        )
    } else {
        missing()
    }
}

async fn repository_branch(
    Path((owner, name, branch)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if has_token(&headers)
        && owner == "acme"
        && matches!(name.as_str(), "web" | "stack" | "api")
        && matches!(branch.as_str(), "main" | "dev" | "develop" | "feature/x")
    {
        (StatusCode::OK, Json(json!({ "name": branch })))
    } else {
        missing()
    }
}

async fn repository_tree(
    Path((owner, name, branch)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> (StatusCode, Json<Value>) {
    if !(has_token(&headers) && owner == "acme" && name == "stack" && branch == "main") {
        return missing();
    }
    (
        StatusCode::OK,
        Json(json!({ "tree": [
            { "path": "README.md", "type": "blob" },
            { "path": "deploy/docker-compose.prod.yml", "type": "blob" },
            { "path": "compose.yaml", "type": "blob" },
            { "path": "deploy", "type": "tree" },
            { "path": "ci/build.yml", "type": "blob" },
        ] })),
    )
}

async fn fake_github() -> (String, Shared) {
    let seen = Shared::default();
    let router = axum::Router::new()
        .route("/app-manifests/{code}/conversions", post(convert))
        .route("/app/installations/{id}", get(installation))
        .route("/app/installations/{id}/access_tokens", post(access_token))
        .route("/installation/repositories", get(installation_repositories))
        .route("/repos/{owner}/{name}", get(repository))
        .route("/repos/{owner}/{name}/branches", get(repository_branches))
        .route(
            "/repos/{owner}/{name}/branches/{branch}",
            get(repository_branch),
        )
        .route(
            "/repos/{owner}/{name}/git/trees/{branch}",
            get(repository_tree),
        )
        .with_state(seen.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, seen)
}

fn app(pool: PgPool, github: &str, public_url: Option<&str>) -> axum::Router {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    let data_dir = std::env::temp_dir().join(format!("rudder-ghapp-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut vars = HashMap::from([
        ("DATABASE_URL".to_owned(), url),
        ("RUDDER_MASTER_KEY".to_owned(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".to_owned(), "false".to_owned()),
        ("RUDDER_DATA_DIR".to_owned(), data_dir.display().to_string()),
        ("RUDDER_GITHUB_API_URL".to_owned(), github.to_owned()),
    ]);
    if let Some(public_url) = public_url {
        vars.insert("RUDDER_PUBLIC_URL".to_owned(), public_url.to_owned());
    }
    let config = rudder_server::Config::from_map(&vars, |_| unreachable!()).unwrap();
    rudder_server::router(rudder_server::AppState::new(pool, config))
}

struct Reply {
    status: StatusCode,
    body: Value,
    cookie: Option<String>,
}

async fn call_raw(
    app: &axum::Router,
    method: &str,
    path: &str,
    session: Option<(&str, &str)>,
    body: Option<&str>,
    extra: &[(&str, &str)],
) -> Reply {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some((cookie, csrf)) = session {
        builder = builder
            .header("cookie", format!("rudder_session={cookie}"))
            .header("x-csrf-token", csrf);
    }
    for (name, value) in extra {
        builder = builder.header(*name, *value);
    }
    let request = match body {
        Some(body) => builder
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Reply {
        status,
        body,
        cookie,
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    session: Option<(&str, &str)>,
    body: Option<Value>,
) -> Reply {
    let text = body.map(|value| value.to_string());
    call_raw(app, method, path, session, text.as_deref(), &[]).await
}

/// Sets up the owner and returns their `(cookie, csrf)`.
async fn owner(app: &axum::Router) -> (String, String) {
    let created = call(
        app,
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
    (
        created.cookie.expect("session cookie"),
        created.body["csrf_token"].as_str().unwrap().to_owned(),
    )
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> String {
    const BLOCK: usize = 64;
    let mut key_block = [0u8; BLOCK];
    key_block[..key.len()].copy_from_slice(key);
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

fn state_of(url: &str) -> String {
    url.split("state=").nth(1).expect("state in url").to_owned()
}

async fn push(app: &axum::Router, provider: &str, payload: &str, secret: &str) -> Reply {
    let signature = format!(
        "sha256={}",
        hmac_sha256(secret.as_bytes(), payload.as_bytes())
    );
    call_raw(
        app,
        "POST",
        &format!("/api/v1/git-providers/{provider}/webhook"),
        None,
        Some(payload),
        &[
            ("x-github-event", "push"),
            ("x-hub-signature-256", &signature),
        ],
    )
    .await
}

#[sqlx::test(migrations = "./migrations")]
async fn manifest_describes_the_panel_and_checks_who_asks(pool: PgPool) {
    let (github, _) = fake_github().await;
    let app = app(pool, &github, Some("https://panel.example"));
    let (cookie, csrf) = owner(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));

    let started = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some(json!({})),
    )
    .await;
    assert_eq!(started.status, StatusCode::OK, "{}", started.body);
    let action = started.body["action_url"].as_str().unwrap();
    assert!(
        action.starts_with("https://github.com/settings/apps/new?state="),
        "{action}"
    );
    assert_eq!(started.body["webhook"], true);
    let manifest: Value = serde_json::from_str(started.body["manifest"].as_str().unwrap()).unwrap();
    assert!(manifest["name"].as_str().unwrap().starts_with("Rudder-"));
    assert_eq!(manifest["redirect_url"], "https://panel.example/git/github");
    assert_eq!(manifest["setup_url"], "https://panel.example/git/github");
    assert_eq!(manifest["default_permissions"]["contents"], "read");
    let hook = manifest["hook_attributes"]["url"].as_str().unwrap();
    assert!(
        hook.starts_with("https://panel.example/api/v1/git-providers/"),
        "{hook}"
    );
    assert!(hook.ends_with("/webhook"));

    let org = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some(json!({ "organization": "acme-co", "name": "Acme Deploy" })),
    )
    .await;
    assert_eq!(org.status, StatusCode::OK);
    assert!(org.body["action_url"]
        .as_str()
        .unwrap()
        .starts_with("https://github.com/organizations/acme-co/settings/apps/new?state="));
    let named: Value = serde_json::from_str(org.body["manifest"].as_str().unwrap()).unwrap();
    assert_eq!(named["name"], "Acme Deploy");

    for bad in [
        json!({ "organization": "acme/../evil" }),
        json!({ "organization": "-acme" }),
        json!({ "name": "bad/name" }),
    ] {
        let rejected = call(
            &app,
            "POST",
            "/api/v1/github-app/manifest",
            session,
            Some(bad),
        )
        .await;
        assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
    }

    let anonymous = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        None,
        Some(json!({})),
    )
    .await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let no_csrf = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        Some((cookie.as_str(), "wrong")),
        Some(json!({})),
    )
    .await;
    assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);

    let made = call(
        &app,
        "POST",
        "/api/v1/users",
        session,
        Some(json!({
            "email": "viewer@example.com",
            "password": "viewer-password-ok",
            "role": "viewer"
        })),
    )
    .await;
    assert_eq!(made.status, StatusCode::CREATED);
    let login = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        Some(json!({ "email": "viewer@example.com", "password": "viewer-password-ok" })),
    )
    .await;
    let viewer = (
        login.cookie.clone().unwrap(),
        login.body["csrf_token"].as_str().unwrap().to_owned(),
    );
    let denied = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        Some((viewer.0.as_str(), viewer.1.as_str())),
        Some(json!({})),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
}

#[sqlx::test(migrations = "./migrations")]
async fn manifest_uses_the_browser_origin_and_skips_private_webhooks(pool: PgPool) {
    let (github, _) = fake_github().await;
    let app = app(pool, &github, None);
    let (cookie, csrf) = owner(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));

    let public = call_raw(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some("{}"),
        &[("origin", "https://deploy.acme.dev")],
    )
    .await;
    let manifest: Value = serde_json::from_str(public.body["manifest"].as_str().unwrap()).unwrap();
    assert_eq!(
        manifest["redirect_url"],
        "https://deploy.acme.dev/git/github"
    );
    assert_eq!(public.body["webhook"], true);

    let private = call_raw(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some("{}"),
        &[("origin", "http://localhost:5173")],
    )
    .await;
    let manifest: Value = serde_json::from_str(private.body["manifest"].as_str().unwrap()).unwrap();
    assert_eq!(manifest["redirect_url"], "http://localhost:5173/git/github");
    assert_eq!(private.body["webhook"], false);
    assert!(manifest.get("hook_attributes").is_none());
}

#[sqlx::test(migrations = "./migrations")]
async fn app_is_created_installed_and_used_as_a_source(pool: PgPool) {
    let (github, seen) = fake_github().await;
    let app = app(pool, &github, Some("https://panel.example"));
    let (cookie, csrf) = owner(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));

    let started = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some(json!({})),
    )
    .await;
    let manifest_state = state_of(started.body["action_url"].as_str().unwrap());

    let tampered = call(
        &app,
        "POST",
        "/api/v1/github-app/complete",
        session,
        Some(json!({ "code": "goodcode", "state": format!("{manifest_state}x") })),
    )
    .await;
    assert_eq!(tampered.status, StatusCode::BAD_REQUEST);
    assert_eq!(tampered.body["error"]["code"], "invalid_state");
    let wrong_step = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "424242", "state": manifest_state })),
    )
    .await;
    assert_eq!(wrong_step.status, StatusCode::BAD_REQUEST);
    assert!(
        seen.lock().unwrap().codes.is_empty(),
        "nothing is sent to GitHub for a bad state"
    );

    let rejected = call(
        &app,
        "POST",
        "/api/v1/github-app/complete",
        session,
        Some(json!({ "code": "../../etc", "state": manifest_state })),
    )
    .await;
    assert_eq!(rejected.status, StatusCode::BAD_REQUEST);
    let expired = call(
        &app,
        "POST",
        "/api/v1/github-app/complete",
        session,
        Some(json!({ "code": "oldcode", "state": manifest_state })),
    )
    .await;
    assert_eq!(expired.status, StatusCode::BAD_REQUEST);
    assert_eq!(expired.body["error"]["code"], "github_rejected");

    let created = call(
        &app,
        "POST",
        "/api/v1/github-app/complete",
        session,
        Some(json!({ "code": "goodcode", "state": manifest_state })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let rendered = created.body.to_string();
    for secret in ["PRIVATE KEY", WEBHOOK_SECRET, "client-secret-value"] {
        assert!(!rendered.contains(secret), "{secret} leaked");
    }
    let provider = &created.body["provider"];
    assert_eq!(provider["kind"], "github_app");
    assert_eq!(provider["name"], "rudder-test");
    assert_eq!(provider["status"], "pending_install");
    assert_eq!(provider["app_slug"], "rudder-test");
    let provider_id = provider["id"].as_str().unwrap().to_owned();
    let install_url = created.body["install_url"].as_str().unwrap();
    assert!(
        install_url.starts_with("https://github.com/apps/rudder-test/installations/new?state="),
        "{install_url}"
    );

    let again = call(
        &app,
        "POST",
        "/api/v1/github-app/complete",
        session,
        Some(json!({ "code": "goodcode", "state": manifest_state })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);

    // A pending app cannot be a source yet, and says so.
    let (server, _, environment) = catalog(&app, session).await;
    let service = new_service(&app, session, &environment, &server, "web").await;
    let source = json!({
        "provider_id": provider_id,
        "repo": "https://github.com/acme/web.git",
        "branch": "main",
        "strategy": "dockerfile"
    });
    let early = call(
        &app,
        "PUT",
        &format!("/api/v1/services/{service}/source"),
        session,
        Some(source.clone()),
    )
    .await;
    assert_eq!(early.status, StatusCode::BAD_REQUEST);
    assert!(early.body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("install"));

    let install_state = state_of(install_url);
    let foreign = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "999", "state": install_state })),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::BAD_REQUEST, "{}", foreign.body);
    let unknown = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "5", "state": install_state })),
    )
    .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    let malformed = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "12/../3", "state": install_state })),
    )
    .await;
    assert_eq!(malformed.status, StatusCode::BAD_REQUEST);

    let ready = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "424242", "state": install_state })),
    )
    .await;
    assert_eq!(ready.status, StatusCode::OK, "{}", ready.body);
    assert_eq!(ready.body["status"], "ready");
    assert_eq!(ready.body["account"], "acme");
    let jwts = seen.lock().unwrap().jwts.clone();
    assert!(jwts.len() >= 3);
    assert!(
        jwts.iter().all(|jwt| jwt.split('.').count() == 3),
        "{jwts:?}"
    );

    let listed = call(&app, "GET", "/api/v1/git-providers", session, None).await;
    assert_eq!(listed.body[0]["status"], "ready");
    assert_eq!(listed.body[0]["account"], "acme");
    assert!(!listed.body.to_string().contains("PRIVATE KEY"));

    let saved = call(
        &app,
        "PUT",
        &format!("/api/v1/services/{service}/source"),
        session,
        Some(source),
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.body);

    let link = call(
        &app,
        "POST",
        &format!("/api/v1/git-providers/{provider_id}/install-link"),
        session,
        None,
    )
    .await;
    assert_eq!(link.status, StatusCode::OK);
    assert!(link.body["install_url"]
        .as_str()
        .unwrap()
        .starts_with("https://github.com/apps/rudder-test/installations/new?state="));
    let pat = call(
        &app,
        "POST",
        "/api/v1/git-providers",
        session,
        Some(json!({ "name": "pat", "kind": "github_pat", "token": "t0ken" })),
    )
    .await;
    assert_eq!(pat.body["status"], "ready");
    let not_app = call(
        &app,
        "POST",
        &format!(
            "/api/v1/git-providers/{}/install-link",
            pat.body["id"].as_str().unwrap()
        ),
        session,
        None,
    )
    .await;
    assert_eq!(not_app.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn installation_made_on_github_is_found_without_state(pool: PgPool) {
    let (github, _) = fake_github().await;
    let app = app(pool, &github, Some("https://panel.example"));
    let (cookie, csrf) = owner(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));

    // Nothing is waiting yet.
    let nothing = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "424242" })),
    )
    .await;
    assert_eq!(nothing.status, StatusCode::BAD_REQUEST);
    assert_eq!(nothing.body["error"]["code"], "no_pending_app");

    let started = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some(json!({})),
    )
    .await;
    let created = call(
        &app,
        "POST",
        "/api/v1/github-app/complete",
        session,
        Some(json!({
            "code": "goodcode",
            "state": state_of(started.body["action_url"].as_str().unwrap())
        })),
    )
    .await;
    assert_eq!(created.body["provider"]["status"], "pending_install");

    // An installation of some other app does not attach to the pending one.
    let foreign = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "999" })),
    )
    .await;
    assert_eq!(foreign.status, StatusCode::BAD_REQUEST);
    assert_eq!(foreign.body["error"]["code"], "no_pending_app");

    // GitHub sends no state after an install made on GitHub itself.
    let attached = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "424242" })),
    )
    .await;
    assert_eq!(attached.status, StatusCode::OK, "{}", attached.body);
    assert_eq!(attached.body["status"], "ready");
    assert_eq!(attached.body["account"], "acme");

    // Once installed, no app is pending any more.
    let again = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "424242" })),
    )
    .await;
    assert_eq!(again.status, StatusCode::BAD_REQUEST);
}

#[sqlx::test(migrations = "./migrations")]
async fn state_belongs_to_the_admin_who_started(pool: PgPool) {
    let (github, seen) = fake_github().await;
    let app = app(pool, &github, Some("https://panel.example"));
    let (cookie, csrf) = owner(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));
    let second = call(
        &app,
        "POST",
        "/api/v1/users",
        session,
        Some(json!({
            "email": "second@example.com",
            "password": "second-password-ok",
            "role": "admin"
        })),
    )
    .await;
    assert_eq!(second.status, StatusCode::CREATED, "{}", second.body);
    let login = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        Some(json!({ "email": "second@example.com", "password": "second-password-ok" })),
    )
    .await;
    let other = (
        login.cookie.clone().unwrap(),
        login.body["csrf_token"].as_str().unwrap().to_owned(),
    );

    let started = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some(json!({})),
    )
    .await;
    let sealed = state_of(started.body["action_url"].as_str().unwrap());
    let stolen = call(
        &app,
        "POST",
        "/api/v1/github-app/complete",
        Some((other.0.as_str(), other.1.as_str())),
        Some(json!({ "code": "goodcode", "state": sealed })),
    )
    .await;
    assert_eq!(stolen.status, StatusCode::BAD_REQUEST);
    assert!(seen.lock().unwrap().codes.is_empty());
}

#[sqlx::test(migrations = "./migrations")]
async fn app_webhook_builds_the_services_that_use_the_repository(pool: PgPool) {
    let (github, _) = fake_github().await;
    let app = app(pool, &github, Some("https://panel.example"));
    let (cookie, csrf) = owner(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));

    let started = call(
        &app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some(json!({})),
    )
    .await;
    let created = call(
        &app,
        "POST",
        "/api/v1/github-app/complete",
        session,
        Some(json!({
            "code": "goodcode",
            "state": state_of(started.body["action_url"].as_str().unwrap())
        })),
    )
    .await;
    let provider = created.body["provider"]["id"].as_str().unwrap().to_owned();
    let install_state = state_of(created.body["install_url"].as_str().unwrap());
    let ready = call(
        &app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "424242", "state": install_state })),
    )
    .await;
    assert_eq!(ready.status, StatusCode::OK);

    let (server, _, environment) = catalog(&app, session).await;
    let mut ids = HashMap::new();
    for (name, repo, branch) in [
        ("web", "https://github.com/acme/web.git", "main"),
        ("web-dev", "git@github.com:Acme/Web.git", "dev"),
        ("api", "https://github.com/acme/api", "main"),
    ] {
        let id = new_service(&app, session, &environment, &server, name).await;
        let saved = call(
            &app,
            "PUT",
            &format!("/api/v1/services/{id}/source"),
            session,
            Some(json!({
                "provider_id": provider,
                "repo": repo,
                "branch": branch,
                "strategy": "dockerfile"
            })),
        )
        .await;
        assert_eq!(saved.status, StatusCode::OK, "{name}: {}", saved.body);
        ids.insert(name, id);
    }

    let main =
        r#"{"ref":"refs/heads/main","after":"abcdef1","repository":{"full_name":"Acme/Web"}}"#;
    let built = push(&app, &provider, main, WEBHOOK_SECRET).await;
    assert_eq!(built.status, StatusCode::ACCEPTED, "{}", built.body);
    assert_eq!(built.body["deploy"], true);
    let builds = built.body["builds"].as_array().unwrap();
    assert_eq!(builds.len(), 1);
    assert_eq!(builds[0]["service_id"], ids["web"]);

    let dev = r#"{"ref":"refs/heads/dev","after":"abcdef2","repository":{"full_name":"acme/web"}}"#;
    let built = push(&app, &provider, dev, WEBHOOK_SECRET).await;
    assert_eq!(built.status, StatusCode::ACCEPTED);
    assert_eq!(built.body["builds"][0]["service_id"], ids["web-dev"]);

    for (payload, why) in [
        (
            r#"{"ref":"refs/heads/feature","after":"abcdef3","repository":{"full_name":"acme/web"}}"#,
            "other branch",
        ),
        (
            r#"{"ref":"refs/heads/main","after":"abcdef3","repository":{"full_name":"acme/other"}}"#,
            "other repository",
        ),
        (
            r#"{"ref":"refs/heads/main","after":"0000000","deleted":true,"repository":{"full_name":"acme/web"}}"#,
            "deleted branch",
        ),
    ] {
        let ignored = push(&app, &provider, payload, WEBHOOK_SECRET).await;
        assert_eq!(ignored.status, StatusCode::OK, "{why}: {}", ignored.body);
        assert_eq!(ignored.body["deploy"], false, "{why}");
    }

    let forged = push(&app, &provider, main, "not-the-secret").await;
    assert_eq!(forged.status, StatusCode::UNAUTHORIZED);
    let unsigned = call_raw(
        &app,
        "POST",
        &format!("/api/v1/git-providers/{provider}/webhook"),
        None,
        Some(main),
        &[("x-github-event", "push")],
    )
    .await;
    assert_eq!(unsigned.status, StatusCode::UNAUTHORIZED);
    let missing = push(
        &app,
        &uuid::Uuid::new_v4().to_string(),
        main,
        WEBHOOK_SECRET,
    )
    .await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);

    let ping = call_raw(
        &app,
        "POST",
        &format!("/api/v1/git-providers/{provider}/webhook"),
        None,
        Some("{}"),
        &[
            ("x-github-event", "ping"),
            (
                "x-hub-signature-256",
                &format!("sha256={}", hmac_sha256(WEBHOOK_SECRET.as_bytes(), b"{}")),
            ),
        ],
    )
    .await;
    assert_eq!(ping.status, StatusCode::OK);
    assert_eq!(ping.body["detail"], "ping");
}

/// An app that is created and installed against the fake GitHub; returns the provider id.
async fn installed_app(app: &axum::Router, session: Option<(&str, &str)>) -> String {
    let started = call(
        app,
        "POST",
        "/api/v1/github-app/manifest",
        session,
        Some(json!({})),
    )
    .await;
    let manifest_state = state_of(started.body["action_url"].as_str().unwrap());
    let created = call(
        app,
        "POST",
        "/api/v1/github-app/complete",
        session,
        Some(json!({ "code": "goodcode", "state": manifest_state })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let install_state = state_of(created.body["install_url"].as_str().unwrap());
    let ready = call(
        app,
        "POST",
        "/api/v1/github-app/installed",
        session,
        Some(json!({ "installation_id": "424242", "state": install_state })),
    )
    .await;
    assert_eq!(ready.status, StatusCode::OK, "{}", ready.body);
    created.body["provider"]["id"].as_str().unwrap().to_owned()
}

#[sqlx::test(migrations = "./migrations")]
async fn pickers_list_what_the_app_can_read_and_saving_checks_access(pool: PgPool) {
    let (github, _seen) = fake_github().await;
    let app = app(pool, &github, Some("https://panel.example"));
    let (cookie, csrf) = owner(&app).await;
    let session = Some((cookie.as_str(), csrf.as_str()));
    let provider = installed_app(&app, session).await;

    let repos = call(
        &app,
        "GET",
        &format!("/api/v1/git-providers/{provider}/repositories"),
        session,
        None,
    )
    .await;
    assert_eq!(repos.status, StatusCode::OK, "{}", repos.body);
    assert_eq!(repos.body[0]["full_name"], "acme/stack");
    assert_eq!(repos.body[1]["full_name"], "acme/web");
    assert_eq!(repos.body[1]["private"], true);
    assert_eq!(repos.body[1]["url"], "https://github.com/acme/web.git");
    assert!(!repos.body.to_string().contains(INSTALLATION_TOKEN));

    let branches = call(
        &app,
        "GET",
        &format!(
            "/api/v1/git-providers/{provider}/branches?repo=https://github.com/acme/stack.git"
        ),
        session,
        None,
    )
    .await;
    assert_eq!(branches.status, StatusCode::OK, "{}", branches.body);
    assert_eq!(branches.body["default_branch"], "main");
    assert_eq!(
        branches.body["branches"],
        json!(["main", "develop", "feature/x"]),
        "default first"
    );
    let outside = call(
        &app,
        "GET",
        &format!("/api/v1/git-providers/{provider}/branches?repo=acme/secret"),
        session,
        None,
    )
    .await;
    assert_eq!(outside.status, StatusCode::BAD_REQUEST);
    assert_eq!(outside.body["error"]["code"], "repo_not_accessible");
    let not_github = call(
        &app,
        "GET",
        &format!("/api/v1/git-providers/{provider}/branches?repo=https://gitlab.com/acme/stack"),
        session,
        None,
    )
    .await;
    assert_eq!(not_github.status, StatusCode::BAD_REQUEST);

    let files = call(
        &app,
        "GET",
        &format!("/api/v1/git-providers/{provider}/compose-files?repo=acme/stack&branch=main"),
        session,
        None,
    )
    .await;
    assert_eq!(files.status, StatusCode::OK, "{}", files.body);
    assert_eq!(
        files.body["files"],
        json!(["compose.yaml", "deploy/docker-compose.prod.yml"])
    );
    let no_branch = call(
        &app,
        "GET",
        &format!("/api/v1/git-providers/{provider}/compose-files?repo=acme/stack&branch=gone"),
        session,
        None,
    )
    .await;
    assert_eq!(no_branch.status, StatusCode::NOT_FOUND);
    let bad_branch = call(
        &app,
        "GET",
        &format!("/api/v1/git-providers/{provider}/compose-files?repo=acme/stack&branch=..%2Fx"),
        session,
        None,
    )
    .await;
    assert_eq!(bad_branch.status, StatusCode::BAD_REQUEST);
    let signed_out = call(
        &app,
        "GET",
        &format!("/api/v1/git-providers/{provider}/repositories"),
        None,
        None,
    )
    .await;
    assert_eq!(signed_out.status, StatusCode::UNAUTHORIZED);

    // Saving a source checks that the app can read the repository and the branch.
    let (server, _, environment) = catalog(&app, session).await;
    let service = new_service(&app, session, &environment, &server, "web").await;
    let save = |repo: &str, branch: &str| {
        let app = app.clone();
        let cookie = cookie.clone();
        let csrf = csrf.clone();
        let path = format!("/api/v1/services/{service}/source");
        let body = json!({
            "provider_id": provider, "repo": repo, "branch": branch, "strategy": "dockerfile"
        });
        async move { call(&app, "PUT", &path, Some((&cookie, &csrf)), Some(body)).await }
    };
    let typo = save("https://github.com/acme/wbe.git", "main").await;
    assert_eq!(typo.status, StatusCode::BAD_REQUEST, "{}", typo.body);
    assert_eq!(typo.body["error"]["code"], "repo_not_accessible");
    let branch = save("https://github.com/acme/web.git", "nope").await;
    assert_eq!(branch.status, StatusCode::BAD_REQUEST);
    assert_eq!(branch.body["error"]["code"], "branch_not_found");
    let fine = save("https://github.com/acme/web.git", "main").await;
    assert_eq!(fine.status, StatusCode::OK, "{}", fine.body);

    // The same check runs when a stack is created or moved.
    let stack = |repo: &str| {
        json!({
            "name": "stack", "server_id": server, "source": "git",
            "git": { "provider_id": provider, "repo": repo, "branch": "main",
                     "compose_file": "compose.yaml" }
        })
    };
    let environment_stacks = format!("/api/v1/environments/{environment}/stacks");
    let refused = call(
        &app,
        "POST",
        &environment_stacks,
        session,
        Some(stack("https://github.com/acme/missing.git")),
    )
    .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST, "{}", refused.body);
    assert_eq!(refused.body["error"]["code"], "repo_not_accessible");
    let created = call(
        &app,
        "POST",
        &environment_stacks,
        session,
        Some(stack("https://github.com/acme/stack.git")),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    let stack_id = created.body["service"]["id"].as_str().unwrap();
    let moved = call(
        &app,
        "PUT",
        &format!("/api/v1/services/{stack_id}/stack"),
        session,
        Some(json!({
            "source": "git",
            "git": { "provider_id": provider, "repo": "https://github.com/acme/missing.git",
                     "branch": "main", "compose_file": "compose.yaml" }
        })),
    )
    .await;
    assert_eq!(moved.status, StatusCode::BAD_REQUEST, "{}", moved.body);
}

/// A local server, a project, and an environment. Returns their ids.
async fn catalog(app: &axum::Router, session: Option<(&str, &str)>) -> (String, String, String) {
    let server = call(
        app,
        "POST",
        "/api/v1/servers",
        session,
        Some(json!({ "name": "local", "kind": "local" })),
    )
    .await;
    let project = call(
        app,
        "POST",
        "/api/v1/projects",
        session,
        Some(json!({ "name": "web" })),
    )
    .await;
    let project_id = project.body["id"].as_str().unwrap().to_owned();
    let environment = call(
        app,
        "POST",
        &format!("/api/v1/projects/{project_id}/environments"),
        session,
        Some(json!({ "name": "prod" })),
    )
    .await;
    (
        server.body["id"].as_str().unwrap().to_owned(),
        project_id,
        environment.body["id"].as_str().unwrap().to_owned(),
    )
}

async fn new_service(
    app: &axum::Router,
    session: Option<(&str, &str)>,
    environment: &str,
    server: &str,
    name: &str,
) -> String {
    let service = call(
        app,
        "POST",
        &format!("/api/v1/environments/{environment}/services"),
        session,
        Some(json!({ "name": name, "kind": "application", "server_id": server })),
    )
    .await;
    assert_eq!(service.status, StatusCode::CREATED, "{}", service.body);
    service.body["id"].as_str().unwrap().to_owned()
}

const TEST_APP_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDMm3BWuPw2Hlpa
/TRK53FVFVMQAu4YEIgfe6UxBgWcDEIQK98obYe7y3D7+pwutdqcjjNpZiNX6mRx
VZqqMFwVfTDIKGIslUEIuJrwhGrVhXBiJFH32Lhi/N0a9elm4qKyvknri1ZnpORe
AneRnumaE4wX+euulDL+asQ0k6f/G9XVgMCHfS09dZisehNngSFFlyBoyG56gall
DvGQHqOUO1ewI5OS05G/7WmId5rbHAZ0k07jPlSHHf4OaaSWjlg3q4z1TiVAUuCc
/JW429HIqqiJTqu3oUQksf2G4jR7WsfLuEfkfqHM064WdjCVsaOjXHCDEa5fLtBC
MqAf+L73AgMBAAECggEACVnXQ1dhS3XiqkLlMhaBbaKJBnPOfty5OTsfkcMqhsxh
STeVaNXB/M6vmLcw48pINfHNHrWIyvqkNtvIiEZShww5f678zE3RSD1A84S8aeAX
LUe8qUecfFr9yJQm48la0dwe07wtVD+gd5LESoGelSiXNVH6N/h/m3S0sonHBUJ4
Tw14zOayx8ce1yxrHBob/MUt6GRS0YXjYxF8JvmntwwmzhHfWRbPGDDJgAns4v0P
XYdJWZ/f8QjXAYl6UY4aKzaG8kGznX/oOfNwBmLBHWrpMHoAgUBSxTzmfSee7zKw
6U5RwNLpEDz1JSSmcGO0JznQb8xo+naq32sPsWcC5QKBgQDtln3wmUVJwzNLe7x3
TGPgYrY0ZAVah7rdDjbg8d8aLFCmfjFavtUOiwtq0t6EFB1MA7qE335N8SX4ayrm
tRFt+H/GKF1CR8B2pBovaiYupshD8M9dFHm/v1giTJbcxzwNtAHPOjivRSzWhcr3
AdssvMNnH034fkK8RvuJi6vcuwKBgQDcdqSUJ4bZKoXif1ZvsA6sSngwwnzqrc/v
4njNhtweH1lY14RT1xm4J+piIIdU5MWhwrcRIlgAd9hAd22mMDMFOonwErOAOHID
7pIzjYHBmjJmISnSz5XjDMKJ+5eIS4y7qOwvUZjoGp4Ma5sthc7s1A2pid6CrZHy
1veESNOA9QKBgEMgBaz4YM8aS1/EMdcyyfUXbosWxzxwpKbX4UFklLf8VTz04lTV
HbTiK/OeaCfA1xYm54eq7TBwNV4eSRytg6MiVldSCCc1xna+nndO8kKz9FjdrHiL
1GoMUYY/RVBTKfBlLy2xMVqznJtvaYyoBZh6LGryOUE/yvMOyvptb70JAoGAVi2C
c+9nhOhcYYdJMGtvsOWDSfwcfFfpGmo6soxu6s9yS4ojTeqdQSFJO4ZfoE20+2Bz
0xzLrZMrUzfcAsSAjSCu3stQo2RtHSBawhAkkptF7Co2gRNoEdLhELLqLvrpTknf
2TdaQBB3i39cr1TI6EaT+JvOAOL8VCK3qlwVYSkCgYAFsXiAxrbo46C8cUFNqM/U
VX+MdJnueahwi5LKxcSztUwYLVSxrWk0e5/8wAhME36acfUl/0Xgf6SFs8iVbOJ6
RDhdlwzmNJrNLaFcSKAPVltO1B8fQcBxIVbQCOwyWawFdcRbwOUcHgmjY1Y+CRjY
ZSY+TIIzNCTcxml9MFZ09A==
-----END PRIVATE KEY-----
";
