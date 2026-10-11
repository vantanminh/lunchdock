//! Cache server configuration. The Pingora proxy loop is attached in the cache milestone.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub listen: SocketAddr,
    pub admin_listen: SocketAddr,
    pub rules_path: PathBuf,
}

impl CacheConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.listen == self.admin_listen {
            anyhow::bail!("proxy listen and admin listen must differ");
        }
        Ok(())
    }
}

/// Read the rules file so a bad document fails before the process detaches.
pub fn read_rules(config: &CacheConfig) -> anyhow::Result<serde_json::Value> {
    config.validate()?;
    let raw = std::fs::read_to_string(&config.rules_path).map_err(|error| {
        anyhow::anyhow!("read cache rules {}: {error}", config.rules_path.display())
    })?;
    let value = serde_json::from_str(&raw)?;
    Ok(value)
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
        };
        assert!(config.validate().is_err());
    }
}
