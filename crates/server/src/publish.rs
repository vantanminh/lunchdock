//! Write Traefik's dynamic file and the certificate PEMs it reads.

use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use rudder_core::{
    render_dynamic_config, NodeDriver, ProxyRoute, TlsMaterial, TlsMode, TRAEFIK_CERT_DIR,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::routes::catalog::{bad, core_error};
use crate::state::AppState;

struct RouteRow {
    id: Uuid,
    host: String,
    path_prefix: String,
    target_port: i32,
    https_redirect: bool,
    ssl_mode: String,
    basic_auth_user: Option<String>,
    basic_auth_hash: Option<String>,
    service_name: String,
    container_name: Option<String>,
    compose_service: Option<String>,
    stack_container: Option<String>,
    project: Option<String>,
}

struct PanelRoute {
    panel_domain: Option<String>,
    panel_ssl_mode: String,
    panel_certificate_id: Option<Uuid>,
}

struct CertBlob {
    id: Uuid,
    cert_pem_enc: Vec<u8>,
    key_pem_enc: Vec<u8>,
    chain_pem_enc: Option<Vec<u8>>,
}

/// Refresh the local Traefik file. Remote servers wait for the agent.
pub async fn publish_server(state: &AppState, server_id: Uuid) -> Result<bool, ApiError> {
    let kind = sqlx::query_scalar!(r#"SELECT kind FROM servers WHERE id = $1"#, server_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let Some(kind) = kind else {
        return Err(crate::routes::catalog::not_found("server not found"));
    };
    if kind != "local" {
        tracing::info!(%server_id, "routing change stored; the agent applies it when connected");
        return Ok(false);
    }
    let yaml = render_for_server(&state.pool, server_id, state.config.bind.port()).await?;
    let materials = write_certificates(state).await?;
    let document = render_dynamic_config(&yaml.0, &materials).map_err(core_error)?;
    let mut body = document;
    if !yaml.1.is_empty() {
        let _ = write!(body, "\n# panel certificate {}\n", yaml.1);
    }
    state
        .local_driver
        .write_file("traefik/dynamic/routes.yml", body.as_bytes())
        .await
        .map_err(core_error)?;
    Ok(true)
}

async fn render_for_server(
    pool: &PgPool,
    server_id: Uuid,
    panel_port: u16,
) -> Result<(Vec<ProxyRoute>, String), ApiError> {
    let rows = sqlx::query_as!(
        RouteRow,
        r#"
        SELECT d.id,
               d.host,
               d.path_prefix,
               d.target_port,
               d.https_redirect,
               d.ssl_mode,
               d.basic_auth_user,
               d.basic_auth_hash,
               s.name AS service_name,
               d.compose_service,
               s.spec -> 'containers' ->> d.compose_service AS "stack_container?",
               s.spec ->> 'project' AS "project?",
               (
                   SELECT sc.container_name
                   FROM service_containers sc
                   WHERE sc.service_id = s.id AND sc.state = 'running'
                   ORDER BY sc.generation DESC
                   LIMIT 1
               ) AS "container_name?"
        FROM domains d
        JOIN services s ON s.id = d.service_id
        WHERE s.server_id = $1
        ORDER BY d.host, d.path_prefix
        "#,
        server_id
    )
    .fetch_all(pool)
    .await
    .map_err(ApiError::database)?;
    let mut routes = Vec::new();
    for row in rows {
        routes.push(route_from_row(row)?);
    }
    let panel = sqlx::query_as!(
        PanelRoute,
        r#"
        SELECT panel_domain, panel_ssl_mode, panel_certificate_id
        FROM panel_settings
        WHERE id = TRUE
        "#
    )
    .fetch_one(pool)
    .await
    .map_err(ApiError::database)?;
    let note = panel
        .panel_certificate_id
        .map(|id| id.to_string())
        .unwrap_or_default();
    if let Some(host) = panel.panel_domain.filter(|host| !host.is_empty()) {
        let local = sqlx::query_scalar!(r#"SELECT kind FROM servers WHERE id = $1"#, server_id)
            .fetch_one(pool)
            .await
            .map_err(ApiError::database)?;
        if local == "local" {
            routes.push(ProxyRoute {
                name: "panel".into(),
                host,
                path_prefix: "/".into(),
                container: "rudder".into(),
                port: panel_port,
                https_redirect: panel.panel_ssl_mode != "none",
                tls: tls_mode(&panel.panel_ssl_mode)?,
                basic_auth_user: None,
                basic_auth_hash: None,
            });
        }
    }
    Ok((routes, note))
}

fn route_from_row(row: RouteRow) -> Result<ProxyRoute, ApiError> {
    let port = u16::try_from(row.target_port)
        .map_err(|_| bad("invalid", "target port is out of range"))?;
    let container = match (row.compose_service, row.stack_container, row.project) {
        (Some(_), Some(container), _) => container,
        (Some(service), None, Some(project)) => format!("{project}-{service}-1"),
        _ => row.container_name.unwrap_or(row.service_name),
    };
    Ok(ProxyRoute {
        name: router_name(row.id),
        host: row.host,
        path_prefix: row.path_prefix,
        container,
        port,
        https_redirect: row.https_redirect,
        tls: tls_mode(&row.ssl_mode)?,
        basic_auth_user: row.basic_auth_user,
        basic_auth_hash: row.basic_auth_hash,
    })
}

pub(crate) fn router_name(id: Uuid) -> String {
    let compact = id.to_string().replace('-', "");
    format!("r{}", &compact[..12])
}

pub(crate) fn tls_mode(value: &str) -> Result<TlsMode, ApiError> {
    match value {
        "letsencrypt" => Ok(TlsMode::LetsEncrypt),
        "custom" => Ok(TlsMode::Custom),
        "none" => Ok(TlsMode::None),
        _ => Err(bad(
            "invalid",
            "ssl mode must be letsencrypt, custom, or none",
        )),
    }
}

async fn write_certificates(state: &AppState) -> Result<Vec<TlsMaterial>, ApiError> {
    let rows = sqlx::query_as!(
        CertBlob,
        r#"
        SELECT id, cert_pem_enc, key_pem_enc, chain_pem_enc
        FROM certificates
        ORDER BY created_at
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let mut materials = Vec::new();
    for row in rows {
        let cert = decrypt_pem(&state.cipher, &row.cert_pem_enc)?;
        let key = decrypt_pem(&state.cipher, &row.key_pem_enc)?;
        let mut full = cert;
        if let Some(chain) = row.chain_pem_enc {
            let chain = decrypt_pem(&state.cipher, &chain)?;
            if !full.ends_with('\n') {
                full.push('\n');
            }
            full.push_str(&chain);
        }
        let cert_rel = format!("traefik/certs/{}.crt", row.id);
        let key_rel = format!("traefik/certs/{}.key", row.id);
        state
            .local_driver
            .write_file(&cert_rel, full.as_bytes())
            .await
            .map_err(core_error)?;
        state
            .local_driver
            .write_file(&key_rel, key.as_bytes())
            .await
            .map_err(core_error)?;
        materials.push(TlsMaterial {
            cert_file: format!("{TRAEFIK_CERT_DIR}/{}.crt", row.id),
            key_file: format!("{TRAEFIK_CERT_DIR}/{}.key", row.id),
        });
    }
    Ok(materials)
}

fn decrypt_pem(cipher: &rudder_core::Cipher, blob: &[u8]) -> Result<String, ApiError> {
    let bytes = cipher.decrypt(blob).map_err(|_| {
        ApiError::new(
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "crypto_error",
            "stored certificate could not be decrypted",
        )
    })?;
    String::from_utf8(bytes).map_err(|_| bad("invalid", "stored certificate is not UTF-8"))
}

pub fn expiry_unix_to_time(unix: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(unix, 0).unwrap_or(DateTime::UNIX_EPOCH)
}
