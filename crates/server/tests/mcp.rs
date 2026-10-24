//! Agent login: approval in the browser, rotating refresh tokens, and MCP tools.

use std::collections::HashMap;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use http_body_util::BodyExt;
use rudder_server::AppState;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tower::ServiceExt;

fn test_config(database_url: &str) -> rudder_server::Config {
    let vars = HashMap::from([
        ("DATABASE_URL".into(), database_url.to_owned()),
        ("RUDDER_MASTER_KEY".into(), "cc".repeat(32)),
        ("RUDDER_COOKIE_SECURE".into(), "false".into()),
        ("RUDDER_LOG_FORMAT".into(), "pretty".into()),
        ("RUDDER_PUBLIC_URL".into(), "https://panel.example".into()),
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
    headers: axum::http::HeaderMap,
}

#[allow(clippy::too_many_arguments)]
async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    cookie: Option<&str>,
    csrf: Option<&str>,
    bearer: Option<&str>,
    content_type: Option<&str>,
    body: Option<Vec<u8>>,
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
    if let Some(content_type) = content_type {
        builder = builder.header("content-type", content_type);
    }
    let request = builder.body(Body::from(body.unwrap_or_default())).unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    Reply {
        status,
        body,
        headers,
    }
}

fn session_of(reply: &Reply) -> (String, String) {
    let cookie = reply
        .headers
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .and_then(|pair| pair.strip_prefix("rudder_session="))
        .unwrap()
        .to_owned();
    let csrf = reply.body["csrf_token"].as_str().unwrap().to_owned();
    (cookie, csrf)
}

fn s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn code_from(redirect: &str) -> String {
    url::Url::parse(redirect)
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .into_owned()
}

async fn owner(app: &axum::Router) -> (String, String) {
    let created = call(
        app,
        "POST",
        "/api/v1/setup",
        None,
        None,
        None,
        Some("application/json"),
        Some(
            json!({
                "email": "owner@example.com",
                "password": "correct-horse-battery",
                "display_name": "Owner"
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{:?}", created.body);
    session_of(&created)
}

#[sqlx::test(migrations = "./migrations")]
async fn agent_login_refreshes_and_can_control_the_panel(pool: PgPool) {
    let app = app(pool);
    let meta = call(
        &app,
        "GET",
        "/.well-known/oauth-authorization-server",
        None,
        None,
        None,
        None,
        None,
    )
    .await;
    assert_eq!(meta.status, StatusCode::OK);
    assert_eq!(
        meta.body["authorization_endpoint"],
        "https://panel.example/oauth/authorize"
    );
    assert!(meta.headers.get("cache-control").is_some());

    let registered = call(
        &app,
        "POST",
        "/oauth/register",
        None,
        None,
        None,
        Some("application/json"),
        Some(
            json!({
                "client_name": "Cursor",
                "redirect_uris": ["http://127.0.0.1:9/callback"],
                "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"]
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(
        registered.status,
        StatusCode::CREATED,
        "{:?}",
        registered.body
    );
    let client_id = registered.body["client_id"].as_str().unwrap();

    let bad = call(
        &app,
        "POST",
        "/oauth/register",
        None,
        None,
        None,
        Some("application/json"),
        Some(
            json!({"client_name": "Bad", "redirect_uris": ["http://evil.example/callback"]})
                .to_string()
                .into_bytes(),
        ),
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);

    let verifier = "a".repeat(64);
    let challenge = s256(&verifier);
    let start = format!(
        "/oauth/authorize?response_type=code&client_id={client_id}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state=xyz&resource=https://panel.example/mcp",
        percent("http://127.0.0.1:9/callback")
    );
    let anonymous = call(&app, "GET", &start, None, None, None, None, None).await;
    assert_eq!(anonymous.status, StatusCode::TEMPORARY_REDIRECT);
    let location = anonymous.headers.get("location").unwrap().to_str().unwrap();
    assert!(location.starts_with("/login?next="), "{location}");

    let (cookie, csrf) = owner(&app).await;
    let signed = call(&app, "GET", &start, Some(&cookie), None, None, None, None).await;
    assert_eq!(signed.status, StatusCode::TEMPORARY_REDIRECT);
    assert!(signed
        .headers
        .get("location")
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("/mcp/consent?"));

    let approved = call(
        &app,
        "POST",
        "/api/v1/mcp/consent",
        Some(&cookie),
        Some(&csrf),
        None,
        Some("application/json"),
        Some(
            json!({
                "client_id": client_id,
                "redirect_uri": "http://127.0.0.1:9/callback",
                "code_challenge": challenge,
                "code_challenge_method": "S256",
                "state": "xyz",
                "template": "full",
                "name": "Laptop"
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    assert_eq!(approved.status, StatusCode::OK, "{:?}", approved.body);
    let code = code_from(approved.body["redirect"].as_str().unwrap());

    let wrong = token_request(
        &app,
        &format!(
            "grant_type=authorization_code&code={code}&redirect_uri={}&client_id={client_id}&code_verifier={}",
            percent("http://127.0.0.1:9/callback"),
            "b".repeat(64)
        ),
    )
    .await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    assert_eq!(wrong.body["error"], "invalid_grant");

    let approved = call(
        &app,
        "POST",
        "/api/v1/mcp/consent",
        Some(&cookie),
        Some(&csrf),
        None,
        Some("application/json"),
        Some(
            json!({
                "client_id": client_id,
                "redirect_uri": "http://127.0.0.1:9/callback",
                "code_challenge": challenge,
                "code_challenge_method": "S256",
                "state": "xyz",
                "template": "full"
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    let code = code_from(approved.body["redirect"].as_str().unwrap());
    let tokens = token_request(
        &app,
        &format!(
            "grant_type=authorization_code&code={code}&redirect_uri={}&client_id={client_id}&code_verifier={verifier}",
            percent("http://127.0.0.1:9/callback")
        ),
    )
    .await;
    assert_eq!(tokens.status, StatusCode::OK, "{:?}", tokens.body);
    let access = tokens.body["access_token"].as_str().unwrap().to_owned();
    let refresh = tokens.body["refresh_token"].as_str().unwrap().to_owned();
    assert!(access.starts_with("rdr_mca_"));
    assert_eq!(tokens.body["expires_in"], 900);
    assert!(tokens.body["scope"]
        .as_str()
        .unwrap()
        .contains("auth:write"));

    let listed = rpc(
        &app,
        &access,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK);
    let names: Vec<&str> = listed.body["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"rudder_whoami"));
    assert!(names.contains(&"get_v1_projects"));
    assert!(names.contains(&"post_v1_projects"));
    assert!(!names.contains(&"post_v1_login"));

    let me = rpc(
        &app,
        &access,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "get_v1_me", "arguments": {}}
        }),
    )
    .await;
    let text = me.body["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("owner@example.com"), "{text}");
    assert_eq!(me.body["result"]["isError"], false);

    let created = rpc(
        &app,
        &access,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "post_v1_projects",
                "arguments": {"body": {"name": "from-agent"}}
            }
        }),
    )
    .await;
    let text = created.body["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert!(text.starts_with("HTTP 201"), "{text}");

    let refreshed = token_request(
        &app,
        &format!("grant_type=refresh_token&refresh_token={refresh}&client_id={client_id}"),
    )
    .await;
    assert_eq!(refreshed.status, StatusCode::OK, "{:?}", refreshed.body);
    let refresh_2 = refreshed.body["refresh_token"].as_str().unwrap().to_owned();
    let access_2 = refreshed.body["access_token"].as_str().unwrap().to_owned();

    let reused = token_request(
        &app,
        &format!("grant_type=refresh_token&refresh_token={refresh}&client_id={client_id}"),
    )
    .await;
    assert_eq!(reused.status, StatusCode::BAD_REQUEST);
    assert_eq!(reused.body["error"], "invalid_grant");
    let after = token_request(
        &app,
        &format!("grant_type=refresh_token&refresh_token={refresh_2}&client_id={client_id}"),
    )
    .await;
    assert_eq!(after.status, StatusCode::BAD_REQUEST);

    let dead = rpc(
        &app,
        &access_2,
        json!({"jsonrpc": "2.0", "id": 4, "method": "ping"}),
    )
    .await;
    assert_eq!(dead.status, StatusCode::UNAUTHORIZED);

    let watch = call(
        &app,
        "POST",
        "/api/v1/mcp/consent",
        Some(&cookie),
        Some(&csrf),
        None,
        Some("application/json"),
        Some(
            json!({
                "client_id": client_id,
                "redirect_uri": "http://127.0.0.1:9/callback",
                "code_challenge": challenge,
                "code_challenge_method": "S256",
                "state": "watch",
                "template": "read-all"
            })
            .to_string()
            .into_bytes(),
        ),
    )
    .await;
    let code = code_from(watch.body["redirect"].as_str().unwrap());
    let limited = token_request(
        &app,
        &format!(
            "grant_type=authorization_code&code={code}&redirect_uri={}&client_id={client_id}&code_verifier={verifier}",
            percent("http://127.0.0.1:9/callback")
        ),
    )
    .await;
    let limited_access = limited.body["access_token"].as_str().unwrap();
    assert!(!limited.body["scope"].as_str().unwrap().contains(":write"));
    let denied = rpc(
        &app,
        limited_access,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "post_v1_projects",
                "arguments": {"body": {"name": "nope"}}
            }
        }),
    )
    .await;
    let text = denied.body["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    assert_eq!(denied.body["result"]["isError"], true, "{text}");

    let missing = rpc(
        &app,
        "",
        json!({"jsonrpc": "2.0", "id": 6, "method": "initialize", "params": {}}),
    )
    .await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    assert!(missing
        .headers
        .get("www-authenticate")
        .unwrap()
        .to_str()
        .unwrap()
        .contains("oauth-protected-resource"));
}

fn percent(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

async fn token_request(app: &axum::Router, form: &str) -> Reply {
    call(
        app,
        "POST",
        "/oauth/token",
        None,
        None,
        None,
        Some("application/x-www-form-urlencoded"),
        Some(form.as_bytes().to_vec()),
    )
    .await
}

async fn rpc(app: &axum::Router, bearer: &str, body: Value) -> Reply {
    call(
        app,
        "POST",
        "/mcp",
        None,
        None,
        if bearer.is_empty() {
            None
        } else {
            Some(bearer)
        },
        Some("application/json"),
        Some(body.to_string().into_bytes()),
    )
    .await
}
