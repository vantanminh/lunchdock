//! Docker Engine on the panel host, via the Unix socket and `docker compose`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use bollard::auth::DockerCredentials;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, HostConfig, PortBinding, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, LogsOptionsBuilder,
    RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
};
use bollard::Docker;
use futures_util::StreamExt;
use rudder_core::{
    compose_args, validate_env_key, validate_image_ref, validate_name, ComposeAction,
    ContainerSpec, DockerInfo, Error, ExecOutput, NodeDriver, RegistryAuth,
};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

pub struct LocalDriver {
    docker: Option<Docker>,
    root: PathBuf,
}

impl LocalDriver {
    /// Build a client for the local socket. A missing socket does not stop the panel;
    /// Docker calls fail until the engine is available.
    pub fn connect(root: impl Into<PathBuf>) -> Self {
        let docker = match Docker::connect_with_unix_defaults() {
            Ok(docker) => Some(docker),
            Err(error) => {
                tracing::warn!(%error, "docker client was not created");
                None
            }
        };
        Self {
            docker,
            root: root.into(),
        }
    }

    fn docker(&self) -> Result<&Docker, Error> {
        self.docker
            .as_ref()
            .ok_or_else(|| Error::Node("docker socket is not available".into()))
    }

    fn node(error: impl std::fmt::Display) -> Error {
        Error::Node(error.to_string())
    }
}

#[async_trait]
impl NodeDriver for LocalDriver {
    async fn docker_info(&self) -> Result<DockerInfo, Error> {
        let version = self.docker()?.version().await.map_err(Self::node)?;
        let info = self.docker()?.info().await.map_err(Self::node)?;
        Ok(DockerInfo {
            version: version.version.unwrap_or_default(),
            os: info
                .operating_system
                .or(version.os)
                .unwrap_or_else(|| "unknown".into()),
            cpus: info.ncpu.unwrap_or(0),
            memory_bytes: info.mem_total.unwrap_or(0),
        })
    }

    async fn pull_image(&self, image: &str, auth: Option<RegistryAuth>) -> Result<(), Error> {
        validate_image_ref(image)?;
        let options = CreateImageOptionsBuilder::default()
            .from_image(image)
            .build();
        let credentials = auth.map(|auth| DockerCredentials {
            username: Some(auth.username),
            password: Some(auth.password),
            serveraddress: Some(auth.server),
            ..DockerCredentials::default()
        });
        let mut stream = self
            .docker()?
            .create_image(Some(options), None, credentials);
        while let Some(event) = stream.next().await {
            event.map_err(Self::node)?;
        }
        Ok(())
    }

    async fn create_container(&self, spec: &ContainerSpec) -> Result<String, Error> {
        validate_name(&spec.name)?;
        validate_image_ref(&spec.image)?;
        let mut env = Vec::new();
        for (key, value) in &spec.env {
            validate_env_key(key)?;
            if value.contains('\0') {
                return Err(Error::Validation(
                    "environment values cannot contain NUL".into(),
                ));
            }
            env.push(format!("{key}={value}"));
        }
        let mut exposed = Vec::new();
        let mut bindings = HashMap::new();
        for port in &spec.ports {
            let key = format!("{}/{}", port.container, protocol(&port.protocol)?);
            exposed.push(key.clone());
            if let Some(host) = port.host {
                bindings.insert(
                    key,
                    Some(vec![PortBinding {
                        host_ip: Some("0.0.0.0".into()),
                        host_port: Some(host.to_string()),
                    }]),
                );
            }
        }
        let mut binds = Vec::new();
        for volume in &spec.volumes {
            if volume.source.contains('\0') || volume.target.contains('\0') {
                return Err(Error::Validation("invalid volume path".into()));
            }
            let mut bind = format!("{}:{}", volume.source, volume.target);
            if volume.read_only {
                bind.push_str(":ro");
            }
            binds.push(bind);
        }
        let labels: HashMap<String, String> = spec.labels.iter().cloned().collect();
        let body = ContainerCreateBody {
            image: Some(spec.image.clone()),
            env: Some(env),
            cmd: if spec.command.is_empty() {
                None
            } else {
                Some(spec.command.clone())
            },
            exposed_ports: if exposed.is_empty() {
                None
            } else {
                Some(exposed)
            },
            labels: if labels.is_empty() {
                None
            } else {
                Some(labels)
            },
            host_config: Some(HostConfig {
                memory: spec.memory_bytes,
                nano_cpus: spec.nano_cpus,
                binds: if binds.is_empty() { None } else { Some(binds) },
                port_bindings: if bindings.is_empty() {
                    None
                } else {
                    Some(bindings)
                },
                restart_policy: Some(RestartPolicy {
                    name: Some(restart_policy(&spec.restart)?),
                    maximum_retry_count: None,
                }),
                network_mode: spec.network.clone(),
                ..HostConfig::default()
            }),
            ..ContainerCreateBody::default()
        };
        let options = CreateContainerOptionsBuilder::default()
            .name(&spec.name)
            .build();
        let created = self
            .docker()?
            .create_container(Some(options), body)
            .await
            .map_err(Self::node)?;
        Ok(created.id)
    }

    async fn start_container(&self, id: &str) -> Result<(), Error> {
        self.docker()?
            .start_container(id, None)
            .await
            .map_err(Self::node)
    }

    async fn stop_container(&self, id: &str, timeout_secs: i64) -> Result<(), Error> {
        let options = StopContainerOptionsBuilder::default()
            .t(i32::try_from(timeout_secs).unwrap_or(10))
            .build();
        self.docker()?
            .stop_container(id, Some(options))
            .await
            .map_err(Self::node)
    }

    async fn restart_container(&self, id: &str) -> Result<(), Error> {
        self.docker()?
            .restart_container(id, None)
            .await
            .map_err(Self::node)
    }

    async fn remove_container(&self, id: &str, force: bool) -> Result<(), Error> {
        let options = RemoveContainerOptionsBuilder::default()
            .force(force)
            .build();
        self.docker()?
            .remove_container(id, Some(options))
            .await
            .map_err(Self::node)
    }

    async fn container_running(&self, id: &str) -> Result<bool, Error> {
        let inspected = self
            .docker()?
            .inspect_container(id, None)
            .await
            .map_err(Self::node)?;
        Ok(inspected
            .state
            .and_then(|state| state.running)
            .unwrap_or(false))
    }

    async fn container_logs(&self, id: &str, tail: u32) -> Result<String, Error> {
        let options = LogsOptionsBuilder::default()
            .stdout(true)
            .stderr(true)
            .tail(&tail.to_string())
            .build();
        let mut stream = self.docker()?.logs(id, Some(options));
        let mut output = String::new();
        while let Some(line) = stream.next().await {
            let line = line.map_err(Self::node)?;
            match line {
                LogOutput::StdOut { message }
                | LogOutput::StdErr { message }
                | LogOutput::Console { message } => {
                    output.push_str(&String::from_utf8_lossy(&message));
                }
                LogOutput::StdIn { .. } => {}
            }
        }
        Ok(output)
    }

    async fn exec(&self, id: &str, command: Vec<String>) -> Result<ExecOutput, Error> {
        if command.is_empty() || command.iter().any(|part| part.contains('\0')) {
            return Err(Error::Validation("exec command is empty".into()));
        }
        let created = self
            .docker()?
            .create_exec(
                id,
                CreateExecOptions {
                    cmd: Some(command),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    ..CreateExecOptions::default()
                },
            )
            .await
            .map_err(Self::node)?;
        let mut output = String::new();
        match self
            .docker()?
            .start_exec(&created.id, None)
            .await
            .map_err(Self::node)?
        {
            StartExecResults::Attached {
                output: mut stream,
                mut input,
            } => {
                input.shutdown().await.ok();
                while let Some(line) = stream.next().await {
                    let line = line.map_err(Self::node)?;
                    output.push_str(&line.to_string());
                }
            }
            StartExecResults::Detached => {}
        }
        let inspected = self
            .docker()?
            .inspect_exec(&created.id)
            .await
            .map_err(Self::node)?;
        Ok(ExecOutput {
            exit_code: inspected.exit_code.unwrap_or(-1),
            output,
        })
    }

    async fn write_file(&self, relative_path: &str, bytes: &[u8]) -> Result<(), Error> {
        let path = confined_path(&self.root, relative_path)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(Self::node)?;
        }
        tokio::fs::write(path, bytes).await.map_err(Self::node)?;
        Ok(())
    }

    async fn compose(
        &self,
        project: &str,
        file: &str,
        action: ComposeAction,
    ) -> Result<String, Error> {
        let args = compose_args(project, file, action)?;
        let output = Command::new("docker")
            .args(&args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .map_err(Self::node)?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if !output.stderr.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if output.status.success() {
            Ok(text)
        } else {
            Err(Error::Node(format!(
                "docker compose exited {}: {text}",
                output.status
            )))
        }
    }
}

fn protocol(value: &str) -> Result<&str, Error> {
    match value {
        "tcp" | "udp" => Ok(value),
        _ => Err(Error::Validation("port protocol must be tcp or udp".into())),
    }
}

fn restart_policy(value: &str) -> Result<RestartPolicyNameEnum, Error> {
    match value {
        "no" => Ok(RestartPolicyNameEnum::NO),
        "always" => Ok(RestartPolicyNameEnum::ALWAYS),
        "unless-stopped" => Ok(RestartPolicyNameEnum::UNLESS_STOPPED),
        "on-failure" => Ok(RestartPolicyNameEnum::ON_FAILURE),
        _ => Err(Error::Validation(
            "restart policy must be no, always, unless-stopped, or on-failure".into(),
        )),
    }
}

/// Join `relative` onto `root`, rejecting absolute paths and `..`.
pub fn confined_path(root: &Path, relative: &str) -> Result<PathBuf, Error> {
    if relative.is_empty()
        || relative.contains('\0')
        || Path::new(relative).is_absolute()
        || relative.split(['/', '\\']).any(|part| part == "..")
    {
        return Err(Error::Validation("path escapes the data directory".into()));
    }
    Ok(root.join(relative))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_writes_stay_inside_the_data_directory() {
        let root = Path::new("/var/lib/rudder");
        assert_eq!(
            confined_path(root, "traefik/dynamic/app.yml").unwrap(),
            PathBuf::from("/var/lib/rudder/traefik/dynamic/app.yml")
        );
        assert!(confined_path(root, "../etc/passwd").is_err());
        assert!(confined_path(root, "/etc/passwd").is_err());
    }
}
