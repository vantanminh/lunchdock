use std::sync::Arc;

use sqlx::PgPool;

use crate::Config;

/// Shared handle for HTTP handlers and background jobs.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
}
