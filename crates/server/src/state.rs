use std::num::NonZeroU32;
use std::sync::Arc;

use governor::clock::DefaultClock;
use governor::middleware::NoOpMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};
use sqlx::PgPool;

use crate::driver::LocalDriver;
use crate::Config;
use rudder_core::Cipher;

pub type LoginLimiter =
    RateLimiter<String, DefaultKeyedStateStore<String>, DefaultClock, NoOpMiddleware>;

/// Shared handle for HTTP handlers and background jobs.
#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub cipher: Arc<Cipher>,
    pub login_limiter: Arc<LoginLimiter>,
    pub local_driver: Arc<LocalDriver>,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config) -> Self {
        let cipher = Arc::new(Cipher::new(*config.master_key()));
        let per_minute = NonZeroU32::new(10).expect("login quota");
        let login_limiter = Arc::new(RateLimiter::keyed(Quota::per_minute(per_minute)));
        let local_driver = Arc::new(LocalDriver::connect(&config.data_dir));
        Self {
            pool,
            config: Arc::new(config),
            cipher,
            login_limiter,
            local_driver,
        }
    }
}
