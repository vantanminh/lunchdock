//! Raw samples to 1-minute, 5-minute, and 1-hour rollups, plus metrics retention.
//!
//! Each resolution has a watermark in `metrics_watermarks`; a step rolls up
//! `[watermark, end)` and advances the watermark in the same transaction, so
//! repeated or concurrent runs never double count.

use chrono::{DateTime, Duration, DurationRound, Utc};
use sqlx::PgPool;

/// Raw samples may arrive this late (agent buffering, slow ticks).
const RAW_SLACK_SECS: i64 = 20;
/// First rollup after install starts this far back.
const INITIAL_LOOKBACK_HOURS: i64 = 24;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RollupReport {
    pub m1: u64,
    pub m5: u64,
    pub h1: u64,
}

async fn watermark(
    tx: &mut sqlx::PgConnection,
    resolution: &str,
) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    sqlx::query_scalar!(
        "SELECT done_until FROM metrics_watermarks WHERE resolution = $1 FOR UPDATE",
        resolution
    )
    .fetch_optional(tx)
    .await
}

async fn set_watermark(
    tx: &mut sqlx::PgConnection,
    resolution: &str,
    until: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO metrics_watermarks (resolution, done_until) VALUES ($1, $2)
        ON CONFLICT (resolution) DO UPDATE SET done_until = EXCLUDED.done_until
        "#,
        resolution,
        until
    )
    .execute(tx)
    .await?;
    Ok(())
}

fn floor(at: DateTime<Utc>, step: Duration) -> DateTime<Utc> {
    at.duration_trunc(step).unwrap_or(at)
}

/// Roll up everything that is complete as of `now`. Proxy rows (`requests`,
/// `http_5xx`) are counts per minute at every resolution; minutes without
/// traffic have no 1m row and count as zero in coarser buckets.
pub async fn rollup_once(pool: &PgPool, now: DateTime<Utc>) -> Result<RollupReport, sqlx::Error> {
    let mut report = RollupReport::default();
    let mut tx = pool.begin().await?;
    sqlx::query!("SELECT pg_advisory_xact_lock(726_431)")
        .execute(&mut *tx)
        .await?;

    let start = |mark: Option<DateTime<Utc>>, step: Duration| {
        mark.unwrap_or_else(|| floor(now - Duration::hours(INITIAL_LOOKBACK_HOURS), step))
    };

    let m1_from = start(watermark(&mut tx, "1m").await?, Duration::minutes(1));
    let m1_to = floor(
        now - Duration::seconds(RAW_SLACK_SECS),
        Duration::minutes(1),
    );
    if m1_to > m1_from {
        let samples = sqlx::query!(
            r#"
            INSERT INTO metrics_rollups (
                bucket_start, resolution, server_id, service_id, domain_id, source, metric,
                avg_value, min_value, max_value, sample_count
            )
            SELECT date_trunc('minute', sampled_at), '1m', server_id, service_id, domain_id,
                   source, metric, avg(value), min(value), max(value), count(*)::int
            FROM metrics_samples
            WHERE sampled_at >= $1 AND sampled_at < $2
            GROUP BY 1, server_id, service_id, domain_id, source, metric
            "#,
            m1_from,
            m1_to
        )
        .execute(&mut *tx)
        .await?;
        let requests = sqlx::query!(
            r#"
            INSERT INTO metrics_rollups (
                bucket_start, resolution, server_id, service_id, source, metric,
                avg_value, min_value, max_value, sample_count
            )
            SELECT bucket, '1m', server_id, service_id, 'proxy', metric, n, n, n, 1
            FROM (
                SELECT date_trunc('minute', logged_at) AS bucket, server_id, service_id,
                       count(*)::float8 AS requests,
                       count(*) FILTER (WHERE http_status >= 500)::float8 AS errors
                FROM log_entries
                WHERE source = 'proxy' AND service_id IS NOT NULL
                  AND logged_at >= $1 AND logged_at < $2
                GROUP BY 1, server_id, service_id
            ) counted
            CROSS JOIN LATERAL (VALUES ('requests', requests), ('http_5xx', errors)) AS m(metric, n)
            "#,
            m1_from,
            m1_to
        )
        .execute(&mut *tx)
        .await?;
        report.m1 = samples.rows_affected() + requests.rows_affected();
        set_watermark(&mut tx, "1m", m1_to).await?;
    }
    let m1_done = watermark(&mut tx, "1m").await?;

    report.m5 = coarser(
        &mut tx,
        "1m",
        "5m",
        Duration::minutes(1),
        Duration::minutes(5),
        m1_done,
        start,
    )
    .await?;
    let m5_done = watermark(&mut tx, "5m").await?;
    report.h1 = coarser(
        &mut tx,
        "5m",
        "1h",
        Duration::minutes(5),
        Duration::hours(1),
        m5_done,
        start,
    )
    .await?;
    tx.commit().await?;
    Ok(report)
}

async fn coarser(
    tx: &mut sqlx::PgConnection,
    from_resolution: &str,
    to_resolution: &str,
    finer: Duration,
    step: Duration,
    finer_done: Option<DateTime<Utc>>,
    start: impl Fn(Option<DateTime<Utc>>, Duration) -> DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let Some(finer_done) = finer_done else {
        return Ok(0);
    };
    let from = start(watermark(tx, to_resolution).await?, step);
    let to = floor(finer_done, step);
    if to <= from {
        return Ok(0);
    }
    let seconds = step.num_seconds() as f64;
    let finer_seconds = finer.num_seconds() as f64;
    let inserted = sqlx::query!(
        r#"
        INSERT INTO metrics_rollups (
            bucket_start, resolution, server_id, service_id, domain_id, source, metric,
            avg_value, min_value, max_value, sample_count
        )
        SELECT date_bin(make_interval(secs => $5), bucket_start, TIMESTAMPTZ 'epoch'), $2,
               server_id, service_id, domain_id, source, metric,
               CASE WHEN source = 'proxy' THEN sum(avg_value) * $6 / $5
                    ELSE sum(avg_value * sample_count) / NULLIF(sum(sample_count), 0) END,
               min(min_value), max(max_value), sum(sample_count)::int
        FROM metrics_rollups
        WHERE resolution = $1 AND bucket_start >= $3 AND bucket_start < $4
        GROUP BY 1, server_id, service_id, domain_id, source, metric
        "#,
        from_resolution,
        to_resolution,
        from,
        to,
        seconds,
        finer_seconds,
    )
    .execute(&mut *tx)
    .await?;
    set_watermark(tx, to_resolution, to).await?;
    Ok(inserted.rows_affected())
}

#[derive(Debug, Default)]
pub struct MetricsRetentionReport {
    pub deleted_rows: u64,
    pub dropped_partitions: Vec<String>,
}

async fn partitions(pool: &PgPool, parent: &str) -> Result<Vec<String>, sqlx::Error> {
    sqlx::query_scalar!(
        r#"
        SELECT child.relname AS "name!"
        FROM pg_inherits
        JOIN pg_class parent ON parent.oid = pg_inherits.inhparent
        JOIN pg_class child ON child.oid = pg_inherits.inhrelid
        WHERE parent.relname = $1
        "#,
        parent
    )
    .fetch_all(pool)
    .await
}

async fn drop_partitions(
    pool: &PgPool,
    parent: &str,
    keep_days: u32,
    report: &mut MetricsRetentionReport,
) -> Result<(), sqlx::Error> {
    let prefix = format!("{parent}_");
    let names = partitions(pool, parent).await?;
    let today = Utc::now().date_naive();
    for name in rudder_core::partitions_to_drop(&prefix, &names, today, keep_days) {
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        sqlx::query(&format!("DROP TABLE IF EXISTS {quoted}"))
            .execute(pool)
            .await?;
        report.dropped_partitions.push(name);
    }
    Ok(())
}

/// Create upcoming metric partitions and remove data past its retention.
pub async fn retention_once(pool: &PgPool) -> Result<MetricsRetentionReport, sqlx::Error> {
    for parent in ["metrics_samples", "metrics_rollups"] {
        for offset in 0_i32..=2 {
            sqlx::query!(
                "SELECT rudder_ensure_daily_partition($1, (CURRENT_DATE + $2::int))",
                parent,
                offset
            )
            .execute(pool)
            .await?;
        }
    }
    let settings = crate::logs::retention::load(pool).await?.metrics;
    let mut report = MetricsRetentionReport::default();
    let raw = sqlx::query!(
        "DELETE FROM metrics_samples WHERE sampled_at < now() - make_interval(hours => $1)",
        settings.raw_hours as i32
    )
    .execute(pool)
    .await?;
    report.deleted_rows += raw.rows_affected();
    for (resolution, days) in [
        ("1m", settings.m1_days),
        ("5m", settings.m5_days),
        ("1h", settings.h1_days),
    ] {
        let deleted = sqlx::query!(
            "DELETE FROM metrics_rollups WHERE resolution = $1 AND bucket_start < now() - make_interval(days => $2)",
            resolution,
            days as i32
        )
        .execute(pool)
        .await?;
        report.deleted_rows += deleted.rows_affected();
    }
    let raw_days = settings.raw_hours.div_ceil(24) + 1;
    drop_partitions(pool, "metrics_samples", raw_days, &mut report).await?;
    let rollup_days = settings.m1_days.max(settings.m5_days).max(settings.h1_days) + 1;
    drop_partitions(pool, "metrics_rollups", rollup_days, &mut report).await?;
    Ok(report)
}
