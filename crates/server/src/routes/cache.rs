//! Cache policies, presets, per-domain cache settings, purge, and stats.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::Json;
use rudder_core::{
    cache_preset, CacheHostStats, CachePolicy, CachePurge, CachePurgeResult, CacheRule, CacheStats,
    CACHE_PRESETS,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::audit;
use crate::auth::AuthUser;
use crate::cache::{load_policies, load_rules, servers_using, PolicyRecord};
use crate::error::ApiError;
use crate::nodes::driver_for;
use crate::state::AppState;

use super::catalog::{bad, conflict, core_error, is_unique, not_found};

const MIN_MEMORY: i64 = 1024 * 1024;
const MAX_MEMORY: i64 = 64 * 1024 * 1024 * 1024;
const MIN_OBJECT: i64 = 1024;
const MAX_OBJECT: i64 = 1024 * 1024 * 1024;

#[derive(Serialize, ToSchema)]
pub struct CachePolicyView {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    /// Share of the server's memory store. Null uses the default (256 MiB in total).
    pub memory_max_bytes: Option<i64>,
    /// Share of the server's disk store. Null or all-null policies keep objects in memory only.
    pub disk_max_bytes: Option<i64>,
    /// Largest response body stored. Null means 16 MiB.
    pub max_object_bytes: Option<i64>,
    pub rules: Vec<CacheRule>,
    /// Domains that use this policy.
    pub domains: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Deserialize, ToSchema)]
pub struct SaveCachePolicy {
    pub name: String,
    pub description: Option<String>,
    pub memory_max_bytes: Option<i64>,
    pub disk_max_bytes: Option<i64>,
    pub max_object_bytes: Option<i64>,
    /// Ordered rules; the first match wins. Defaults to the preset's rules on create.
    pub rules: Option<Vec<CacheRule>>,
    /// Preset slug to start from when creating.
    pub preset: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct CachePresetView {
    pub slug: String,
    pub name: String,
    pub description: String,
    pub rules: Vec<CacheRule>,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateDomainCache {
    pub enabled: bool,
    /// Null uses the built-in "Honor origin headers" policy.
    pub policy_id: Option<Uuid>,
}

#[derive(Serialize, ToSchema)]
pub struct DomainCacheView {
    pub domain_id: Uuid,
    pub host: String,
    pub path_prefix: String,
    pub enabled: bool,
    pub policy_id: Option<Uuid>,
    pub policy_name: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ServiceCacheView {
    pub domains: Vec<DomainCacheView>,
    /// Counters for this service's cached hosts.
    pub stats: Vec<CacheHostStats>,
    /// Why stats are missing, such as an unreachable cache server.
    pub stats_error: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct ServiceCachePurge {
    /// Limit the purge to one domain. Defaults to every cached domain of the service.
    pub domain_id: Option<Uuid>,
    pub paths: Option<Vec<String>>,
    pub prefix: Option<String>,
    pub all: Option<bool>,
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route(
            "/api/v1/cache-policies",
            get(list_policies).post(create_policy),
        )
        .route(
            "/api/v1/cache-policies/{id}",
            get(get_policy).put(update_policy).delete(delete_policy),
        )
        .route("/api/v1/cache-presets", get(list_presets))
        .route("/api/v1/domains/{id}/cache", put(update_domain_cache))
        .route("/api/v1/services/{id}/cache", get(service_cache))
        .route("/api/v1/services/{id}/cache/purge", post(purge_service))
        .route("/api/v1/servers/{id}/cache/stats", get(server_stats))
        .route(
            "/api/v1/cache/settings",
            get(cache_settings).put(update_cache_settings),
        )
        .route("/api/v1/cache/history", get(cache_history))
}

fn validate_policy(body: &SaveCachePolicy, rules: &[CacheRule]) -> Result<(), ApiError> {
    let name = body.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(bad("invalid", "policy name must be 1-100 characters"));
    }
    if body.description.as_deref().is_some_and(|d| d.len() > 500) {
        return Err(bad("invalid", "description is at most 500 characters"));
    }
    if body
        .memory_max_bytes
        .is_some_and(|v| !(MIN_MEMORY..=MAX_MEMORY).contains(&v))
    {
        return Err(bad(
            "invalid",
            "memory budget must be between 1 MiB and 64 GiB",
        ));
    }
    if body
        .disk_max_bytes
        .is_some_and(|v| !(MIN_MEMORY..=MAX_MEMORY).contains(&v))
    {
        return Err(bad(
            "invalid",
            "disk budget must be between 1 MiB and 64 GiB",
        ));
    }
    if body
        .max_object_bytes
        .is_some_and(|v| !(MIN_OBJECT..=MAX_OBJECT).contains(&v))
    {
        return Err(bad(
            "invalid",
            "object size limit must be between 1 KiB and 1 GiB",
        ));
    }
    CachePolicy {
        id: "check".into(),
        name: name.into(),
        max_object_bytes: None,
        rules: rules.to_vec(),
    }
    .validate()
    .map_err(core_error)
}

async fn policy_views(
    state: &AppState,
    records: Vec<PolicyRecord>,
) -> Result<Vec<CachePolicyView>, ApiError> {
    let ids: Vec<Uuid> = records.iter().map(|record| record.id).collect();
    let mut rules = load_rules(&state.pool, &ids).await?;
    let counts = sqlx::query!(
        r#"
        SELECT cache_policy_id AS "id!", COUNT(*) AS "count!"
        FROM domains
        WHERE cache_policy_id = ANY($1)
        GROUP BY cache_policy_id
        "#,
        &ids
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    Ok(records
        .into_iter()
        .map(|record| CachePolicyView {
            rules: rules.remove(&record.id).unwrap_or_default(),
            domains: counts
                .iter()
                .find(|row| row.id == record.id)
                .map_or(0, |row| row.count),
            id: record.id,
            name: record.name,
            description: record.description,
            memory_max_bytes: record.memory_max_bytes,
            disk_max_bytes: record.disk_max_bytes,
            max_object_bytes: record.max_object_bytes,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
        .collect())
}

async fn policy_view(state: &AppState, id: Uuid) -> Result<CachePolicyView, ApiError> {
    let records = load_policies(&state.pool, &[id]).await?;
    if records.is_empty() {
        return Err(not_found("cache policy not found"));
    }
    policy_views(state, records)
        .await?
        .pop()
        .ok_or_else(|| not_found("cache policy not found"))
}

async fn list_policies(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<Vec<CachePolicyView>>, ApiError> {
    let ids = sqlx::query_scalar!(r#"SELECT id FROM cache_policies"#)
        .fetch_all(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let records = load_policies(&state.pool, &ids).await?;
    Ok(Json(policy_views(&state, records).await?))
}

async fn get_policy(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<Json<CachePolicyView>, ApiError> {
    Ok(Json(policy_view(&state, id).await?))
}

async fn replace_rules(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    policy_id: Uuid,
    rules: &[CacheRule],
) -> Result<(), ApiError> {
    sqlx::query!(r#"DELETE FROM cache_rules WHERE policy_id = $1"#, policy_id)
        .execute(&mut **tx)
        .await
        .map_err(ApiError::database)?;
    for (position, rule) in rules.iter().enumerate() {
        sqlx::query!(
            r#"
            INSERT INTO cache_rules (policy_id, position, name, match_spec, action_spec)
            VALUES ($1, $2, $3, $4, $5)
            "#,
            policy_id,
            i32::try_from(position).unwrap_or(i32::MAX),
            rule.name,
            serde_json::to_value(&rule.matcher).unwrap_or_default(),
            serde_json::to_value(&rule.action).unwrap_or_default(),
        )
        .execute(&mut **tx)
        .await
        .map_err(ApiError::database)?;
    }
    Ok(())
}

async fn create_policy(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<SaveCachePolicy>,
) -> Result<(StatusCode, Json<CachePolicyView>), ApiError> {
    auth.require_write()?;
    let rules = match (&body.rules, body.preset.as_deref()) {
        (Some(rules), _) => rules.clone(),
        (None, Some(slug)) => (cache_preset(slug)
            .ok_or_else(|| bad("invalid", format!("unknown preset {slug}")))?
            .rules)(),
        (None, None) => Vec::new(),
    };
    validate_policy(&body, &rules)?;
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO cache_policies (name, description, memory_max_bytes, disk_max_bytes, max_object_bytes)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING id
        "#,
        body.name.trim(),
        body.description.as_deref().unwrap_or(""),
        body.memory_max_bytes,
        body.disk_max_bytes,
        body.max_object_bytes,
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| {
        if is_unique(&error) {
            conflict("name_taken", "a cache policy with this name exists")
        } else {
            ApiError::database(error)
        }
    })?;
    replace_rules(&mut tx, id, &rules).await?;
    tx.commit().await.map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "cache_policy.create",
        "cache_policy",
        Some(&id.to_string()),
        json!({ "name": body.name.trim(), "preset": body.preset, "rules": rules.len() }),
        None,
    )
    .await;
    Ok((StatusCode::CREATED, Json(policy_view(&state, id).await?)))
}

async fn republish(state: &AppState, servers: Vec<Uuid>) {
    for server_id in servers {
        if let Err(error) = crate::publish::publish_server(state, server_id).await {
            tracing::warn!(%server_id, message = %error.public_message(), "routes were not republished");
        }
    }
}

async fn update_policy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveCachePolicy>,
) -> Result<Json<CachePolicyView>, ApiError> {
    auth.require_write()?;
    let current = policy_view(&state, id).await?;
    let rules = body.rules.clone().unwrap_or(current.rules);
    validate_policy(&body, &rules)?;
    let mut tx = state.pool.begin().await.map_err(ApiError::database)?;
    sqlx::query!(
        r#"
        UPDATE cache_policies
        SET name = $2, description = $3, memory_max_bytes = $4, disk_max_bytes = $5,
            max_object_bytes = $6, updated_at = now()
        WHERE id = $1
        "#,
        id,
        body.name.trim(),
        body.description.as_deref().unwrap_or(&current.description),
        body.memory_max_bytes,
        body.disk_max_bytes,
        body.max_object_bytes,
    )
    .execute(&mut *tx)
    .await
    .map_err(|error| {
        if is_unique(&error) {
            conflict("name_taken", "a cache policy with this name exists")
        } else {
            ApiError::database(error)
        }
    })?;
    replace_rules(&mut tx, id, &rules).await?;
    tx.commit().await.map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "cache_policy.update",
        "cache_policy",
        Some(&id.to_string()),
        json!({ "name": body.name.trim(), "rules": rules.len() }),
        None,
    )
    .await;
    republish(&state, servers_using(&state.pool, id).await?).await;
    Ok(Json(policy_view(&state, id).await?))
}

async fn delete_policy(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    auth.require_write()?;
    let servers = servers_using(&state.pool, id).await?;
    let deleted = sqlx::query!(r#"DELETE FROM cache_policies WHERE id = $1"#, id)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    if deleted.rows_affected() == 0 {
        return Err(not_found("cache policy not found"));
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "cache_policy.delete",
        "cache_policy",
        Some(&id.to_string()),
        json!({}),
        None,
    )
    .await;
    republish(&state, servers).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn list_presets(_auth: AuthUser) -> Json<Vec<CachePresetView>> {
    Json(
        CACHE_PRESETS
            .iter()
            .map(|preset| CachePresetView {
                slug: preset.slug.into(),
                name: preset.name.into(),
                description: preset.description.into(),
                rules: (preset.rules)(),
            })
            .collect(),
    )
}

struct DomainCacheRow {
    domain_id: Uuid,
    host: String,
    path_prefix: String,
    enabled: bool,
    policy_id: Option<Uuid>,
    policy_name: Option<String>,
    server_id: Uuid,
}

async fn domain_cache_rows(
    state: &AppState,
    service_id: Option<Uuid>,
    domain_id: Option<Uuid>,
) -> Result<Vec<DomainCacheRow>, ApiError> {
    sqlx::query_as!(
        DomainCacheRow,
        r#"
        SELECT d.id AS domain_id, d.host, d.path_prefix, d.cache_enabled AS enabled,
               d.cache_policy_id AS policy_id, p.name AS "policy_name?", s.server_id
        FROM domains d
        JOIN services s ON s.id = d.service_id
        LEFT JOIN cache_policies p ON p.id = d.cache_policy_id
        WHERE ($1::uuid IS NULL OR d.service_id = $1)
          AND ($2::uuid IS NULL OR d.id = $2)
        ORDER BY d.host, d.path_prefix
        "#,
        service_id,
        domain_id
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)
}

fn domain_cache_view(row: DomainCacheRow) -> DomainCacheView {
    DomainCacheView {
        domain_id: row.domain_id,
        host: row.host,
        path_prefix: row.path_prefix,
        enabled: row.enabled,
        policy_id: row.policy_id,
        policy_name: row.policy_name,
    }
}

async fn update_domain_cache(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateDomainCache>,
) -> Result<Json<DomainCacheView>, ApiError> {
    auth.require_write()?;
    let current = domain_cache_rows(&state, None, Some(id))
        .await?
        .pop()
        .ok_or_else(|| not_found("domain not found"))?;
    if let Some(policy_id) = body.policy_id {
        if load_policies(&state.pool, &[policy_id]).await?.is_empty() {
            return Err(bad("invalid", "cache policy not found"));
        }
    }
    sqlx::query!(
        r#"UPDATE domains SET cache_enabled = $2, cache_policy_id = $3 WHERE id = $1"#,
        id,
        body.enabled,
        body.policy_id
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    audit::record(
        &state.pool,
        Some(auth.id),
        "domain.cache",
        "domain",
        Some(&id.to_string()),
        json!({ "host": current.host, "enabled": body.enabled, "policy_id": body.policy_id }),
        None,
    )
    .await;
    crate::publish::publish_server(&state, current.server_id).await?;
    let row = domain_cache_rows(&state, None, Some(id))
        .await?
        .pop()
        .ok_or_else(|| not_found("domain not found"))?;
    Ok(Json(domain_cache_view(row)))
}

async fn service_cache(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(service_id): Path<Uuid>,
) -> Result<Json<ServiceCacheView>, ApiError> {
    let server_id = sqlx::query_scalar!(
        r#"SELECT server_id FROM services WHERE id = $1"#,
        service_id
    )
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::database)?
    .ok_or_else(|| not_found("service not found"))?;
    let rows = domain_cache_rows(&state, Some(service_id), None).await?;
    let hosts: Vec<String> = rows
        .iter()
        .filter(|row| row.enabled)
        .map(|row| row.host.clone())
        .collect();
    let (stats, stats_error) = if hosts.is_empty() {
        (Vec::new(), None)
    } else {
        match fetch_stats(&state, server_id).await {
            Ok(all) => (
                all.hosts
                    .into_iter()
                    .filter(|host| hosts.contains(&host.host))
                    .collect(),
                None,
            ),
            Err(error) => (Vec::new(), Some(error.public_message().to_owned())),
        }
    };
    Ok(Json(ServiceCacheView {
        domains: rows.into_iter().map(domain_cache_view).collect(),
        stats,
        stats_error,
    }))
}

async fn fetch_stats(state: &AppState, server_id: Uuid) -> Result<CacheStats, ApiError> {
    driver_for(state, server_id)
        .await?
        .cache_stats()
        .await
        .map_err(core_error)
}

async fn purge_service(
    State(state): State<AppState>,
    auth: AuthUser,
    Path(service_id): Path<Uuid>,
    Json(body): Json<ServiceCachePurge>,
) -> Result<Json<CachePurgeResult>, ApiError> {
    auth.require_write()?;
    let rows: Vec<DomainCacheRow> = domain_cache_rows(&state, Some(service_id), body.domain_id)
        .await?
        .into_iter()
        .filter(|row| row.enabled)
        .collect();
    let Some(server_id) = rows.first().map(|row| row.server_id) else {
        return Err(bad("invalid", "no cached domain matches"));
    };
    let all = body.all.unwrap_or(false);
    let paths = body.paths.clone().unwrap_or_default();
    let driver = driver_for(&state, server_id).await?;
    let mut removed = 0;
    let mut purged_hosts: Vec<(String, Option<String>)> = Vec::new();
    for row in &rows {
        let scope = (all && row.path_prefix != "/").then(|| row.path_prefix.clone());
        let entry = (row.host.clone(), scope);
        if purged_hosts.contains(&entry) {
            continue;
        }
        let request = CachePurge {
            host: row.host.clone(),
            paths: paths.clone(),
            prefix: body.prefix.clone().or(entry.1.clone()),
            all: all && entry.1.is_none(),
        };
        request.validate().map_err(core_error)?;
        removed += driver
            .cache_purge(&request)
            .await
            .map_err(core_error)?
            .removed;
        purged_hosts.push(entry);
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "cache.purge",
        "service",
        Some(&service_id.to_string()),
        json!({
            "hosts": purged_hosts.iter().map(|(host, _)| host).collect::<Vec<_>>(),
            "paths": paths.len(),
            "prefix": body.prefix,
            "all": all,
            "removed": removed,
        }),
        None,
    )
    .await;
    Ok(Json(CachePurgeResult { removed }))
}

#[derive(Serialize, ToSchema)]
pub struct CacheSettingsView {
    pub memory_percent: i16,
    pub memory_cap_bytes: Option<i64>,
    pub host_memory_bytes: Option<i64>,
    /// The most bytes a rules file will ask the cache to store right now.
    pub effective_cap_bytes: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateCacheSettings {
    pub memory_percent: i16,
    /// Blank clears the extra ceiling. 16 MiB to 4 GiB.
    pub memory_cap_bytes: Option<i64>,
}

#[derive(Deserialize)]
struct HistoryQuery {
    server_id: Option<Uuid>,
    hours: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct CachePoint {
    pub sampled_at: chrono::DateTime<chrono::Utc>,
    /// Hit ratio since the previous sample, from 0 to 1. Null when nothing was served.
    pub hit_ratio: Option<f64>,
    pub hits: i64,
    pub misses: i64,
    pub stored_bytes: i64,
    pub memory_max_bytes: i64,
    pub evictions: i64,
    pub bytes_served: i64,
}

#[derive(Serialize, ToSchema)]
pub struct CacheServerHistory {
    pub server_id: Uuid,
    pub name: String,
    pub points: Vec<CachePoint>,
}

async fn cache_settings(
    State(state): State<AppState>,
    _auth: AuthUser,
) -> Result<Json<CacheSettingsView>, ApiError> {
    Ok(Json(settings_view(&state).await?))
}

async fn update_cache_settings(
    State(state): State<AppState>,
    auth: AuthUser,
    Json(body): Json<UpdateCacheSettings>,
) -> Result<Json<CacheSettingsView>, ApiError> {
    auth.require_write()?;
    if !(1..=40).contains(&body.memory_percent) {
        return Err(bad("invalid", "memory share must be from 1 to 40 percent"));
    }
    if body
        .memory_cap_bytes
        .is_some_and(|value| !(16 * 1024 * 1024..=4 * 1024 * 1024 * 1024).contains(&value))
    {
        return Err(bad(
            "invalid",
            "memory cap must be between 16 MiB and 4 GiB",
        ));
    }
    sqlx::query!(
        r#"
        UPDATE panel_settings
        SET cache_memory_percent = $1, cache_memory_cap_bytes = $2
        WHERE id = TRUE
        "#,
        body.memory_percent,
        body.memory_cap_bytes,
    )
    .execute(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let servers = sqlx::query_scalar!(r#"SELECT id FROM servers"#)
        .fetch_all(&state.pool)
        .await
        .map_err(ApiError::database)?;
    for server_id in servers {
        if let Err(error) = crate::publish::publish_server(&state, server_id).await {
            tracing::warn!(
                %server_id,
                message = %error.public_message(),
                "republish after cache settings failed"
            );
        }
    }
    audit::record(
        &state.pool,
        Some(auth.id),
        "cache.settings",
        "panel_settings",
        None,
        json!({
            "memory_percent": body.memory_percent,
            "memory_cap_bytes": body.memory_cap_bytes,
        }),
        None,
    )
    .await;
    Ok(Json(settings_view(&state).await?))
}

async fn settings_view(state: &AppState) -> Result<CacheSettingsView, ApiError> {
    let row = sqlx::query!(
        r#"
        SELECT cache_memory_percent, cache_memory_cap_bytes
        FROM panel_settings
        WHERE id = TRUE
        "#
    )
    .fetch_one(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let host = std::fs::read_to_string(state.config.host_proc.join("meminfo"))
        .ok()
        .and_then(|text| rudder_core::parse_meminfo(&text))
        .map(|(total, _)| i64::try_from(total).unwrap_or(i64::MAX));
    let cap = crate::cache::memory_cap(&state.pool, &state.config.host_proc).await?;
    Ok(CacheSettingsView {
        memory_percent: row.cache_memory_percent,
        memory_cap_bytes: row.cache_memory_cap_bytes,
        host_memory_bytes: host,
        effective_cap_bytes: i64::try_from(cap).unwrap_or(i64::MAX),
    })
}

async fn cache_history(
    State(state): State<AppState>,
    _auth: AuthUser,
    Query(query): Query<HistoryQuery>,
) -> Result<Json<Vec<CacheServerHistory>>, ApiError> {
    let hours = query.hours.unwrap_or(24).clamp(1, 24 * 14);
    let rows = sqlx::query!(
        r#"
        SELECT s.id AS server_id, s.name,
               c.sampled_at, c.hits, c.misses, c.stored_bytes, c.memory_max_bytes,
               c.evictions, c.bytes_served
        FROM cache_stat_samples c
        JOIN servers s ON s.id = c.server_id
        WHERE ($1::uuid IS NULL OR c.server_id = $1)
          AND c.sampled_at >= now() - ($2::bigint * interval '1 hour')
        ORDER BY s.name, c.sampled_at
        "#,
        query.server_id,
        hours,
    )
    .fetch_all(&state.pool)
    .await
    .map_err(ApiError::database)?;
    let mut out: Vec<CacheServerHistory> = Vec::new();
    let mut previous: Option<(Uuid, i64, i64, i64, i64)> = None;
    for row in rows {
        if out
            .last()
            .is_none_or(|server| server.server_id != row.server_id)
        {
            out.push(CacheServerHistory {
                server_id: row.server_id,
                name: row.name.clone(),
                points: Vec::new(),
            });
            previous = None;
        }
        let prior = previous.filter(|(server, ..)| *server == row.server_id);
        let hits = delta(prior.map(|(_, hits, ..)| hits), row.hits);
        let misses = delta(prior.map(|(_, _, misses, ..)| misses), row.misses);
        let served = hits.saturating_add(misses);
        out.last_mut()
            .expect("server just pushed")
            .points
            .push(CachePoint {
                sampled_at: row.sampled_at,
                hit_ratio: (served > 0).then(|| hits as f64 / served as f64),
                hits,
                misses,
                stored_bytes: row.stored_bytes,
                memory_max_bytes: row.memory_max_bytes,
                evictions: delta(
                    prior.map(|(_, _, _, evictions, _)| evictions),
                    row.evictions,
                ),
                bytes_served: delta(prior.map(|(_, _, _, _, bytes)| bytes), row.bytes_served),
            });
        previous = Some((
            row.server_id,
            row.hits,
            row.misses,
            row.evictions,
            row.bytes_served,
        ));
    }
    Ok(Json(out))
}

fn delta(previous: Option<i64>, current: i64) -> i64 {
    match previous {
        Some(previous) if current >= previous => current - previous,
        Some(_) => current,
        None => 0,
    }
}

/// Record one sample per server whose cache answers. Old samples are dropped.
pub async fn sample_cache(state: &AppState) -> Result<usize, ApiError> {
    let servers = sqlx::query_scalar!(r#"SELECT id FROM servers"#)
        .fetch_all(&state.pool)
        .await
        .map_err(ApiError::database)?;
    let mut saved = 0;
    for server_id in servers {
        let Ok(stats) = fetch_stats(state, server_id).await else {
            continue;
        };
        let fold = |pick: fn(&rudder_core::CacheHostStats) -> u64| {
            i64::try_from(stats.hosts.iter().map(pick).fold(0u64, u64::saturating_add))
                .unwrap_or(i64::MAX)
        };
        sqlx::query!(
            r#"
            INSERT INTO cache_stat_samples (
                server_id, objects, stored_bytes, memory_max_bytes, evictions,
                hits, misses, stale, bypass, bytes_served
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
            server_id,
            i64::try_from(stats.objects).unwrap_or(i64::MAX),
            i64::try_from(stats.stored_bytes).unwrap_or(i64::MAX),
            i64::try_from(stats.memory_max_bytes).unwrap_or(i64::MAX),
            i64::try_from(stats.evictions).unwrap_or(i64::MAX),
            fold(|host| host.hits),
            fold(|host| host.misses),
            fold(|host| host.stale),
            fold(|host| host.bypass),
            fold(|host| host.bytes_served),
        )
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
        saved += 1;
    }
    sqlx::query!(r#"DELETE FROM cache_stat_samples WHERE sampled_at < now() - interval '14 days'"#)
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
    Ok(saved)
}

async fn server_stats(
    State(state): State<AppState>,
    _auth: AuthUser,
    Path(server_id): Path<Uuid>,
) -> Result<Json<CacheStats>, ApiError> {
    Ok(Json(fetch_stats(&state, server_id).await?))
}
