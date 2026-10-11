//! Certificates, domains, registries, and image deploy records.

use std::collections::HashMap;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::PgPool;
use tower::ServiceExt;

fn app(pool: PgPool, data_dir: &std::path::Path) -> axum::Router {
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
        builder = builder.header("cookie", format!("rudder_session={cookie}"));
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

async fn owner(app: &axum::Router) -> (String, String) {
    let created = call(
        app,
        "POST",
        "/api/v1/setup",
        None,
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

fn certificate(names: &[&str]) -> (String, String) {
    use rcgen::{CertificateParams, DistinguishedName, DnType, KeyPair};
    let key = KeyPair::generate().unwrap();
    let names: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
    let mut distinguished = DistinguishedName::new();
    distinguished.push(DnType::CommonName, names[0].clone());
    let mut params = CertificateParams::new(names).unwrap();
    params.distinguished_name = distinguished;
    let cert = params.self_signed(&key).unwrap();
    (cert.pem(), key.serialize_pem())
}

#[sqlx::test(migrations = "./migrations")]
async fn certificates_domains_registries_and_failed_deploy(pool: PgPool) {
    let data_dir: PathBuf =
        std::env::temp_dir().join(format!("rudder-edge-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&data_dir).unwrap();
    let sql = pool.clone();
    let app = app(pool, &data_dir);
    let (cookie, csrf) = owner(&app).await;

    let docs = call(&app, "GET", "/api/openapi.json", None, None, None).await;
    let document = docs.body.to_string();
    assert!(document.contains("/api/v1/certificates"));
    assert!(document.contains("/api/v1/services/{id}/deploy"));

    let server = call(
        &app,
        "POST",
        "/api/v1/servers",
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "local",
            "kind": "local",
            "public_ip": "203.0.113.10"
        })),
    )
    .await;
    assert_eq!(server.status, StatusCode::CREATED);
    let server_id = server.body["id"].as_str().unwrap();

    let project = call(
        &app,
        "POST",
        "/api/v1/projects",
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "web" })),
    )
    .await;
    let project_id = project.body["id"].as_str().unwrap();
    let environment = call(
        &app,
        "POST",
        &format!("/api/v1/projects/{project_id}/environments"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "name": "prod" })),
    )
    .await;
    let environment_id = environment.body["id"].as_str().unwrap();
    let service = call(
        &app,
        "POST",
        &format!("/api/v1/environments/{environment_id}/services"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "web",
            "kind": "application",
            "server_id": server_id,
            "spec": { "image": "nginx:1.27" }
        })),
    )
    .await;
    let service_id = service.body["id"].as_str().unwrap();

    let (cert_pem, key_pem) = certificate(&["example.com", "*.example.com"]);
    let (_, other_key) = certificate(&["example.com"]);
    let mismatch = call(
        &app,
        "POST",
        "/api/v1/certificates",
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "bad",
            "cert_pem": cert_pem.clone(),
            "key_pem": other_key
        })),
    )
    .await;
    assert_eq!(mismatch.status, StatusCode::BAD_REQUEST);

    let uploaded = call(
        &app,
        "POST",
        "/api/v1/certificates",
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "wildcard",
            "cert_pem": cert_pem.clone(),
            "key_pem": key_pem.clone()
        })),
    )
    .await;
    assert_eq!(uploaded.status, StatusCode::CREATED, "{}", uploaded.body);
    assert!(uploaded.body["sans"]
        .as_array()
        .unwrap()
        .iter()
        .any(|name| name == "*.example.com"));
    assert!(!uploaded.body.to_string().contains("PRIVATE KEY"));
    let certificate_id = uploaded.body["id"].as_str().unwrap();
    let cert_file = data_dir.join(format!("traefik/certs/{certificate_id}.crt"));
    let key_file = data_dir.join(format!("traefik/certs/{certificate_id}.key"));
    assert!(std::fs::read_to_string(&cert_file)
        .unwrap()
        .contains("BEGIN CERTIFICATE"));
    assert!(std::fs::read_to_string(&key_file)
        .unwrap()
        .contains("PRIVATE KEY"));

    let matched = call(
        &app,
        "GET",
        "/api/v1/certificates/match?host=www.example.com",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(matched.status, StatusCode::OK);
    assert_eq!(matched.body["name"], "wildcard");
    let apex = call(
        &app,
        "GET",
        "/api/v1/certificates/match?host=example.com",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(apex.body["name"], "wildcard");
    let nested = call(
        &app,
        "GET",
        "/api/v1/certificates/match?host=a.b.example.com",
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert!(nested.body.is_null());

    let domain = call(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/domains"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "host": "www.example.com",
            "path_prefix": "/api",
            "target_port": 8080,
            "ssl_mode": "custom",
            "https_redirect": true
        })),
    )
    .await;
    assert_eq!(domain.status, StatusCode::CREATED, "{}", domain.body);
    assert_eq!(domain.body["matched_certificate_name"], "wildcard");
    assert_eq!(domain.body["published"], true);
    let routes = std::fs::read_to_string(data_dir.join("traefik/dynamic/routes.yml")).unwrap();
    assert!(routes.contains("Host(`www.example.com`) && PathPrefix(`/api`)"));
    assert!(routes.contains("tls: {}"));
    assert!(routes.contains(&format!("/etc/rudder/traefik/certs/{certificate_id}.crt")));

    let letsencrypt = call(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/domains"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "host": "app.example.com",
            "target_port": 8080,
            "ssl_mode": "letsencrypt"
        })),
    )
    .await;
    assert_eq!(
        letsencrypt.status,
        StatusCode::CREATED,
        "{}",
        letsencrypt.body
    );
    let routes = std::fs::read_to_string(data_dir.join("traefik/dynamic/routes.yml")).unwrap();
    assert!(routes.contains("certResolver: letsencrypt"));

    let preview = call(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/preview-domain"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "target_port": 8080 })),
    )
    .await;
    assert_eq!(preview.status, StatusCode::CREATED, "{}", preview.body);
    let preview_host = preview.body["host"].as_str().unwrap();
    assert!(preview_host.ends_with(".203.0.113.10.sslip.io"));
    assert_eq!(preview.body["is_preview"], true);

    let registry = call(
        &app,
        "POST",
        "/api/v1/registries",
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "name": "ghcr",
            "url": "https://ghcr.io",
            "username": "octocat",
            "password": "registry-secret-value"
        })),
    )
    .await;
    assert_eq!(registry.status, StatusCode::CREATED, "{}", registry.body);
    assert!(!registry.body.to_string().contains("registry-secret-value"));
    let registry_id = registry.body["id"].as_str().unwrap();
    let listed = call(&app, "GET", "/api/v1/registries", Some(&cookie), None, None).await;
    assert_eq!(listed.body[0]["has_password"], true);
    assert!(!listed.body.to_string().contains("registry-secret-value"));

    let deployed = call(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/deploy"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "image": "ghcr.io/acme/web:1",
            "registry_id": registry_id,
            "health_kind": "http",
            "health_path": "/health",
            "health_port": 8080,
            "health_timeout_secs": 1
        })),
    )
    .await;
    assert_eq!(
        deployed.status,
        StatusCode::BAD_GATEWAY,
        "{}",
        deployed.body
    );
    assert!(!deployed.body.to_string().contains("registry-secret-value"));
    let deployments = call(
        &app,
        "GET",
        &format!("/api/v1/services/{service_id}/deployments"),
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert_eq!(deployments.body[0]["status"], "failed");
    assert!(!deployments
        .body
        .to_string()
        .contains("registry-secret-value"));
    assert!(!deployments.body.to_string().contains("postgres"));

    sqlx::query(
        "INSERT INTO deployments (service_id, status, trigger, image_ref, finished_at)
         VALUES ($1, 'succeeded', 'manual', 'nginx:1.25', now())",
    )
    .bind(uuid::Uuid::parse_str(service_id).unwrap())
    .execute(&sql)
    .await
    .unwrap();
    let succeeded = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM deployments WHERE service_id = $1 AND status = 'succeeded'",
    )
    .bind(uuid::Uuid::parse_str(service_id).unwrap())
    .fetch_one(&sql)
    .await
    .unwrap();
    let rolled = call(
        &app,
        "POST",
        &format!("/api/v1/services/{service_id}/rollback"),
        Some(&cookie),
        Some(&csrf),
        Some(json!({ "deployment_id": succeeded })),
    )
    .await;
    assert_eq!(rolled.status, StatusCode::BAD_GATEWAY, "{}", rolled.body);
    let after = call(
        &app,
        "GET",
        &format!("/api/v1/services/{service_id}/deployments"),
        Some(&cookie),
        None,
        None,
    )
    .await;
    assert!(after
        .body
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["image_ref"] == "nginx:1.25" && row["trigger"] == "rollback"));

    let viewer = call(
        &app,
        "POST",
        "/api/v1/users",
        Some(&cookie),
        Some(&csrf),
        Some(json!({
            "email": "viewer@example.com",
            "password": "viewer-password-ok",
            "role": "viewer"
        })),
    )
    .await;
    assert_eq!(viewer.status, StatusCode::CREATED);
    let login = call(
        &app,
        "POST",
        "/api/v1/login",
        None,
        None,
        Some(json!({
            "email": "viewer@example.com",
            "password": "viewer-password-ok"
        })),
    )
    .await;
    let denied = call(
        &app,
        "POST",
        "/api/v1/certificates",
        login.cookie.as_deref(),
        login.body["csrf_token"].as_str(),
        Some(json!({
            "name": "nope",
            "cert_pem": cert_pem,
            "key_pem": key_pem
        })),
    )
    .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let _ = std::fs::remove_dir_all(&data_dir);
}
