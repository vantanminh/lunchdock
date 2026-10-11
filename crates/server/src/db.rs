//! PostgreSQL connection helpers.
//!
//! Row-shaped reads use `sqlx::query!` so column types are checked at compile
//! time. Partition maintenance stays on `sqlx::query` because the statement is
//! a function call rather than a stable row shape.

use anyhow::Context;
use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::Config;

pub async fn connect(config: &Config) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(20)
        .acquire_timeout(std::time::Duration::from_secs(10))
        .connect(&config.database_url)
        .await
        .context("connect to postgres")
}

pub struct PanelSettingsRow {
    pub github_repo: String,
    pub update_channel: String,
    pub auto_update: bool,
}

pub async fn load_panel_settings(pool: &PgPool) -> anyhow::Result<PanelSettingsRow> {
    let row = sqlx::query!(
        r#"
        SELECT github_repo, update_channel, auto_update
        FROM panel_settings
        WHERE id = TRUE
        "#
    )
    .fetch_one(pool)
    .await
    .context("load panel settings")?;
    Ok(PanelSettingsRow {
        github_repo: row.github_repo,
        update_channel: row.update_channel,
        auto_update: row.auto_update,
    })
}

pub async fn database_reachable(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query!("SELECT 1 AS ok").fetch_one(pool).await?;
    Ok(())
}

/// Create today's and tomorrow's partitions when they are not already present.
pub async fn ensure_partitions(pool: &PgPool) -> Result<(), sqlx::Error> {
    for parent in ["log_entries", "metrics_samples", "metrics_rollups"] {
        for offset in [0_i32, 1] {
            sqlx::query(
                "SELECT rudder_ensure_daily_partition($1, (CURRENT_DATE + $2 * INTERVAL '1 day')::date)",
            )
            .bind(parent)
            .bind(offset)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}

/// Deployments and services interrupted by a restart cannot still be in progress.
pub async fn recover_interrupted(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE deployments
        SET status = 'failed',
            error = 'interrupted by process restart',
            finished_at = COALESCE(finished_at, now())
        WHERE status = 'running'
        "#
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        r#"
        UPDATE services
        SET status = 'failed', updated_at = now()
        WHERE status IN ('deploying', 'building')
        "#
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        r#"
        UPDATE build_queue
        SET status = 'failed'
        WHERE status = 'running'
        "#
    )
    .execute(pool)
    .await?;
    sqlx::query!(
        r#"
        UPDATE backup_runs
        SET status = 'failed',
            error = 'interrupted by process restart',
            finished_at = COALESCE(finished_at, now())
        WHERE status IN ('queued', 'running')
        "#
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Insert the panel host the first time the control plane starts.
pub async fn ensure_local_server(pool: &PgPool) -> Result<(), sqlx::Error> {
    let count =
        sqlx::query_scalar!(r#"SELECT COUNT(*) AS "count!" FROM servers WHERE kind = 'local'"#)
            .fetch_one(pool)
            .await?;
    if count == 0 {
        sqlx::query!(
            r#"
            INSERT INTO servers (name, kind, status, description)
            VALUES ('local', 'local', 'unknown', 'This machine')
            "#
        )
        .execute(pool)
        .await?;
    }
    Ok(())
}
