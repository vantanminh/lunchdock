//! Operations every server can perform. Local and remote drivers implement this.

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures_core::Stream;
use serde::{Deserialize, Serialize};

use crate::Error;

/// Chunked bytes from or to a machine. An `Err` item ends the stream.
pub type ByteStream = Pin<Box<dyn Stream<Item = Result<Bytes, Error>> + Send>>;

/// A command run inside a container. `env` entries are `KEY=value` and are
/// never logged; pass secrets here instead of on the argv.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ExecSpec {
    pub command: Vec<String>,
    pub env: Vec<(String, String)>,
    pub user: Option<String>,
}

impl std::fmt::Debug for ExecSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let keys: Vec<&str> = self.env.iter().map(|(key, _)| key.as_str()).collect();
        f.debug_struct("ExecSpec")
            .field("command", &self.command)
            .field("env_keys", &keys)
            .field("user", &self.user)
            .finish()
    }
}

/// Where a tar stream is read from or extracted to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ArchiveSource {
    /// Docker named volume.
    Volume { name: String },
    /// Absolute path on the machine (a bind mount source or a chosen host path).
    HostPath { path: String },
}

impl ArchiveSource {
    pub fn validate(&self) -> Result<(), Error> {
        match self {
            Self::Volume { name } => {
                let ok = !name.is_empty()
                    && name.len() <= 128
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                    && name.as_bytes()[0].is_ascii_alphanumeric();
                if ok {
                    Ok(())
                } else {
                    Err(Error::Validation("invalid volume name".into()))
                }
            }
            Self::HostPath { path } => validate_host_path(path),
        }
    }
}

/// Absolute, normalized, and not the filesystem root.
pub fn validate_host_path(path: &str) -> Result<(), Error> {
    let ok = path.starts_with('/')
        && path.len() > 1
        && path.len() <= 1024
        && !path.contains(['\0', '\n', '\r', ':'])
        && !path.split('/').any(|part| part == ".." || part == ".");
    if ok {
        Ok(())
    } else {
        Err(Error::Validation(
            "host path must be absolute, without '..', ':' or control characters, and not /".into(),
        ))
    }
}

/// Point-in-time resource usage of one container.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContainerStats {
    pub id: String,
    pub name: String,
    pub cpu_percent: f64,
    pub memory_bytes: u64,
    pub memory_limit_bytes: u64,
    pub net_rx_bytes: u64,
    pub net_tx_bytes: u64,
    pub block_read_bytes: u64,
    pub block_write_bytes: u64,
    pub restart_count: i64,
    pub labels: Vec<(String, String)>,
}

/// Docker engine facts shown on a server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerInfo {
    pub version: String,
    pub os: String,
    pub cpus: i64,
    pub memory_bytes: i64,
}

/// Registry credentials passed to the engine. Callers must not log the password.
#[derive(Clone, Serialize, Deserialize)]
pub struct RegistryAuth {
    pub server: String,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for RegistryAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryAuth")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortMapping {
    pub container: u16,
    pub host: Option<u16>,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMount {
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

/// Desired container. Names are validated by the driver before they reach Docker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub name: String,
    pub image: String,
    pub env: Vec<(String, String)>,
    pub command: Vec<String>,
    pub ports: Vec<PortMapping>,
    pub volumes: Vec<VolumeMount>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub restart: String,
    pub network: Option<String>,
    pub labels: Vec<(String, String)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_password_is_redacted_in_debug() {
        let auth = RegistryAuth {
            server: "ghcr.io".into(),
            username: "octocat".into(),
            password: "super-secret".into(),
        };
        let rendered = format!("{auth:?}");
        assert!(rendered.contains("ghcr.io"));
        assert!(!rendered.contains("super-secret"));
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecOutput {
    pub exit_code: i64,
    pub output: String,
}

/// Everything the control plane does on a machine.
///
/// Business logic depends on this trait. `LocalDriver` talks to the local
/// socket. `RemoteDriver` will forward the same calls to an agent.
#[async_trait]
pub trait NodeDriver: Send + Sync {
    async fn docker_info(&self) -> Result<DockerInfo, Error>;
    async fn pull_image(&self, image: &str, auth: Option<RegistryAuth>) -> Result<(), Error>;
    async fn create_container(&self, spec: &ContainerSpec) -> Result<String, Error>;
    async fn start_container(&self, id: &str) -> Result<(), Error>;
    async fn stop_container(&self, id: &str, timeout_secs: i64) -> Result<(), Error>;
    async fn restart_container(&self, id: &str) -> Result<(), Error>;
    async fn remove_container(&self, id: &str, force: bool) -> Result<(), Error>;
    async fn container_running(&self, id: &str) -> Result<bool, Error>;
    async fn container_logs(&self, id: &str, tail: u32) -> Result<String, Error>;
    async fn exec(&self, id: &str, command: Vec<String>) -> Result<ExecOutput, Error>;
    async fn write_file(&self, relative_path: &str, bytes: &[u8]) -> Result<(), Error>;
    /// Run `docker compose`. `file` is relative to the node's data directory.
    async fn compose(
        &self,
        project: &str,
        file: &str,
        action: crate::ComposeAction,
    ) -> Result<String, Error>;
    async fn docker_health(&self, id: &str) -> Result<HealthState, Error>;
    async fn http_ready(&self, id: &str, port: u16, path: &str) -> Result<bool, Error>;
    async fn tcp_ready(&self, id: &str, port: u16) -> Result<bool, Error>;
    async fn image_digest(&self, image: &str) -> Result<String, Error>;

    /// Run a command and stream its stdout. A non-zero exit becomes the last item.
    async fn exec_read(&self, _id: &str, _spec: ExecSpec) -> Result<ByteStream, Error> {
        Err(unsupported("exec_read"))
    }

    /// Run a command with `input` on stdin and wait for it to exit.
    async fn exec_write(
        &self,
        _id: &str,
        _spec: ExecSpec,
        _input: ByteStream,
    ) -> Result<ExecOutput, Error> {
        Err(unsupported("exec_write"))
    }

    /// Stream a tar of a volume or host path. Nothing is staged on disk.
    async fn archive_read(&self, _source: &ArchiveSource) -> Result<ByteStream, Error> {
        Err(unsupported("archive_read"))
    }

    /// Extract a tar stream into a volume or host path.
    async fn archive_write(
        &self,
        _target: &ArchiveSource,
        _input: ByteStream,
    ) -> Result<(), Error> {
        Err(unsupported("archive_write"))
    }

    /// Remove a named volume. Used when a database service is deleted.
    async fn remove_volume(&self, _name: &str) -> Result<(), Error> {
        Err(unsupported("remove_volume"))
    }

    /// Stats for containers carrying the `rudder.managed=true` label.
    async fn container_stats(&self) -> Result<Vec<ContainerStats>, Error> {
        Err(unsupported("container_stats"))
    }

    /// Remove unused images and build cache. Returns reclaimed bytes.
    async fn prune(&self) -> Result<u64, Error> {
        Err(unsupported("prune"))
    }

    /// Read a file below the driver data directory.
    async fn read_file(&self, _relative_path: &str) -> Result<Vec<u8>, Error> {
        Err(unsupported("read_file"))
    }

    /// Remove objects from the node's HTTP cache.
    async fn cache_purge(
        &self,
        _request: &crate::CachePurge,
    ) -> Result<crate::CachePurgeResult, Error> {
        Err(unsupported("cache_purge"))
    }

    /// Counters and usage from the node's HTTP cache.
    async fn cache_stats(&self) -> Result<crate::CacheStats, Error> {
        Err(unsupported("cache_stats"))
    }
}

fn unsupported(op: &str) -> Error {
    Error::Node(format!("{op} is not supported by this driver"))
}

/// Docker HEALTHCHECK state. `None` means the image defines no check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthState {
    None,
    Starting,
    Healthy,
    Unhealthy,
}
