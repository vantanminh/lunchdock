//! Central log pipeline: batched inserts into `log_entries` and a broadcast for live tail.

pub mod collector;
pub mod layer;
pub mod retention;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::{broadcast, mpsc};
use utoipa::ToSchema;
use uuid::Uuid;

/// A line on its way to the database.
#[derive(Debug, Clone)]
pub struct NewLog {
    pub logged_at: DateTime<Utc>,
    pub server_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub source: String,
    pub level: String,
    pub message: String,
    pub http_status: Option<i32>,
    pub fields: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LogView {
    pub id: i64,
    pub logged_at: DateTime<Utc>,
    pub server_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub source: String,
    pub level: String,
    pub message: String,
    pub http_status: Option<i32>,
    #[schema(value_type = Object)]
    pub fields: serde_json::Value,
}

const BATCH: usize = 500;
const QUEUE: usize = 20_000;

pub struct LogHub {
    tx: mpsc::Sender<NewLog>,
    rx: Mutex<Option<mpsc::Receiver<NewLog>>>,
    live: broadcast::Sender<Arc<LogView>>,
}

impl Default for LogHub {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel(QUEUE);
        let (live, _) = broadcast::channel(4096);
        Self {
            tx,
            rx: Mutex::new(Some(rx)),
            live,
        }
    }
}

impl LogHub {
    pub fn sender(&self) -> mpsc::Sender<NewLog> {
        self.tx.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<LogView>> {
        self.live.subscribe()
    }

    pub fn publish(&self, rows: Vec<LogView>) {
        for row in rows {
            let _ = self.live.send(Arc::new(row));
        }
    }

    /// Drain the queue into the database until the process exits. Call once.
    pub async fn run_writer(self: Arc<Self>, pool: PgPool) {
        let Some(mut rx) = self.rx.lock().ok().and_then(|mut slot| slot.take()) else {
            return;
        };
        let mut batch = Vec::with_capacity(BATCH);
        loop {
            let first = rx.recv().await;
            let Some(first) = first else { return };
            batch.push(first);
            let deadline = tokio::time::sleep(Duration::from_millis(500));
            tokio::pin!(deadline);
            while batch.len() < BATCH {
                tokio::select! {
                    item = rx.recv() => match item {
                        Some(item) => batch.push(item),
                        None => break,
                    },
                    _ = &mut deadline => break,
                }
            }
            match insert_batch(&pool, &batch).await {
                Ok(rows) => self.publish(rows),
                Err(error) => {
                    tracing::warn!(target: "rudder_server::logs", %error, lines = batch.len(), "log batch dropped")
                }
            }
            batch.clear();
        }
    }
}

pub async fn insert_batch(pool: &PgPool, batch: &[NewLog]) -> Result<Vec<LogView>, sqlx::Error> {
    if batch.is_empty() {
        return Ok(Vec::new());
    }
    let mut logged_at = Vec::with_capacity(batch.len());
    let mut server_id = Vec::with_capacity(batch.len());
    let mut project_id = Vec::with_capacity(batch.len());
    let mut service_id = Vec::with_capacity(batch.len());
    let mut source = Vec::with_capacity(batch.len());
    let mut level = Vec::with_capacity(batch.len());
    let mut message = Vec::with_capacity(batch.len());
    let mut status = Vec::with_capacity(batch.len());
    let mut fields = Vec::with_capacity(batch.len());
    for line in batch {
        logged_at.push(line.logged_at);
        server_id.push(line.server_id);
        project_id.push(line.project_id);
        service_id.push(line.service_id);
        source.push(line.source.clone());
        level.push(line.level.clone());
        message.push(line.message.replace('\0', ""));
        status.push(line.http_status);
        fields.push(line.fields.clone());
    }
    sqlx::query_as!(
        LogView,
        r#"
        INSERT INTO log_entries (
            logged_at, server_id, project_id, service_id, source, level, message, http_status,
            fields
        )
        SELECT * FROM UNNEST(
            $1::timestamptz[], $2::uuid[], $3::uuid[], $4::uuid[], $5::text[], $6::text[],
            $7::text[], $8::int[], $9::jsonb[]
        )
        RETURNING id, logged_at, server_id, project_id, service_id, source, level, message,
                  http_status, fields
        "#,
        &logged_at,
        &server_id as &[Option<Uuid>],
        &project_id as &[Option<Uuid>],
        &service_id as &[Option<Uuid>],
        &source,
        &level,
        &message,
        &status as &[Option<i32>],
        &fields,
    )
    .fetch_all(pool)
    .await
}

/// Filters shared by search, export, and live tail.
#[derive(Debug, Clone, Default)]
pub struct LogFilter {
    pub source: Option<String>,
    pub service_id: Option<Uuid>,
    pub server_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub levels: Vec<String>,
    pub search: Option<String>,
    pub status_min: Option<i32>,
    pub status_max: Option<i32>,
}

impl LogFilter {
    pub fn matches(&self, row: &LogView) -> bool {
        self.source.as_ref().is_none_or(|s| *s == row.source)
            && self.service_id.is_none_or(|id| Some(id) == row.service_id)
            && self.server_id.is_none_or(|id| Some(id) == row.server_id)
            && self.project_id.is_none_or(|id| Some(id) == row.project_id)
            && (self.levels.is_empty() || self.levels.contains(&row.level))
            && self
                .search
                .as_ref()
                .is_none_or(|needle| row.message.to_lowercase().contains(&needle.to_lowercase()))
            && self
                .status_min
                .is_none_or(|min| row.http_status.is_some_and(|s| s >= min))
            && self
                .status_max
                .is_none_or(|max| row.http_status.is_some_and(|s| s <= max))
    }
}

pub struct Page {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub before: Option<(DateTime<Utc>, i64)>,
    pub limit: i64,
}

pub async fn search(
    pool: &PgPool,
    filter: &LogFilter,
    page: &Page,
) -> Result<Vec<LogView>, sqlx::Error> {
    let pattern = filter
        .search
        .as_ref()
        .map(|needle| format!("%{}%", rudder_core::escape_like(needle)));
    sqlx::query_as!(
        LogView,
        r#"
        SELECT id, logged_at, server_id, project_id, service_id, source, level, message,
               http_status, fields
        FROM log_entries
        WHERE logged_at >= $1 AND logged_at <= $2
          AND ($3::text IS NULL OR source = $3)
          AND ($4::uuid IS NULL OR service_id = $4)
          AND ($5::uuid IS NULL OR server_id = $5)
          AND ($6::uuid IS NULL OR project_id = $6)
          AND (cardinality($7::text[]) = 0 OR level = ANY($7))
          AND ($8::text IS NULL OR message ILIKE $8 ESCAPE '\')
          AND ($9::int IS NULL OR http_status >= $9)
          AND ($10::int IS NULL OR http_status <= $10)
          AND ($11::timestamptz IS NULL OR (logged_at, id) < ($11, $12::bigint))
        ORDER BY logged_at DESC, id DESC
        LIMIT $13
        "#,
        page.since,
        page.until,
        filter.source,
        filter.service_id,
        filter.server_id,
        filter.project_id,
        &filter.levels,
        pattern,
        filter.status_min,
        filter.status_max,
        page.before.map(|(time, _)| time),
        page.before.map_or(0, |(_, id)| id),
        page.limit,
    )
    .fetch_all(pool)
    .await
}
