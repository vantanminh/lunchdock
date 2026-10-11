//! Structured tracing setup.
//!
//! Panel logs are JSON by default so the log pipeline can ingest them without
//! a second parser. `RUDDER_LOG_FORMAT=pretty` is for local terminals.

use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

use crate::config::{Config, LogFormat};

/// Install a global subscriber. Later calls are ignored so tests can share a process.
pub fn init(config: &Config) {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let registry = tracing_subscriber::registry().with(filter);
    match config.log_format {
        LogFormat::Json => {
            let _ = registry
                .with(tracing_subscriber::fmt::layer().json())
                .try_init();
        }
        LogFormat::Pretty => {
            let _ = registry.with(tracing_subscriber::fmt::layer()).try_init();
        }
    }
}
