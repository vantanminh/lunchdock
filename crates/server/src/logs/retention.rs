//! Log retention: per-source row deletes plus whole-partition drops.

use std::collections::BTreeMap;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use utoipa::ToSchema;

pub const MAX_LOG_DAYS: u32 = 365;
const DEFAULT_LOG_DAYS: u32 = 14;

pub const MAX_RAW_METRIC_HOURS: u32 = 168;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, ToSchema)]
pub struct MetricsRetention {
    /// Hours to keep 10-second samples, 1 to 168.
    pub raw_hours: u32,
    /// Days to keep each rollup resolution, 1 to 365.
    pub m1_days: u32,
    pub m5_days: u32,
    pub h1_days: u32,
}

impl Default for MetricsRetention {
    fn default() -> Self {
        Self {
            raw_hours: 24,
            m1_days: 7,
            m5_days: 14,
            h1_days: 30,
        }
    }
}

impl MetricsRetention {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=MAX_RAW_METRIC_HOURS).contains(&self.raw_hours) {
            return Err(format!(
                "raw samples are kept 1 to {MAX_RAW_METRIC_HOURS} hours"
            ));
        }
        for days in [self.m1_days, self.m5_days, self.h1_days] {
            if !(1..=MAX_LOG_DAYS).contains(&days) {
                return Err(format!("rollups are kept 1 to {MAX_LOG_DAYS} days"));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RetentionSettings {
    /// Days to keep per log source.
    pub log_days: BTreeMap<String, u32>,
    pub metrics: MetricsRetention,
}

pub async fn load(pool: &PgPool) -> Result<RetentionSettings, sqlx::Error> {
    let row = sqlx::query!("SELECT log_days, metrics FROM retention_settings WHERE id")
        .fetch_one(pool)
        .await?;
    let mut log_days: BTreeMap<String, u32> =
        serde_json::from_value(row.log_days).unwrap_or_default();
    for source in rudder_core::LOG_SOURCES {
        log_days
            .entry(source.to_string())
            .or_insert(DEFAULT_LOG_DAYS);
    }
    Ok(RetentionSettings {
        log_days,
        metrics: serde_json::from_value(row.metrics).unwrap_or_default(),
    })
}

#[derive(Debug, Default, Serialize)]
pub struct RetentionReport {
    pub deleted_rows: u64,
    pub dropped_partitions: Vec<String>,
}

/// Create upcoming partitions, delete expired rows, and drop fully expired partitions.
pub async fn run_once(pool: &PgPool) -> Result<RetentionReport, sqlx::Error> {
    for offset in 0_i32..=2 {
        sqlx::query!(
            "SELECT rudder_ensure_daily_partition('log_entries', (CURRENT_DATE + $1::int))",
            offset
        )
        .execute(pool)
        .await?;
    }
    let settings = load(pool).await?;
    let mut report = RetentionReport::default();
    for (source, days) in &settings.log_days {
        let result = sqlx::query!(
            "DELETE FROM log_entries WHERE source = $1 AND logged_at < now() - make_interval(days => $2)",
            source,
            i32::try_from(*days).unwrap_or(i32::MAX)
        )
        .execute(pool)
        .await?;
        report.deleted_rows += result.rows_affected();
    }
    let keep = settings
        .log_days
        .values()
        .copied()
        .max()
        .unwrap_or(DEFAULT_LOG_DAYS);
    let names: Vec<String> = sqlx::query_scalar!(
        r#"
        SELECT child.relname AS "name!"
        FROM pg_inherits
        JOIN pg_class parent ON parent.oid = pg_inherits.inhparent
        JOIN pg_class child ON child.oid = pg_inherits.inhrelid
        WHERE parent.relname = 'log_entries'
        "#
    )
    .fetch_all(pool)
    .await?;
    let today = Utc::now().date_naive();
    for name in rudder_core::partitions_to_drop(
        rudder_core::LOG_PARTITION_PREFIX,
        &names,
        today,
        keep.saturating_add(1),
    ) {
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        sqlx::query(&format!("DROP TABLE IF EXISTS {quoted}"))
            .execute(pool)
            .await?;
        report.dropped_partitions.push(name);
    }
    Ok(report)
}
