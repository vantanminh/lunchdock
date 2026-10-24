//! Cache policies in PostgreSQL and the rules document the cache server reads.

use std::collections::HashMap;

use rudder_core::{
    cache_preset, CacheDocument, CachePolicy, CacheRule, CacheSite, NodeDriver, RuleAction,
    RuleMatch, CACHE_TOKEN_FILE, DEFAULT_MEMORY_BYTES,
};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::ApiError;
use crate::routes::catalog::core_error;

/// Policy id used for cache-enabled domains without a policy.
pub const DEFAULT_POLICY_ID: &str = "default";
const DEFAULT_PRESET: &str = "honor-origin";

/// A cache-enabled route before policies are resolved.
pub struct SiteSource {
    pub host: String,
    pub path_prefix: String,
    pub upstream: String,
    pub policy_id: Option<Uuid>,
}

pub struct PolicyRecord {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub memory_max_bytes: Option<i64>,
    pub disk_max_bytes: Option<i64>,
    pub max_object_bytes: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

struct RuleRow {
    policy_id: Uuid,
    name: String,
    match_spec: serde_json::Value,
    action_spec: serde_json::Value,
}

/// Rules for each policy, in position order. Rows that no longer parse are skipped.
pub async fn load_rules(
    pool: &PgPool,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<CacheRule>>, ApiError> {
    let rows = sqlx::query_as!(
        RuleRow,
        r#"
        SELECT policy_id, name, match_spec, action_spec
        FROM cache_rules
        WHERE policy_id = ANY($1)
        ORDER BY policy_id, position
        "#,
        ids
    )
    .fetch_all(pool)
    .await
    .map_err(ApiError::database)?;
    let mut out: HashMap<Uuid, Vec<CacheRule>> = HashMap::new();
    for row in rows {
        let parsed = serde_json::from_value::<RuleMatch>(row.match_spec).and_then(|matcher| {
            serde_json::from_value::<RuleAction>(row.action_spec).map(|action| CacheRule {
                name: row.name.clone(),
                matcher,
                action,
            })
        });
        match parsed {
            Ok(rule) => out.entry(row.policy_id).or_default().push(rule),
            Err(error) => {
                tracing::warn!(policy = %row.policy_id, %error, "stored cache rule skipped")
            }
        }
    }
    Ok(out)
}

pub async fn load_policies(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<PolicyRecord>, ApiError> {
    sqlx::query_as!(
        PolicyRecord,
        r#"
        SELECT id, name, description, memory_max_bytes, disk_max_bytes, max_object_bytes, created_at, updated_at
        FROM cache_policies
        WHERE id = ANY($1)
        ORDER BY name
        "#,
        ids
    )
    .fetch_all(pool)
    .await
    .map_err(ApiError::database)
}

fn default_policy() -> CachePolicy {
    let preset = cache_preset(DEFAULT_PRESET).expect("default cache preset exists");
    CachePolicy {
        id: DEFAULT_POLICY_ID.into(),
        name: preset.name.into(),
        max_object_bytes: None,
        rules: (preset.rules)(),
    }
}

/// Build the document for one server. The memory budget is the sum of the budgets set on the
/// policies in use, or the default when none sets one.
pub async fn render_document(
    pool: &PgPool,
    sources: Vec<SiteSource>,
) -> Result<CacheDocument, ApiError> {
    let mut ids: Vec<Uuid> = sources.iter().filter_map(|site| site.policy_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let records = load_policies(pool, &ids).await?;
    let mut rules = load_rules(pool, &ids).await?;
    let mut policies: Vec<CachePolicy> = Vec::new();
    let mut memory: Option<u64> = None;
    let mut disk: Option<u64> = None;
    for record in &records {
        if let Some(bytes) = record.memory_max_bytes.and_then(|v| u64::try_from(v).ok()) {
            memory = Some(memory.unwrap_or(0).saturating_add(bytes));
        }
        if let Some(bytes) = record.disk_max_bytes.and_then(|v| u64::try_from(v).ok()) {
            disk = Some(disk.unwrap_or(0).saturating_add(bytes));
        }
        policies.push(CachePolicy {
            id: record.id.to_string(),
            name: record.name.clone(),
            max_object_bytes: record.max_object_bytes.and_then(|v| u64::try_from(v).ok()),
            rules: rules.remove(&record.id).unwrap_or_default(),
        });
    }
    let mut sites = Vec::new();
    for source in sources {
        let policy = match source.policy_id {
            Some(id) if records.iter().any(|record| record.id == id) => id.to_string(),
            _ => DEFAULT_POLICY_ID.to_owned(),
        };
        if policy == DEFAULT_POLICY_ID && !policies.iter().any(|p| p.id == DEFAULT_POLICY_ID) {
            policies.push(default_policy());
        }
        sites.push(CacheSite {
            host: source.host,
            path_prefix: source.path_prefix,
            upstream: source.upstream,
            policy,
        });
    }
    let document = CacheDocument {
        version: u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or_default(),
        memory_max_bytes: Some(memory.unwrap_or(DEFAULT_MEMORY_BYTES)),
        disk_max_bytes: Some(disk.unwrap_or(0)),
        sites,
        policies,
    };
    document.validate().map_err(core_error)?;
    Ok(document)
}

/// Create the cache admin token when it does not exist yet, so the panel and the cache share it.
pub async fn ensure_token(driver: &dyn NodeDriver) -> Result<(), ApiError> {
    if driver
        .read_file(CACHE_TOKEN_FILE)
        .await
        .is_ok_and(|bytes| !bytes.iter().all(u8::is_ascii_whitespace))
    {
        return Ok(());
    }
    let token: String = rudder_core::random_bytes::<32>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    driver
        .write_file(CACHE_TOKEN_FILE, token.as_bytes())
        .await
        .map_err(core_error)
}

/// Servers whose routes use `policy_id`.
pub async fn servers_using(pool: &PgPool, policy_id: Uuid) -> Result<Vec<Uuid>, ApiError> {
    sqlx::query_scalar!(
        r#"
        SELECT DISTINCT s.server_id
        FROM domains d
        JOIN services s ON s.id = d.service_id
        WHERE d.cache_enabled AND d.cache_policy_id = $1
        "#,
        policy_id
    )
    .fetch_all(pool)
    .await
    .map_err(ApiError::database)
}
