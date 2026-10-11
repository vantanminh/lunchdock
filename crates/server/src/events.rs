//! System events: deploys, backups, alerts, node changes, and updates.
//!
//! Rows land in `system_events`. Notification delivery reads rows whose
//! `delivered_at` is NULL. Messages must not contain secrets.

use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

pub struct Event {
    pub kind: String,
    pub severity: &'static str,
    pub title: String,
    pub message: String,
    pub server_id: Option<Uuid>,
    pub service_id: Option<Uuid>,
    pub payload: Value,
}

impl Event {
    fn new(kind: &str, severity: &'static str, title: impl Into<String>) -> Self {
        Self {
            kind: kind.to_owned(),
            severity,
            title: title.into(),
            message: String::new(),
            server_id: None,
            service_id: None,
            payload: json!({}),
        }
    }

    pub fn info(kind: &str, title: impl Into<String>) -> Self {
        Self::new(kind, "info", title)
    }

    pub fn warning(kind: &str, title: impl Into<String>) -> Self {
        Self::new(kind, "warning", title)
    }

    pub fn critical(kind: &str, title: impl Into<String>) -> Self {
        Self::new(kind, "critical", title)
    }

    pub fn message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    pub fn server(mut self, id: Uuid) -> Self {
        self.server_id = Some(id);
        self
    }

    pub fn service(mut self, id: Uuid) -> Self {
        self.service_id = Some(id);
        self
    }

    pub fn payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }
}

pub async fn record(pool: &PgPool, event: Event) {
    let result = sqlx::query!(
        r#"
        INSERT INTO system_events (kind, severity, server_id, service_id, title, message, payload)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        "#,
        event.kind,
        event.severity,
        event.server_id,
        event.service_id,
        event.title,
        event.message,
        event.payload,
    )
    .execute(pool)
    .await;
    if let Err(error) = result {
        tracing::error!(%error, kind = %event.kind, "failed to record system event");
    }
}
