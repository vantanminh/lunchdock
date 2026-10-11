use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use governor::clock::DefaultClock;
use governor::middleware::NoOpMiddleware;
use governor::state::keyed::DefaultKeyedStateStore;
use governor::{Quota, RateLimiter};
use sqlx::PgPool;
use tokio::sync::{Mutex, Semaphore};
use uuid::Uuid;

use crate::agents::AgentRegistry;
use crate::driver::LocalDriver;
use crate::Config;
use rudder_core::Cipher;

/// Per-server backup concurrency limit and its semaphore.
pub type BackupSlots = Mutex<HashMap<Uuid, (usize, Arc<Semaphore>)>>;

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
    pub agents: Arc<AgentRegistry>,
    pub backup_slots: Arc<BackupSlots>,
}

impl AppState {
    pub fn new(pool: PgPool, config: Config) -> Self {
        let cipher = Arc::new(Cipher::new(*config.master_key()));
        let per_minute = NonZeroU32::new(10).expect("login quota");
        let login_limiter = Arc::new(RateLimiter::keyed(Quota::per_minute(per_minute)));
        let local_driver = Arc::new(
            LocalDriver::connect(&config.data_dir, config.docker_socket.as_deref())
                .with_cache_admin(&config.cache_admin_url),
        );
        Self {
            pool,
            config: Arc::new(config),
            cipher,
            login_limiter,
            local_driver,
            agents: Arc::new(AgentRegistry::default()),
            backup_slots: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}
