//! Tracing layer that copies panel events into the central log store.

use std::fmt::Write as _;
use std::sync::OnceLock;

use chrono::Utc;
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use super::NewLog;

static SINK: OnceLock<mpsc::Sender<NewLog>> = OnceLock::new();

/// Start forwarding panel events. Only the first call wins.
pub fn install(sender: mpsc::Sender<NewLog>) {
    let _ = SINK.set(sender);
}

pub struct PanelLayer;

const SKIPPED_TARGETS: [&str; 6] = [
    "sqlx",
    "rudder_server::logs",
    "hyper",
    "h2",
    "tower",
    "rustls",
];

impl<S: Subscriber> Layer<S> for PanelLayer {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let Some(sink) = SINK.get() else { return };
        let meta = event.metadata();
        if *meta.level() > Level::INFO {
            return;
        }
        let target = meta.target();
        if SKIPPED_TARGETS
            .iter()
            .any(|skip| target == *skip || target.starts_with(&format!("{skip}::")))
        {
            return;
        }
        let mut visitor = Collect::default();
        event.record(&mut visitor);
        visitor
            .fields
            .insert("target".into(), serde_json::Value::String(target.into()));
        let level = match *meta.level() {
            Level::ERROR => "error",
            Level::WARN => "warn",
            _ => "info",
        };
        let _ = sink.try_send(NewLog {
            logged_at: Utc::now(),
            server_id: None,
            project_id: None,
            service_id: None,
            source: "panel".into(),
            level: level.into(),
            message: visitor.message,
            http_status: None,
            fields: serde_json::Value::Object(visitor.fields),
        });
    }
}

#[derive(Default)]
struct Collect {
    message: String,
    fields: serde_json::Map<String, serde_json::Value>,
}

impl Visit for Collect {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else {
            self.fields
                .insert(field.name().into(), serde_json::Value::String(value.into()));
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(field.name().into(), value.into());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(field.name().into(), value.into());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields.insert(field.name().into(), value.into());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else {
            self.fields.insert(
                field.name().into(),
                serde_json::Value::String(format!("{value:?}")),
            );
        }
    }
}
