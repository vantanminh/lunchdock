//! Owner setup, sessions, TOTP, roles, API tokens, and the audit log.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use rudder_server::AppState;
use serde_json::{json, Value};
use sqlx::PgPool;
use totp_rs::{Builder, Secret};
use tower::ServiceExt;

fn test_config(database_url: &str) -> rudder_server::Config {
    let vars = HashMap::from([
        ("DATABASE_URL".into(), database_url.to_owned()),
        ("RUDDER_MASTER_KEY".into(), "bb".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_LOG_FORMAT".into(), "pretty".into()),
    ]);
    rudder_server::Config::from_map(&vars, |_| unreachable!("no key file")).expect("config")
}

fn app(pool: PgPool) -> axum::Router {
    let url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "postgres://localhost/rudder".into());
    rudder_server::router(AppState::new(pool, test_config(&url)))
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
    bearer: Option<&str>,
    body: Option<Value>,
) -> Reply {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", format!("rudder_session={cookie}"));
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-csrf-token", csrf);
    }
    if let Some(bearer) = bearer {
        builder = builder.header("authorization", format!("Bearer {bearer}"));
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
    let cookie = response
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .and_then(|pair| pair.strip_prefix("rudder_session="))
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
        cookie,
    }
}

fn session_of(reply: &Reply) -> (String, String) {
    let cookie = reply.cookie.clone().expect("session cookie");
    let csrf = reply.body["csrf_token"].as_str().expect("csrf").to_owned();
    (cookie, csrf)
}

#[sqlx::test(migrations = "./migrations")]
async fn setup_login_logout_and_csrf(pool: PgPool) {
    let app = app(pool);
    let status = call(&app, "GET", "/api/v1/setup/status", None, None, None, None).await;
    assert_eq!(status.status, StatusCode::OK);
    assert_eq!(status.body["needs_setup"], true);

    let created = call(
        &app,
        "POST",
        "/api/v1/setup",
        None,
        None,
        None,
        Some(json!({
            "email": "Owner@Example.com",
            "password": "correct-horse-battery",
            "display_name": "Owner"
        })),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK);
    assert_eq!(created.body["user"]["email"], "owner@example.com");
    assert_eq!(created.body["user"]["role"], "admin");
    let (cookie, csrf) = session_of(&created);

    let again = call(
        &app,
        "POST",
        "/api/v1/setup",
        None,
        None,
        None,
        Some(json!({
            "email": "other@example.com",
            "password": "correct-horse-battery"
        })),
    )
    .await;
    assert_eq!(again.status, StatusCode::CONFLICT);

    let me = call(&app, "GET", "/api/v1/me", Some(&cookie), None, None, None).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.body["user"]["display_name"], "Owner");

    let missing_csrf = call(
        &app,
        "POST",
        "/api/v1/me/password",
        Some(&cookie),
        None,
        None,
        Some(json!({
            "current_password": "correct-horse-battery",
            "new_password": "another-strong-secret"
        })),
    )
    .await;
    assert_eq!(missing_csrf.status, StatusCode::FORBIDDEN);
    assert_eq!(missing_csrf.body["error"]["code"], "csrf");

    let changed = call(
        &app,
        "POST",
        "/api/v1/me/password",
        Some(&cookie),
        Some(&csrf),
        None,
        Some(json!({
            "current_password": "correct-horse-battery",
            "new_password": "another-strong-secret"
        })),
    )
    .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);

    let stale = call(&app, "GET", "/api/v1/me", Some(&cookie), None, None, None).await;
    assert_eq!(stale.status, StatusCode::UNAUTHORIZED);

    let bad = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "not-the-password"
        })),
    )
    .await;
    assert_eq!(bad.status, StatusCode::UNAUTHORIZED);

    let login = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "another-strong-secret"
        })),
    )
    .await;
    assert_eq!(login.status, StatusCode::OK);
    let (cookie, csrf) = session_of(&login);
    let logout = call(
        &app,
        "POST",
        "/api/v1/logout",
        Some(&cookie),
        Some(&csrf),
        None,
        None,
    )
    .await;
    assert_eq!(logout.status, StatusCode::NO_CONTENT);
    let after = call(&app, "GET", "/api/v1/me", Some(&cookie), None, None, None).await;
    assert_eq!(after.status, StatusCode::UNAUTHORIZED);

    let spec = call(&app, "GET", "/api/openapi.json", None, None, None, None).await;
    assert_eq!(spec.status, StatusCode::OK);
    let text = spec.body.to_string();
    assert!(text.contains("/api/v1/login"));
    assert!(text.contains("Rudder API"));
}

#[sqlx::test(migrations = "./migrations")]
async fn totp_setup_enable_and_login(pool: PgPool) {
    let app = app(pool);
    let created = call(
        &app,
        "POST",
        "/api/v1/setup",
        None,
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "correct-horse-battery"
        })),
    )
    .await;
    let (cookie, csrf) = session_of(&created);
    let setup = call(
        &app,
        "POST",
        "/api/v1/me/totp/setup",
        Some(&cookie),
        Some(&csrf),
        None,
        None,
    )
    .await;
    assert_eq!(setup.status, StatusCode::OK);
    let secret = setup.body["secret"].as_str().unwrap();
    assert!(setup.body["otpauth_url"]
        .as_str()
        .unwrap()
        .starts_with("otpauth://totp/"));
    let code = current_code(secret, "owner@example.com");
    let enabled = call(
        &app,
        "POST",
        "/api/v1/me/totp/enable",
        Some(&cookie),
        Some(&csrf),
        None,
        Some(json!({ "code": code })),
    )
    .await;
    assert_eq!(enabled.status, StatusCode::NO_CONTENT);

    let needs_code = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "correct-horse-battery"
        })),
    )
    .await;
    assert_eq!(needs_code.status, StatusCode::UNAUTHORIZED);
    assert_eq!(needs_code.body["error"]["code"], "totp_required");

    let code = current_code(secret, "owner@example.com");
    let login = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "correct-horse-battery",
            "totp_code": code
        })),
    )
    .await;
    assert_eq!(login.status, StatusCode::OK);
    assert_eq!(login.body["user"]["totp_enabled"], true);
}

#[sqlx::test(migrations = "./migrations")]
async fn roles_and_api_tokens(pool: PgPool) {
    let app = app(pool);
    let created = call(
        &app,
        "POST",
        "/api/v1/setup",
        None,
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "correct-horse-battery"
        })),
    )
    .await;
    let (cookie, csrf) = session_of(&created);
    let viewer = call(
        &app,
        "POST",
        "/api/v1/users",
        Some(&cookie),
        Some(&csrf),
        None,
        Some(json!({
            "email": "viewer@example.com",
            "password": "viewer-password-ok",
            "role": "viewer"
        })),
    )
    .await;
    assert_eq!(viewer.status, StatusCode::CREATED);
    let viewer_id = viewer.body["id"].as_str().unwrap().to_owned();

    let viewer_login = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        None,
        None,
        Some(json!({
            "email": "viewer@example.com",
            "password": "viewer-password-ok"
        })),
    )
    .await;
    let (viewer_cookie, viewer_csrf) = session_of(&viewer_login);
    let forbidden = call(
        &app,
        "POST",
        "/api/v1/users",
        Some(&viewer_cookie),
        Some(&viewer_csrf),
        None,
        Some(json!({
            "email": "extra@example.com",
            "password": "extra-password-ok",
            "role": "viewer"
        })),
    )
    .await;
    assert_eq!(forbidden.status, StatusCode::FORBIDDEN);

    let read_token = call(
        &app,
        "POST",
        "/api/v1/tokens",
        Some(&cookie),
        Some(&csrf),
        None,
        Some(json!({ "name": "ci", "scopes": ["read"] })),
    )
    .await;
    assert_eq!(read_token.status, StatusCode::CREATED);
    let read = read_token.body["token"].as_str().unwrap().to_owned();
    assert!(read.starts_with("rdr_"));
    let listed = call(&app, "GET", "/api/v1/users", None, None, Some(&read), None).await;
    assert_eq!(listed.status, StatusCode::OK);
    let write_denied = call(
        &app,
        "DELETE",
        &format!("/api/v1/users/{viewer_id}"),
        None,
        None,
        Some(&read),
        None,
    )
    .await;
    assert_eq!(write_denied.status, StatusCode::FORBIDDEN);

    let write_token = call(
        &app,
        "POST",
        "/api/v1/tokens",
        Some(&cookie),
        Some(&csrf),
        None,
        Some(json!({ "name": "deploy", "scopes": ["read", "write"] })),
    )
    .await;
    let write = write_token.body["token"].as_str().unwrap();
    let removed = call(
        &app,
        "DELETE",
        &format!("/api/v1/users/{viewer_id}"),
        None,
        None,
        Some(write),
        None,
    )
    .await;
    assert_eq!(removed.status, StatusCode::NO_CONTENT);
}

#[sqlx::test(migrations = "./migrations")]
async fn audit_log_records_mutations(pool: PgPool) {
    let app = app(pool);
    let created = call(
        &app,
        "POST",
        "/api/v1/setup",
        None,
        None,
        None,
        Some(json!({
            "email": "owner@example.com",
            "password": "correct-horse-battery"
        })),
    )
    .await;
    let (cookie, csrf) = session_of(&created);
    let _ = call(
        &app,
        "POST",
        "/api/v1/tokens",
        Some(&cookie),
        Some(&csrf),
        None,
        Some(json!({ "name": "audit", "scopes": ["read"] })),
    )
    .await;
    let audit = call(
        &app,
        "GET",
        "/api/v1/audit",
        Some(&cookie),
        None,
        None,
        None,
    )
    .await;
    assert_eq!(audit.status, StatusCode::OK);
    let actions: Vec<&str> = audit
        .body
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["action"].as_str())
        .collect();
    assert!(actions.contains(&"user.setup"));
    assert!(actions.contains(&"token.create"));
    let leaked = audit.body.to_string();
    assert!(!leaked.contains("correct-horse-battery"));
    assert!(!leaked.contains("rdr_"));
}

fn current_code(secret_b32: &str, email: &str) -> String {
    let secret = Secret::try_from_base32(secret_b32).unwrap();
    let totp = Builder::new()
        .with_secret(secret)
        .with_account_name(email)
        .with_issuer(Some("Rudder"))
        .build()
        .unwrap();
    totp.generate_current().to_string()
}
