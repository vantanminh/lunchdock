//! The wire form of every [`NodeDriver`] call, shared by the panel's remote driver
//! and the agent.
//!
//! A request is JSON-encoded inside the gRPC `Command`. Unary calls answer with the
//! JSON-encoded return value. Streaming calls move their bytes over a separate
//! `Channel` RPC named by the command's `stream_id`.

use rudder_core::{
    ArchiveSource, CachePurge, ComposeAction, ComposeOptions, ContainerSpec, Error, ExecSpec,
    NodeDriver, RegistryAuth, ReplaceContainer,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    DockerInfo,
    PullImage {
        image: String,
        auth: Option<RegistryAuth>,
    },
    CreateContainer {
        spec: ContainerSpec,
    },
    StartContainer {
        id: String,
    },
    StopContainer {
        id: String,
        timeout_secs: i64,
    },
    RestartContainer {
        id: String,
    },
    RemoveContainer {
        id: String,
        force: bool,
    },
    ContainerRunning {
        id: String,
    },
    ContainerLogs {
        id: String,
        tail: u32,
    },
    Exec {
        id: String,
        command: Vec<String>,
    },
    WriteFile {
        path: String,
        #[serde(with = "hex_bytes")]
        data: Vec<u8>,
    },
    ReadFile {
        path: String,
    },
    Compose {
        project: String,
        file: String,
        action: ComposeAction,
        /// Registry logins for the run. Agents from before this field ignore it.
        #[serde(default)]
        logins: Vec<RegistryAuth>,
    },
    DockerHealth {
        id: String,
    },
    HttpReady {
        id: String,
        port: u16,
        path: String,
    },
    TcpReady {
        id: String,
        port: u16,
    },
    ImageDigest {
        image: String,
    },
    RemoveVolume {
        name: String,
    },
    ContainerStats,
    Prune,
    CachePurge {
        request: CachePurge,
    },
    CacheStats,
    ListContainers,
    HostStats,
    ContainerImage {
        name: String,
    },
    ReplaceContainer {
        request: ReplaceContainer,
    },
    /// Version and target triple of the agent itself. Answered by the agent.
    AgentInfo,
    /// Input stream: a new agent binary. The agent checks it, swaps it in, and
    /// restarts once the reply is sent.
    UpdateAgent {
        version: String,
        sha256: String,
    },
    /// Output stream: stdout of the command.
    ExecRead {
        id: String,
        spec: ExecSpec,
    },
    /// Input stream: stdin of the command. Answers with `ExecOutput`.
    ExecWrite {
        id: String,
        spec: ExecSpec,
    },
    /// Output stream: a tar.
    ArchiveRead {
        source: ArchiveSource,
    },
    /// Input stream: a tar to extract.
    ArchiveWrite {
        target: ArchiveSource,
    },
    /// Input stream: a tar of a repository work tree. `path` is relative to the
    /// node data directory.
    ExtractTree {
        path: String,
    },
    /// Output stream: one JSON `LogLine` per frame.
    SaveImage {
        image: String,
    },
    LoadImage,
    FollowLogs {
        id: String,
        since_unix: i64,
    },
    /// Both directions: keystrokes and resizes in, TTY output out.
    Terminal {
        id: String,
        cols: u16,
        rows: u16,
    },
}

/// Which way bytes move on the `Channel` for a streaming request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Streaming {
    None,
    /// Agent to panel.
    Output,
    /// Panel to agent; the command result arrives after the input ends.
    Input,
    Both,
}

impl Request {
    pub fn name(&self) -> &'static str {
        match self {
            Self::DockerInfo => "docker_info",
            Self::PullImage { .. } => "pull_image",
            Self::CreateContainer { .. } => "create_container",
            Self::StartContainer { .. } => "start_container",
            Self::StopContainer { .. } => "stop_container",
            Self::RestartContainer { .. } => "restart_container",
            Self::RemoveContainer { .. } => "remove_container",
            Self::ContainerRunning { .. } => "container_running",
            Self::ContainerLogs { .. } => "container_logs",
            Self::Exec { .. } => "exec",
            Self::WriteFile { .. } => "write_file",
            Self::ReadFile { .. } => "read_file",
            Self::Compose { .. } => "compose",
            Self::DockerHealth { .. } => "docker_health",
            Self::HttpReady { .. } => "http_ready",
            Self::TcpReady { .. } => "tcp_ready",
            Self::ImageDigest { .. } => "image_digest",
            Self::RemoveVolume { .. } => "remove_volume",
            Self::ContainerStats => "container_stats",
            Self::Prune => "prune",
            Self::CachePurge { .. } => "cache_purge",
            Self::CacheStats => "cache_stats",
            Self::ListContainers => "list_containers",
            Self::HostStats => "host_stats",
            Self::ContainerImage { .. } => "container_image",
            Self::ReplaceContainer { .. } => "replace_container",
            Self::AgentInfo => "agent_info",
            Self::UpdateAgent { .. } => "update_agent",
            Self::ExecRead { .. } => "exec_read",
            Self::ExecWrite { .. } => "exec_write",
            Self::ArchiveRead { .. } => "archive_read",
            Self::ArchiveWrite { .. } => "archive_write",
            Self::ExtractTree { .. } => "extract_tree",
            Self::SaveImage { .. } => "save_image",
            Self::LoadImage => "load_image",
            Self::FollowLogs { .. } => "follow_logs",
            Self::Terminal { .. } => "terminal",
        }
    }

    pub fn streaming(&self) -> Streaming {
        match self {
            Self::ExecRead { .. }
            | Self::ArchiveRead { .. }
            | Self::FollowLogs { .. }
            | Self::SaveImage { .. } => Streaming::Output,
            Self::ExecWrite { .. }
            | Self::ArchiveWrite { .. }
            | Self::ExtractTree { .. }
            | Self::LoadImage
            | Self::UpdateAgent { .. } => Streaming::Input,
            Self::Terminal { .. } => Streaming::Both,
            _ => Streaming::None,
        }
    }

    /// How long the panel waits for the result of a unary call.
    pub fn timeout_secs(&self) -> u64 {
        match self {
            Self::PullImage { .. } | Self::Prune => 1800,
            // Builds are slow; the panel enforces its own limit per stack action.
            Self::Compose { .. } => 4 * 3600,
            Self::StopContainer { timeout_secs, .. } => (*timeout_secs).max(0) as u64 + 60,
            Self::ReplaceContainer { request } => request.timeout_secs.min(1800) + 120,
            Self::UpdateAgent { .. } => 600,
            Self::Exec { .. } => 600,
            // Input streams answer only after the whole upload, so the panel's own
            // backup timeout bounds them instead.
            Self::ExecWrite { .. }
            | Self::ArchiveWrite { .. }
            | Self::ExtractTree { .. }
            | Self::LoadImage => 6 * 3600,
            _ => 120,
        }
    }
}

/// Run a unary request on a driver and JSON-encode the result.
pub async fn call(driver: &dyn NodeDriver, request: Request) -> Result<Value, Error> {
    fn json<T: Serialize>(value: T) -> Result<Value, Error> {
        serde_json::to_value(value).map_err(|error| Error::Node(error.to_string()))
    }
    match request {
        Request::DockerInfo => json(driver.docker_info().await?),
        Request::PullImage { image, auth } => json(driver.pull_image(&image, auth).await?),
        Request::CreateContainer { spec } => json(driver.create_container(&spec).await?),
        Request::StartContainer { id } => json(driver.start_container(&id).await?),
        Request::StopContainer { id, timeout_secs } => {
            json(driver.stop_container(&id, timeout_secs).await?)
        }
        Request::RestartContainer { id } => json(driver.restart_container(&id).await?),
        Request::RemoveContainer { id, force } => json(driver.remove_container(&id, force).await?),
        Request::ContainerRunning { id } => json(driver.container_running(&id).await?),
        Request::ContainerLogs { id, tail } => json(driver.container_logs(&id, tail).await?),
        Request::Exec { id, command } => json(driver.exec(&id, command).await?),
        Request::WriteFile { path, data } => json(driver.write_file(&path, &data).await?),
        Request::ReadFile { path } => json(hex::encode(driver.read_file(&path).await?)),
        Request::Compose {
            project,
            file,
            action,
            logins,
        } => {
            let options = ComposeOptions { logins };
            json(
                driver
                    .compose_run(&project, &file, action, &options, None)
                    .await?,
            )
        }
        Request::DockerHealth { id } => json(driver.docker_health(&id).await?),
        Request::HttpReady { id, port, path } => json(driver.http_ready(&id, port, &path).await?),
        Request::TcpReady { id, port } => json(driver.tcp_ready(&id, port).await?),
        Request::ImageDigest { image } => json(driver.image_digest(&image).await?),
        Request::RemoveVolume { name } => json(driver.remove_volume(&name).await?),
        Request::ContainerStats => json(driver.container_stats().await?),
        Request::Prune => json(driver.prune().await?),
        Request::CachePurge { request } => json(driver.cache_purge(&request).await?),
        Request::CacheStats => json(driver.cache_stats().await?),
        Request::ListContainers => json(driver.list_containers().await?),
        Request::HostStats => json(driver.host_stats().await?),
        Request::ContainerImage { name } => json(driver.container_image(&name).await?),
        Request::ReplaceContainer { request } => json(driver.replace_container(&request).await?),
        Request::AgentInfo => Err(Error::Node("agent_info is answered by the agent".into())),
        streaming => Err(Error::Node(format!(
            "{} needs a stream channel",
            streaming.name()
        ))),
    }
}

/// `ReadFile` answers with hex so binary files survive JSON.
pub fn decode_read_file(value: Value) -> Result<Vec<u8>, Error> {
    let text: String =
        serde_json::from_value(value).map_err(|error| Error::Node(error.to_string()))?;
    hex::decode(text).map_err(|error| Error::Node(error.to_string()))
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(deserializer)?;
        hex::decode(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip_with_their_op_tag() {
        let request = Request::WriteFile {
            path: "traefik/dynamic/app.yml".into(),
            data: vec![0, 159, 255],
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["op"], "write_file");
        assert_eq!(encoded["data"], "009fff");
        let decoded: Request = serde_json::from_value(encoded).unwrap();
        assert!(matches!(decoded, Request::WriteFile { data, .. } if data == vec![0, 159, 255]));
        let compose: Request = serde_json::from_str(
            r#"{"op":"compose","project":"shop","file":"stacks/shop.yml","action":"destroy"}"#,
        )
        .unwrap();
        // Requests written before registry logins existed still decode.
        assert!(matches!(
            compose,
            Request::Compose {
                action: ComposeAction::Destroy,
                ref logins,
                ..
            } if logins.is_empty()
        ));
    }

    #[test]
    fn streaming_directions_match_the_driver_methods() {
        let id = || "c1".to_owned();
        assert_eq!(
            Request::FollowLogs {
                id: id(),
                since_unix: 0
            }
            .streaming(),
            Streaming::Output
        );
        assert_eq!(
            Request::ArchiveWrite {
                target: ArchiveSource::Volume { name: "v".into() }
            }
            .streaming(),
            Streaming::Input
        );
        assert_eq!(
            Request::Terminal {
                id: id(),
                cols: 80,
                rows: 24
            }
            .streaming(),
            Streaming::Both
        );
        assert_eq!(Request::Prune.streaming(), Streaming::None);
    }

    #[test]
    fn registry_password_is_not_in_the_op_name_or_debug() {
        let request = Request::PullImage {
            image: "ghcr.io/acme/web:1".into(),
            auth: Some(RegistryAuth {
                server: "ghcr.io".into(),
                username: "bot".into(),
                password: "hunter2-secret".into(),
            }),
        };
        assert_eq!(request.name(), "pull_image");
        assert!(!format!("{request:?}").contains("hunter2-secret"));
    }
}
