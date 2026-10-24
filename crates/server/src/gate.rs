//! In-memory rate limits and counters for the edge gate.
//!
//! Traefik asks the panel on each gated request. The decision stays in memory so
//! it does not wait on a database write. A background task copies the counts
//! into Postgres for the security page.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rudder_core::{decide_rate, Bucket, RateKey, RateRequest, RateRule};
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::error::ApiError;
use crate::state::AppState;

pub struct EdgeMemory {
    rules: Mutex<Vec<RateRule>>,
    buckets: Mutex<HashMap<String, Bucket>>,
    allowed: Mutex<HashMap<Uuid, u64>>,
    blocked: Mutex<HashMap<Uuid, u64>>,
    loaded: Mutex<Option<Instant>>,
    pub http_requests: AtomicU64,
    pub ip_requests: AtomicU64,
}

impl Default for EdgeMemory {
    fn default() -> Self {
        Self {
            rules: Mutex::new(Vec::new()),
            buckets: Mutex::new(HashMap::new()),
            allowed: Mutex::new(HashMap::new()),
            blocked: Mutex::new(HashMap::new()),
            loaded: Mutex::new(None),
            http_requests: AtomicU64::new(0),
            ip_requests: AtomicU64::new(0),
        }
    }
}

impl EdgeMemory {
    pub async fn decide(
        &self,
        state: &AppState,
        host: &str,
        path: &str,
        method: &str,
        ip: &str,
        headers: &HashMap<String, String>,
    ) -> Result<rudder_core::RateDecision, ApiError> {
        self.refresh(state).await?;
        let rules = self.rules.lock().await.clone();
        let mut buckets = self.buckets.lock().await;
        let now = unix_ms();
        let decision = decide_rate(
            &rules,
            &RateRequest {
                host,
                path,
                method,
                ip,
                headers,
                now_ms: now,
            },
            &mut buckets,
        );
        drop(buckets);
        if let Some(id) = decision
            .blocked_by
            .as_deref()
            .and_then(|id| id.parse::<Uuid>().ok())
        {
            *self.blocked.lock().await.entry(id).or_default() += 1;
        }
        for id in &decision.allowed_by {
            if let Ok(id) = id.parse::<Uuid>() {
                *self.allowed.lock().await.entry(id).or_default() += 1;
            }
        }
        Ok(decision)
    }

    async fn refresh(&self, state: &AppState) -> Result<(), ApiError> {
        let mut loaded = self.loaded.lock().await;
        if loaded.is_some_and(|at| at.elapsed() < Duration::from_secs(5)) {
            return Ok(());
        }
        let rows = sqlx::query!(
            r#"
            SELECT id, host, path_prefix, methods, limit_count, window_secs, burst, key_kind, header_name
            FROM rate_limit_rules
            WHERE enabled
            ORDER BY created_at
            "#
        )
        .fetch_all(&state.pool)
        .await
        .map_err(ApiError::database)?;
        let mut rules = Vec::new();
        for row in rows {
            rules.push(RateRule {
                id: row.id.to_string(),
                host: row.host.unwrap_or_default(),
                path_prefix: row.path_prefix,
                methods: row.methods,
                limit: u32::try_from(row.limit_count).unwrap_or(1),
                window_secs: u32::try_from(row.window_secs).unwrap_or(60),
                burst: u32::try_from(row.burst).unwrap_or(0),
                key: if row.key_kind == "header" {
                    RateKey::Header
                } else {
                    RateKey::Ip
                },
                header_name: row.header_name.unwrap_or_default(),
            });
        }
        *self.rules.lock().await = rules;
        *loaded = Some(Instant::now());
        Ok(())
    }

    pub async fn flush(&self, state: &AppState) -> Result<(), ApiError> {
        let allowed = std::mem::take(&mut *self.allowed.lock().await);
        let blocked = std::mem::take(&mut *self.blocked.lock().await);
        let now = chrono::Utc::now();
        let bucket =
            chrono::DateTime::from_timestamp(now.timestamp() - now.timestamp().rem_euclid(60), 0)
                .unwrap_or(now);
        let mut ids = allowed.keys().copied().collect::<Vec<_>>();
        ids.extend(blocked.keys().copied());
        ids.sort();
        ids.dedup();
        for id in ids {
            let allowed = i64::try_from(*allowed.get(&id).unwrap_or(&0)).unwrap_or(i64::MAX);
            let blocked = i64::try_from(*blocked.get(&id).unwrap_or(&0)).unwrap_or(i64::MAX);
            if allowed == 0 && blocked == 0 {
                continue;
            }
            sqlx::query!(
                r#"
                INSERT INTO rate_limit_samples (rule_id, bucket, allowed, blocked)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (rule_id, bucket)
                DO UPDATE SET allowed = rate_limit_samples.allowed + EXCLUDED.allowed,
                              blocked = rate_limit_samples.blocked + EXCLUDED.blocked
                "#,
                id,
                bucket,
                allowed,
                blocked,
            )
            .execute(&state.pool)
            .await
            .map_err(ApiError::database)?;
        }
        let http = self.http_requests.swap(0, Ordering::Relaxed);
        let ip = self.ip_requests.swap(0, Ordering::Relaxed);
        if http > 0 || ip > 0 {
            sqlx::query!(
                r#"
                UPDATE edge_counters
                SET http_requests = http_requests + $1,
                    ip_requests = ip_requests + $2,
                    updated_at = now()
                WHERE id = TRUE
                "#,
                i64::try_from(http).unwrap_or(i64::MAX),
                i64::try_from(ip).unwrap_or(i64::MAX),
            )
            .execute(&state.pool)
            .await
            .map_err(ApiError::database)?;
        }
        sqlx::query!(
            r#"DELETE FROM rate_limit_blocks WHERE created_at < now() - interval '24 hours'"#
        )
        .execute(&state.pool)
        .await
        .map_err(ApiError::database)?;
        Ok(())
    }
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn memory() -> Arc<EdgeMemory> {
    Arc::new(EdgeMemory::default())
}
