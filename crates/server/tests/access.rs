//! Rudder Access sign-in, the edge gate, and owner protection around it.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rudder_core::Cipher;
use rudder_server::AppState;
use serde_json::{json, Value};
use sqlx::PgPool;
use tokio::net::TcpListener;
use tower::ServiceExt;

fn config(database_url: &str, siteverify: &str) -> rudder_server::Config {
    let vars = HashMap::from([
        ("DATABASE_URL".into(), database_url.to_owned()),
        ("RUDDER_MASTER_KEY".into(), "bb".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_LOG_FORMAT".into(), "pretty".into()),
        (
            "RUDDER_PUBLIC_URL".into(),
            "https://panel.example.com".into(),
        ),
        (
            "RUDDER_TURNSTILE_SITEVERIFY_URL".into(),
            siteverify.to_owned(),
        ),
    ]);
    rudder_server::Config::from_map(&vars, |_| unreachable!("no key file")).unwrap()
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
        builder = builder.header("cookie", cookie);
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
    let set_cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
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
        cookie: set_cookie,
    }
}

async fn turnstile() -> String {
    let app = axum::Router::new().route(
        "/siteverify",
        axum::routing::post(|| async {
            axum::Json(json!({
                "success": true,
                "action": "access_login",
                "hostname": "access.example.com"
            }))
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{address}/siteverify")
}

#[sqlx::test(migrations = "./migrations")]
async fn access_blocks_unknown_people_and_limits_a_host(pool: PgPool) {
    let siteverify = turnstile().await;
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://rudder:rudder@127.0.0.1:5432/rudder".into());
    let app = rudder_server::router(AppState::new(pool.clone(), config(&url, &siteverify)));

    let setup = call(
        &app,
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
    assert_eq!(setup.status, StatusCode::OK);
    let panel_cookie = setup.cookie.unwrap();
    let csrf = setup.body["csrf_token"].as_str().unwrap();

    let settings = call(
        &app,
        "PUT",
        "/api/v1/access/settings",
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({
            "turnstile_site_key": "0xSITE",
            "turnstile_secret": "secret-value",
            "turnstile_hostnames": ["access.example.com"],
            "email_provider": "none"
        })),
    )
    .await;
    assert_eq!(settings.status, StatusCode::OK, "{}", settings.body);
    assert!(settings.body["has_turnstile_secret"].as_bool().unwrap());
    assert_eq!(
        settings.body["google_redirect_uri"],
        "https://panel.example.com/api/v1/access/login/google/callback"
    );
    let missing = gate(&app, "00", "").await;
    assert_eq!(missing, StatusCode::NOT_FOUND);
    let _ = gate_secret(&pool).await;

    let created = call(
        &app,
        "POST",
        "/api/v1/access/identities",
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({ "email": "accountant@example.com", "display_name": "Ada", "admin": true })),
    )
    .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.body);
    assert!(created.body["owner"].as_bool().unwrap());

    let blocked = call(
        &app,
        "POST",
        "/api/v1/access/login/email",
        None,
        None,
        Some(json!({
            "email": "stranger@example.com",
            "turnstile_token": "token"
        })),
    )
    .await;
    assert_eq!(blocked.status, StatusCode::FORBIDDEN);
    assert_eq!(blocked.body["error"]["code"], "access_blocked");

    sqlx::query(
        "INSERT INTO access_codes (email, code_hash, purpose, expires_at)
         VALUES ($1, encode(digest($2, 'sha256'), 'hex'), 'login', now() + interval '10 minutes')",
    )
    .bind("accountant@example.com")
    .bind("123456")
    .execute(&pool)
    .await
    .unwrap();

    let verified = call(
        &app,
        "POST",
        "/api/v1/access/login/email/verify",
        None,
        None,
        Some(json!({
            "email": "accountant@example.com",
            "code": "123456",
            "turnstile_token": "token"
        })),
    )
    .await;
    assert_eq!(verified.status, StatusCode::OK, "{}", verified.body);
    let access_cookie = verified.cookie.unwrap();
    assert!(access_cookie.starts_with("rudder_access="));

    let me = call(
        &app,
        "GET",
        "/api/v1/access/me",
        Some(&access_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(me.status, StatusCode::OK, "{}", me.body);
    assert_eq!(me.body["email"], "accountant@example.com");
    assert!(me.body["admin"].as_bool().unwrap());

    let server = call(
        &app,
        "POST",
        "/api/v1/servers",
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({ "name": "local", "kind": "local" })),
    )
    .await;
    let project = call(
        &app,
        "POST",
        "/api/v1/projects",
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({ "name": "finance" })),
    )
    .await;
    let environment = call(
        &app,
        "POST",
        &format!(
            "/api/v1/projects/{}/environments",
            project.body["id"].as_str().unwrap()
        ),
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({ "name": "prod" })),
    )
    .await;
    let service = call(
        &app,
        "POST",
        &format!("/api/v1/environments/{}/services", environment.body["id"].as_str().unwrap()),
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({
            "name": "invoices",
            "kind": "application",
            "server_id": server.body["id"],
            "spec": { "image": "ghcr.io/acme/invoices:1", "ports": [{ "container": 80, "host": 80 }] }
        })),
    )
    .await;
    let service_id = service.body["id"].as_str().unwrap();
    let network = call(
        &app,
        "PATCH",
        &format!("/api/v1/services/{service_id}/network"),
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({ "isolation": true })),
    )
    .await;
    assert_eq!(network.status, StatusCode::OK, "{}", network.body);

    let policy = call(
        &app,
        "PUT",
        &format!("/api/v1/access/services/{service_id}"),
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({
            "enabled": true,
            "rules": [{ "kind": "email", "email": "accountant@example.com" }]
        })),
    )
    .await;
    assert_eq!(policy.status, StatusCode::OK, "{}", policy.body);

    let apps = call(
        &app,
        "GET",
        "/api/v1/access/apps",
        Some(&access_cookie),
        None,
        None,
    )
    .await;
    assert_eq!(apps.status, StatusCode::OK, "{}", apps.body);
    assert_eq!(apps.body[0]["name"], "invoices");

    let rule = call(
        &app,
        "POST",
        "/api/v1/security/rate-limits",
        Some(&panel_cookie),
        Some(csrf),
        Some(json!({
            "name": "invoices",
            "host": "invoices.example.com",
            "limit_count": 1,
            "window_secs": 60,
            "key_kind": "ip"
        })),
    )
    .await;
    assert_eq!(rule.status, StatusCode::CREATED, "{}", rule.body);

    let secret = gate_secret(&pool).await;
    assert_eq!(secret.len(), 64);
    let first = gate(&app, &secret, &access_cookie).await;
    assert_eq!(first, StatusCode::OK);
    let second = gate(&app, &secret, &access_cookie).await;
    assert_eq!(second, StatusCode::TOO_MANY_REQUESTS);
}

async fn gate(app: &axum::Router, secret: &str, cookie: &str) -> StatusCode {
    let request = Request::builder()
        .uri(format!("/api/v1/edge/gate?k={secret}"))
        .header("x-forwarded-host", "invoices.example.com")
        .header("x-forwarded-uri", "/")
        .header("x-forwarded-method", "GET")
        .header("x-forwarded-for", "203.0.113.8")
        .header("cookie", cookie)
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

async fn gate_secret(pool: &PgPool) -> String {
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT gate_secret_enc FROM access_settings WHERE id")
        .fetch_one(pool)
        .await
        .unwrap();
    let cipher = Cipher::new([0xbb; 32]);
    String::from_utf8(cipher.decrypt(&bytes).unwrap()).unwrap()
}
