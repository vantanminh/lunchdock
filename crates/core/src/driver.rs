//! Operations every server can perform. Local and remote drivers implement this.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Error;

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
    async fn compose(
        &self,
        project: &str,
        file: &str,
        action: crate::ComposeAction,
    ) -> Result<String, Error>;
}
