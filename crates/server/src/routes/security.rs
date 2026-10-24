//! Security center: SSL activity, exposure checks, and shared rate limits.
//!
//! Checks run only against domains this panel routes. A finding says that a
//! URL answered; the response body is not stored.

use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::get;
use axum::Json;
use chrono::{DateTime, Utc};
use rudder_core::{
    classify_probe, host_is_ip, validate_host, validate_path_prefix, validate_rate_rule, RateKey,
    RateRule, PROBES,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::auth::AuthUser;
use crate::error::ApiError;
use crate::routes::catalog::{bad, not_found};
use crate::state::AppState;

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route("/api/v1/security/overview", get(overview))
        .route("/api/v1/security/ssl", get(ssl_summary))
        .route("/api/v1/security/ssl/events", get(ssl_events))
        .route("/api/v1/security/scans", get(list_scans).post(start_scan))
        .route("/api/v1/security/findings", get(findings))
        .route(
            "/api/v1/security/rate-limits",
            get(list_rules).post(create_rule),
        )
        .route(
            "/api/v1/security/rate-limits/{id}",
            axum::routing::patch(update_rule).delete(delete_rule),
        )
        .route("/api/v1/security/rate-limits/stats", get(rate_stats))
        .route("/api/v1/security/edge", get(edge_stats))
}

#[derive(Serialize, ToSchema)]
pub struct SecurityOverview {
    pub certificates: i64,
    pub expiring: i64,
    pub expired: i64,
    pub open_findings: i64,
    pub http_requests: i64,
    pub ip_requests: i64,
    pub rate_rules: i64,
}

#[derive(Serialize, ToSchema)]
pub struct SslCertificateStat {
    pub id: Uuid,
    pub name: String,
    pub not_after: Option<DateTime<Utc>>,
    pub days_left: Option<i64>,
    pub domains: i64,
}

#[derive(Serialize, ToSchema)]
pub struct SslSummary {
    pub letsencrypt: i64,
    pub custom: i64,
    pub none: i64,
    pub certificates: Vec<SslCertificateStat>,
}

#[derive(Serialize, ToSchema)]
pub struct SslEventView {
    pub id: i64,
    pub host: Option<String>,
    pub kind: String,
    pub summary: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct SecurityScanView {
    pub id: Uuid,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct SecurityFindingView {
    pub id: Uuid,
    pub host: String,
    pub kind: String,
    pub severity: String,
    pub path: Option<String>,
    pub summary: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Serialize, ToSchema)]
pub struct RateLimitView {
    pub id: Uuid,
    pub name: String,
    pub enabled: bool,
    pub host: Option<String>,
    pub path_prefix: String,
    pub methods: Vec<String>,
    pub limit_count: i32,
    pub window_secs: i32,
    pub burst: i32,
    pub key_kind: String,
    pub header_name: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveRateLimit {
    pub name: String,
    pub enabled: Option<bool>,
    pub host: Option<String>,
    pub path_prefix: Option<String>,
    pub methods: Option<Vec<String>>,
    pub limit_count: i32,
    pub window_secs: i32,
    pub burst: Option<i32>,
    pub key_kind: String,
    pub header_name: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RateLimitStat {
    pub rule_id: Uuid,
    pub name: String,
    pub allowed: i64,
    pub blocked: i64,
}

#[derive(Serialize, ToSchema)]
pub struct EdgeStats {
    pub http_requests: i64,
    pub ip_requests: i64,
    pub recent_blocks: Vec<BlockView>,
}

#[derive(Serialize, ToSchema)]
pub struct BlockView {
    pub rule_id: Uuid,
    pub host: String,
    pub path: String,
    pub ip: String,
    pub created_at: DateTime<Utc>,
}

pub async fn record_ssl(
    pool: &PgPool,
    host: Option<&str>,
    certificate_id: Option<Uuid>,
    kind: &str,
    summary: &str,
) {
    let _ = sqlx::query!(
        r#"
        INSERT INTO ssl_events (host, certificate_id, kind, summary)
        VALUES ($1, $2, $3, $4)
        "#,
        host,
        certificate_id,
        kind,
        summary,
    )
    .execute(pool)
    .await;
}

async fn overview(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<SecurityOverview>, ApiError> {
    let certificates = sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM certificates"#)
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let expiring = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!" FROM certificates
        WHERE not_after IS NOT NULL AND not_after < now() + interval '14 days' AND not_after >= now()
        "#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let expired = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM certificates WHERE not_after IS NOT NULL AND not_after < now()"#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let open_findings = sqlx::query_scalar!(
        r#"
        SELECT COUNT(*) AS "count!"
        FROM security_findings f
        JOIN security_scans s ON s.id = f.scan_id
        WHERE s.id = (SELECT id FROM security_scans WHERE status = 'ok' ORDER BY started_at DESC LIMIT 1)
        "#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let edge = edge_row(&state).await?;
    let rate_rules =
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM rate_limit_rules WHERE enabled"#)
            .fetch_one(&state.pool)
            .await
            .map_err(ApiError::database)?;
    Ok(Json(SecurityOverview {
        certificates,
        expiring,
        expired,
        open_findings,
        http_requests: edge.0,
        ip_requests: edge.1,
        rate_rules,
    }))
}

async fn ssl_summary(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<SslSummary>, ApiError> {
    let letsencrypt = mode_count(&state, "letsencrypt").await?;
    let custom = mode_count(&state, "custom").await?;
    let none = mode_count(&state, "none").await?;
    let rows = sqlx::query!(
        r#"
        SELECT c.id, c.name, c.not_after,
               (SELECT COUNT(*) FROM domains d WHERE d.certificate_id = c.id) AS "domains!"
        FROM certificates c
        ORDER BY c.not_after NULLS LAST, c.name
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let now = Utc::now();
    Ok(Json(SslSummary {
        letsencrypt,
        custom,
        none,
        certificates: rows
            .into_iter()
            .map(|row| SslCertificateStat {
                id: row.id,
                name: row.name,
                not_after: row.not_after,
                days_left: row.not_after.map(|when| (when - now).num_days()),
                domains: row.domains,
            })
            .collect(),
    }))
}

async fn ssl_events(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<SslEventView>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT id, host, kind, summary, created_at
        FROM ssl_events
        ORDER BY created_at DESC
        LIMIT 100
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| SslEventView {
                id: row.id,
                host: row.host,
                kind: row.kind,
                summary: row.summary,
                created_at: row.created_at,
            })
            .collect(),
    ))
}

async fn list_scans(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<SecurityScanView>>, ApiError> {
    Ok(Json(scan_rows(&state).await?))
}

async fn start_scan(
    State(state): State<AppState>,
    auth: AuthUser,
) -> Result<(StatusCode, Json<SecurityScanView>), ApiError> {
    auth.require_write()?;
    let running = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM security_scans WHERE status = 'running' AND started_at > now() - interval '30 minutes') AS "running!""#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if running {
        return Err(crate::routes::catalog::conflict(
            "scan_running",
            "a security check is already running",
        ));
    }
    let id = sqlx::query_scalar!(
        r#"INSERT INTO security_scans (status) VALUES ('running') RETURNING id"#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let background = state.clone();
    tokio::spawn(async move {
        if let Err(error) = run_scan(&background, id).await {
            tracing::warn!(message = %error.public_message(), "security scan failed");
            let _ = sqlx::query!(
                r#"UPDATE security_scans SET status = 'failed', error = $2, finished_at = now() WHERE id = $1"#,
                id,
                error.public_message(),
            )
            .execute(&background.pool)
            .await;
        }
    });
    let view = scan_rows(&state)
        .await?
        .into_iter()
        .find(|scan| scan.id == id);
    Ok((
        StatusCode::ACCEPTED,
        Json(view.unwrap_or(SecurityScanView {
            id,
            status: "running".into(),
            started_at: Utc::now(),
            finished_at: None,
            error: None,
        })),
    ))
}

async fn findings(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<SecurityFindingView>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT f.id, f.host, f.kind, f.severity, f.path, f.summary, f.created_at
        FROM security_findings f
        WHERE f.scan_id = (
            SELECT id FROM security_scans WHERE status = 'ok' ORDER BY started_at DESC LIMIT 1
        )
        ORDER BY f.severity, f.host, f.path
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| SecurityFindingView {
                id: row.id,
                host: row.host,
                kind: row.kind,
                severity: row.severity,
                path: row.path,
                summary: row.summary,
                created_at: row.created_at,
            })
            .collect(),
    ))
}

async fn list_rules(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<RateLimitView>>, ApiError> {
    Ok(Json(rule_rows(&state).await?))
}

async fn create_rule(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SaveRateLimit>,
) -> Result<(StatusCode, Json<RateLimitView>), ApiError> {
    auth.require_write()?;
    let rule = clean_rule(&body)?;
    let id = Uuid::new_v4();
    sqlx::query!(
        r#"
        INSERT INTO rate_limit_rules (
            id, name, enabled, host, path_prefix, methods, limit_count, window_secs, burst, key_kind, header_name
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
        "#,
        id,
        rule.name,
        rule.enabled,
        rule.host,
        rule.path_prefix,
        &rule.methods,
        rule.limit,
        rule.window,
        rule.burst,
        rule.key,
        rule.header,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let _ = publish_all(&state).await;
    let view = rule_rows(&state)
        .await?
        .into_iter()
        .find(|item| item.id == id)
        .unwrap();
    Ok((StatusCode::CREATED, Json(view)))
}

async fn update_rule(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveRateLimit>,
) -> Result<Json<RateLimitView>, ApiError> {
    auth.require_write()?;
    let rule = clean_rule(&body)?;
    let updated = sqlx::query!(
        r#"
        UPDATE rate_limit_rules
        SET name = $2, enabled = $3, host = $4, path_prefix = $5, methods = $6,
            limit_count = $7, window_secs = $8, burst = $9, key_kind = $10, header_name = $11,
            updated_at = now()
        WHERE id = $1
        "#,
        id,
        rule.name,
        rule.enabled,
        rule.host,
        rule.path_prefix,
        &rule.methods,
        rule.limit,
        rule.window,
        rule.burst,
        rule.key,
        rule.header,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    if updated.rows_affected() == 0 {
        return Err(not_found("rate limit not found"));
    }
    let _ = publish_all(&state).await;
    Ok(Json(
        rule_rows(&state)
            .await?
            .into_iter()
            .find(|item| item.id == id)
            .unwrap(),
    ))
}

async fn delete_rule(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let deleted = sqlx::query!(r#"DELETE FROM rate_limit_rules WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if deleted.rows_affected() == 0 {
        return Err(not_found("rate limit not found"));
    }
    let _ = publish_all(&state).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn rate_stats(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<RateLimitStat>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT r.id, r.name,
               COALESCE(SUM(s.allowed), 0)::bigint AS "allowed!",
               COALESCE(SUM(s.blocked), 0)::bigint AS "blocked!"
        FROM rate_limit_rules r
        LEFT JOIN rate_limit_samples s ON s.rule_id = r.id AND s.bucket > now() - interval '24 hours'
        GROUP BY r.id, r.name
        ORDER BY r.name
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(
        rows.into_iter()
            .map(|row| RateLimitStat {
                rule_id: row.id,
                name: row.name,
                allowed: row.allowed,
                blocked: row.blocked,
            })
            .collect(),
    ))
}

async fn edge_stats(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<EdgeStats>, ApiError> {
    let (http_requests, ip_requests) = edge_row(&state).await?;
    let rows = sqlx::query!(
        r#"
        SELECT rule_id, host, path, ip, created_at
        FROM rate_limit_blocks
        ORDER BY created_at DESC
        LIMIT 50
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(Json(EdgeStats {
        http_requests,
        ip_requests,
        recent_blocks: rows
            .into_iter()
            .map(|row| BlockView {
                rule_id: row.rule_id,
                host: row.host,
                path: row.path,
                ip: row.ip,
                created_at: row.created_at,
            })
            .collect(),
    }))
}

struct CleanRule {
    name: String,
    enabled: bool,
    host: Option<String>,
    path_prefix: String,
    methods: Vec<String>,
    limit: i32,
    window: i32,
    burst: i32,
    key: String,
    header: Option<String>,
}

fn clean_rule(body: &SaveRateLimit) -> Result<CleanRule, ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 80 || name.chars().any(|ch| ch.is_control()) {
        return Err(bad("invalid", "name must be 1 to 80 characters"));
    }
    let host = body
        .host
        .as_deref()
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| host.to_ascii_lowercase())
        .transpose_ok()?;
    if let Some(host) = &host {
        validate_host(host).map_err(|_| bad("invalid", "host is not a hostname"))?;
    }
    let path_prefix = body.path_prefix.clone().unwrap_or_else(|| "/".into());
    validate_path_prefix(&path_prefix).map_err(|_| bad("invalid", "path prefix is not valid"))?;
    let methods = body
        .methods
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|method| method.to_ascii_uppercase())
        .collect::<Vec<_>>();
    let burst = body.burst.unwrap_or(0);
    let key = if body.key_kind == "header" {
        RateKey::Header
    } else if body.key_kind == "ip" {
        RateKey::Ip
    } else {
        return Err(bad("invalid", "key must be ip or header"));
    };
    let header = body.header_name.clone().unwrap_or_default();
    let draft = RateRule {
        id: "rule".into(),
        host: host.clone().unwrap_or_default(),
        path_prefix: path_prefix.clone(),
        methods: methods.clone(),
        limit: u32::try_from(body.limit_count).unwrap_or(0),
        window_secs: u32::try_from(body.window_secs).unwrap_or(0),
        burst: u32::try_from(burst).unwrap_or(u32::MAX),
        key,
        header_name: header.clone(),
    };
    validate_rate_rule(&draft).map_err(|error| bad("invalid", error.to_string()))?;
    Ok(CleanRule {
        name: name.to_owned(),
        enabled: body.enabled.unwrap_or(true),
        host,
        path_prefix,
        methods,
        limit: body.limit_count,
        window: body.window_secs,
        burst,
        key: body.key_kind.clone(),
        header: if header.is_empty() {
            None
        } else {
            Some(header)
        },
    })
}

trait TransposeOk {
    fn transpose_ok(self) -> Result<Option<String>, ApiError>;
}

impl TransposeOk for Option<String> {
    fn transpose_ok(self) -> Result<Option<String>, ApiError> {
        Ok(self)
    }
}

async fn run_scan(state: &AppState, id: Uuid) -> Result<(), ApiError> {
    let hosts = sqlx::query!(
        r#"
        SELECT DISTINCT d.host, d.ssl_mode, s.public_ip AS "public_ip?"
        FROM domains d
        JOIN services sv ON sv.id = d.service_id
        JOIN servers s ON s.id = sv.server_id
        WHERE NOT d.is_preview
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .redirect(reqwest::redirect::Policy::limited(2))
        .build()
        .map_err(|_| bad("invalid", "could not start the check"))?;
    for host in &hosts {
        check_host(&client, state, id, &host.host, &host.ssl_mode).await;
        if let Some(ip) = host.public_ip.as_deref().filter(|ip| host_is_ip(ip)) {
            check_ip(&client, state, id, &host.host, ip).await;
        }
    }
    let _ = note_expiring(state).await;
    sqlx::query!(
        r#"UPDATE security_scans SET status = 'ok', finished_at = now() WHERE id = $1"#,
        id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(())
}

async fn check_host(
    client: &reqwest::Client,
    state: &AppState,
    scan: Uuid,
    host: &str,
    ssl_mode: &str,
) {
    if ssl_mode != "none" {
        let url = format!("http://{host}/");
        if let Ok(response) = client.get(&url).send().await {
            let final_https = response.url().scheme() == "https";
            if response.status().is_success() && !final_https {
                insert_finding(
                    state,
                    scan,
                    host,
                    "http",
                    "medium",
                    Some("/"),
                    "This domain answers over plain HTTP. Turn on HTTPS redirect.",
                )
                .await;
            }
        }
    }
    for probe in PROBES {
        let url = format!("https://{host}{}", probe.path);
        let Ok(response) = client.get(&url).send().await else {
            continue;
        };
        if response.url().host_str() != Some(host) {
            continue;
        }
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let body = response.text().await.unwrap_or_default();
        let body = &body[..body.len().min(8192)];
        if let Some(finding) = classify_probe(probe, status, &content_type, body) {
            insert_finding(
                state,
                scan,
                host,
                probe.family,
                finding.severity,
                Some(probe.path),
                &finding.summary,
            )
            .await;
        }
    }
}

async fn check_ip(client: &reqwest::Client, state: &AppState, scan: Uuid, host: &str, ip: &str) {
    let url = format!("http://{ip}/");
    let Ok(response) = client.get(&url).header("host", host).send().await else {
        return;
    };
    if response.status().is_success() {
        insert_finding(
            state,
            scan,
            host,
            "ip",
            "medium",
            None,
            "The server IP answers for this app. Visitors can bypass the domain name.",
        )
        .await;
    }
}

async fn insert_finding(
    state: &AppState,
    scan: Uuid,
    host: &str,
    kind: &str,
    severity: &str,
    path: Option<&str>,
    summary: &str,
) {
    let _ = sqlx::query!(
        r#"
        INSERT INTO security_findings (scan_id, host, kind, severity, path, summary)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        scan,
        host,
        kind,
        severity,
        path,
        summary,
    )
    .execute(&state.pool)
    .await;
}

async fn note_expiring(state: &AppState) -> Result<(), ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT id, name, not_after
        FROM certificates
        WHERE not_after IS NOT NULL AND not_after < now() + interval '14 days'
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    for row in rows {
        let kind = if row.not_after.is_some_and(|when| when < Utc::now()) {
            "expired"
        } else {
            "expiring"
        };
        let already = sqlx::query_scalar!(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM ssl_events
                WHERE certificate_id = $1 AND kind = $2 AND created_at > now() - interval '1 day'
            ) AS "exists!"
            "#,
            row.id,
            kind
        )
        .fetch_one(&state.pool)
        .await
        .map_err(ApiError::database)?;
        if !already {
            record_ssl(
                &state.pool,
                None,
                Some(row.id),
                kind,
                &format!("{name} is {kind}", name = row.name),
            )
            .await;
        }
    }
    Ok(())
}

async fn scan_rows(state: &AppState) -> Result<Vec<SecurityScanView>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT id, status, started_at, finished_at, error
        FROM security_scans
        ORDER BY started_at DESC
        LIMIT 20
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(rows
        .into_iter()
        .map(|row| SecurityScanView {
            id: row.id,
            status: row.status,
            started_at: row.started_at,
            finished_at: row.finished_at,
            error: row.error,
        })
        .collect())
}

async fn rule_rows(state: &AppState) -> Result<Vec<RateLimitView>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT id, name, enabled, host, path_prefix, methods, limit_count, window_secs, burst, key_kind, header_name
        FROM rate_limit_rules
        ORDER BY name
        "#
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(rows
        .into_iter()
        .map(|row| RateLimitView {
            id: row.id,
            name: row.name,
            enabled: row.enabled,
            host: row.host,
            path_prefix: row.path_prefix,
            methods: row.methods,
            limit_count: row.limit_count,
            window_secs: row.window_secs,
            burst: row.burst,
            key_kind: row.key_kind,
            header_name: row.header_name,
        })
        .collect())
}

async fn mode_count(state: &AppState, mode: &str) -> Result<i64, ApiError> {
    sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM domains WHERE ssl_mode = $1"#,
        mode
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)
}

async fn edge_row(state: &AppState) -> Result<(i64, i64), ApiError> {
    let row =
        sqlx::query!(r#"SELECT http_requests, ip_requests FROM edge_counters WHERE id = TRUE"#)
            .fetch_one(&state.pool)
            .await
            .map_err(ApiError::database)?;
    Ok((row.http_requests, row.ip_requests))
}

async fn publish_all(state: &AppState) -> Result<(), ApiError> {
    crate::publish::publish_local(state).await
}
