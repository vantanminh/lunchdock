//! Rudder HTTP cache: a Pingora reverse proxy with an in-memory store, driven by the rules
//! file the panel writes.

mod admin;
mod proxy;
mod state;
mod storage;

use std::io::Write;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use pingora_cache::lock::{CacheKeyLockImpl, CacheLock};
use pingora_core::server::configuration::ServerConf;
use pingora_core::server::{Server, ShutdownWatch};
use pingora_core::services::background::{background_service, BackgroundService};
use pingora_core::services::listening::Service;

pub use state::{Outcome, Shared};
pub use storage::{primary_key, MemoryStore};

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub listen: SocketAddr,
    pub admin_listen: SocketAddr,
    pub rules_path: PathBuf,
    pub token_path: PathBuf,
    /// Overrides the budget from the rules file.
    pub memory_max_bytes: Option<u64>,
}

impl CacheConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.listen == self.admin_listen {
            anyhow::bail!("proxy listen and admin listen must differ");
        }
        Ok(())
    }
}

/// Read the admin token, creating a random one with mode 0600 when the file is missing.
pub fn load_or_create_token(path: &Path) -> anyhow::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(token) if !token.trim().is_empty() => return Ok(token.trim().to_owned()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => anyhow::bail!("read admin token {}: {error}", path.display()),
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let token: String = rudder_core::random_bytes::<32>()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(token.as_bytes())?;
    Ok(token)
}

struct Reloader {
    shared: &'static Shared,
}

#[async_trait]
impl BackgroundService for Reloader {
    async fn start(&self, mut shutdown: ShutdownWatch) {
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = shutdown.changed() => return,
                _ = ticker.tick() => {}
            }
            match self.shared.reload() {
                Ok(true) => tracing::info!(
                    version = self.shared.rules().version,
                    sites = self.shared.rules().sites.len(),
                    "cache rules reloaded"
                ),
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, "cache rules rejected; keeping the previous rules")
                }
            }
        }
    }
}

/// Start the proxy, admin, and reload services. Returns only on a startup error.
pub fn run(config: CacheConfig) -> anyhow::Result<()> {
    config.validate()?;
    let token = load_or_create_token(&config.token_path)?;
    let shared = Shared::new(config.rules_path.clone(), config.memory_max_bytes, token);
    if let Err(error) = shared.reload() {
        anyhow::bail!("cache rules: {error}");
    }
    let lock: &'static CacheKeyLockImpl =
        Box::leak(CacheLock::new_boxed(Duration::from_secs(10)) as Box<CacheKeyLockImpl>);

    let conf = ServerConf {
        threads: 2,
        grace_period_seconds: Some(0),
        graceful_shutdown_timeout_seconds: Some(1),
        ..ServerConf::default()
    };
    let mut server = Server::new_with_opt_and_conf(None, conf);
    server.bootstrap();
    let mut proxy = pingora_proxy::http_proxy_service(
        &server.configuration,
        proxy::CacheProxy { shared, lock },
    );
    proxy.add_tcp(&config.listen.to_string());
    let mut admin = Service::new("rudder-cache admin".into(), admin::Admin { shared });
    admin.add_tcp(&config.admin_listen.to_string());
    let reloader = background_service("rules reload", Reloader { shared });
    server.add_service(proxy);
    server.add_service(admin);
    server.add_service(reloader);
    tracing::info!(
        listen = %config.listen,
        admin = %config.admin_listen,
        rules = %config.rules_path.display(),
        "rudder-cache starting"
    );
    server.run_forever()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_identical_listen_addresses() {
        let addr: SocketAddr = "127.0.0.1:8080".parse().unwrap();
        let config = CacheConfig {
            listen: addr,
            admin_listen: addr,
            rules_path: PathBuf::from("rules.json"),
            token_path: PathBuf::from("admin.token"),
            memory_max_bytes: None,
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn token_is_created_once_with_private_mode() {
        let dir = std::env::temp_dir().join(format!("rudder-cache-token-{}", std::process::id()));
        let path = dir.join("admin.token");
        let first = load_or_create_token(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(load_or_create_token(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
