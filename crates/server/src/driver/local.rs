//! Docker Engine on the panel host, via the Unix socket and `docker compose`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use bollard::auth::DockerCredentials;
use bollard::container::LogOutput;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::HealthStatusEnum;
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
    compose_args, validate_env_key, validate_image_ref, validate_name, ArchiveSource, ByteStream,
    ComposeAction, ContainerSpec, DockerInfo, Error, ExecOutput, ExecSpec, HealthState, NodeDriver,
    RegistryAuth, VolumeMount, CACHE_TOKEN_FILE, PROXY_NETWORK,
};
use rudder_core::{
    ContainerInfo, LogLine, LogLineStream, LogStreamKind, TerminalInput, TerminalInputStream,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// Small image with `tar` used to read and write volumes and host paths.
pub const HELPER_IMAGE: &str = "alpine:3.20";

/// Fixed argv for web terminals: bash when the image has it, otherwise sh.
const TERMINAL_SHELL: &[&str] = &[
    "/bin/sh",
    "-c",
    "if command -v bash >/dev/null 2>&1; then exec bash -l; else exec sh -l; fi",
];

/// Admin listener of the cache server on the panel host.
pub const DEFAULT_CACHE_ADMIN: &str = "http://127.0.0.1:9090";

pub struct LocalDriver {
    docker: Option<Docker>,
    root: PathBuf,
    socket: Option<PathBuf>,
    cache_admin: String,
    host_proc: PathBuf,
    cpu_previous: std::sync::Mutex<Option<rudder_core::CpuTimes>>,
}

impl LocalDriver {
    /// Build a client for the local socket. A missing socket does not stop the panel;
    /// Docker calls fail until the engine is available.
    pub fn connect(root: impl Into<PathBuf>, socket: Option<&std::path::Path>) -> Self {
        let connected = match socket {
            Some(path) => Docker::connect_with_unix(
                &path.to_string_lossy(),
                120,
                bollard::API_DEFAULT_VERSION,
            ),
            None => Docker::connect_with_unix_defaults(),
        };
        let docker = match connected {
            Ok(docker) => Some(docker),
            Err(error) => {
                tracing::warn!(%error, "docker client was not created");
                None
            }
        };
        Self {
            docker,
            root: root.into(),
            socket: socket.map(std::path::Path::to_path_buf),
            cache_admin: DEFAULT_CACHE_ADMIN.into(),
            host_proc: PathBuf::from("/proc"),
            cpu_previous: std::sync::Mutex::new(None),
        }
    }

    pub fn with_host_proc(mut self, path: &Path) -> Self {
        self.host_proc = path.to_path_buf();
        self
    }

    /// Base URL of the local cache server's admin listener.
    pub fn with_cache_admin(mut self, url: &str) -> Self {
        self.cache_admin = url.trim_end_matches('/').to_owned();
        self
    }

    async fn cache_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&rudder_core::CachePurge>,
    ) -> Result<Vec<u8>, Error> {
        let token = tokio::fs::read_to_string(confined_path(&self.root, CACHE_TOKEN_FILE)?)
            .await
            .map_err(|error| Error::Node(format!("cache admin token is not readable: {error}")))?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(Self::node)?;
        let mut request = client
            .request(method, format!("{}{path}", self.cache_admin))
            .bearer_auth(token.trim());
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Error::Node(format!("cache server is not reachable: {error}")))?;
        let status = response.status();
        let bytes = response.bytes().await.map_err(Self::node)?;
        if !status.is_success() {
            let message = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|value| value["error"].as_str().map(str::to_owned))
                .unwrap_or_else(|| status.to_string());
            return Err(Error::Node(format!("cache server: {message}")));
        }
        Ok(bytes.to_vec())
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
        if spec.network.as_deref() == Some(PROXY_NETWORK) {
            self.ensure_network(PROXY_NETWORK).await?;
        }
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
        tokio::fs::write(&path, bytes).await.map_err(Self::node)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(Self::node)?;
        }
        Ok(())
    }

    async fn compose(
        &self,
        project: &str,
        file: &str,
        action: ComposeAction,
    ) -> Result<String, Error> {
        let path = confined_path(&self.root, file)?;
        let path = path
            .to_str()
            .ok_or_else(|| Error::Validation("compose file path is not UTF-8".into()))?;
        let args = compose_args(project, path, action)?;
        let mut command = Command::new("docker");
        if let Some(socket) = &self.socket {
            command.env("DOCKER_HOST", format!("unix://{}", socket.display()));
        }
        let output = command
            .args(&args)
            .stdin(Stdio::null())
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

    async fn docker_health(&self, id: &str) -> Result<HealthState, Error> {
        let inspected = self
            .docker()?
            .inspect_container(id, None)
            .await
            .map_err(Self::node)?;
        let status = inspected
            .state
            .and_then(|state| state.health)
            .and_then(|health| health.status);
        Ok(match status {
            Some(HealthStatusEnum::HEALTHY) => HealthState::Healthy,
            Some(HealthStatusEnum::STARTING) => HealthState::Starting,
            Some(HealthStatusEnum::UNHEALTHY) => HealthState::Unhealthy,
            _ => HealthState::None,
        })
    }

    async fn http_ready(&self, id: &str, port: u16, path: &str) -> Result<bool, Error> {
        if port == 0
            || !path.starts_with('/')
            || path
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || byte == 0)
        {
            return Err(Error::Validation("invalid health check path".into()));
        }
        let Some(ip) = container_ip(&self.inspect(id).await?) else {
            return Ok(false);
        };
        let address = format!("{ip}:{port}");
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::net::TcpStream::connect(&address),
        )
        .await;
        let mut stream = match stream {
            Ok(Ok(stream)) => stream,
            _ => return Ok(false),
        };
        let request = format!("GET {path} HTTP/1.0\r\nHost: {ip}\r\nConnection: close\r\n\r\n");
        if stream.write_all(request.as_bytes()).await.is_err() {
            return Ok(false);
        }
        let mut buffer = [0_u8; 64];
        let read = stream.read(&mut buffer).await.unwrap_or(0);
        Ok(http_status_ok(&buffer[..read]))
    }

    async fn tcp_ready(&self, id: &str, port: u16) -> Result<bool, Error> {
        if port == 0 {
            return Err(Error::Validation("invalid health check port".into()));
        }
        let Some(ip) = container_ip(&self.inspect(id).await?) else {
            return Ok(false);
        };
        let address = format!("{ip}:{port}");
        let connected = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            tokio::net::TcpStream::connect(&address),
        )
        .await;
        Ok(matches!(connected, Ok(Ok(_))))
    }

    async fn image_digest(&self, image: &str) -> Result<String, Error> {
        validate_image_ref(image)?;
        let inspected = self
            .docker()?
            .inspect_image(image)
            .await
            .map_err(Self::node)?;
        if let Some(digest) = inspected
            .repo_digests
            .into_iter()
            .flatten()
            .find(|digest| !digest.is_empty())
        {
            return Ok(digest);
        }
        inspected
            .id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| Error::Node("image has no digest".into()))
    }

    async fn exec_read(&self, id: &str, spec: ExecSpec) -> Result<ByteStream, Error> {
        let docker = self.docker()?.clone();
        let tool = check_exec(&spec)?;
        let created = docker
            .create_exec(id, exec_options(&spec, false))
            .await
            .map_err(Self::node)?;
        let started = docker
            .start_exec(&created.id, None)
            .await
            .map_err(Self::node)?;
        let StartExecResults::Attached { output, mut input } = started else {
            return Err(Error::Node("exec did not attach".into()));
        };
        input.shutdown().await.ok();
        drop(input);
        let state = ReadState::Running {
            output,
            stderr: Vec::new(),
            docker,
            exec_id: created.id,
            tool,
        };
        let stream = futures_util::stream::unfold(state, |state| async move {
            let ReadState::Running {
                mut output,
                mut stderr,
                docker,
                exec_id,
                tool,
            } = state
            else {
                return None;
            };
            loop {
                match output.next().await {
                    Some(Ok(LogOutput::StdOut { message })) => {
                        return Some((
                            Ok(message),
                            ReadState::Running {
                                output,
                                stderr,
                                docker,
                                exec_id,
                                tool,
                            },
                        ));
                    }
                    Some(Ok(LogOutput::StdErr { message })) => push_tail(&mut stderr, &message),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        return Some((Err(Error::Node(error.to_string())), ReadState::Done))
                    }
                    None => {
                        let code = docker
                            .inspect_exec(&exec_id)
                            .await
                            .ok()
                            .and_then(|inspect| inspect.exit_code)
                            .unwrap_or(-1);
                        if code == 0 {
                            return None;
                        }
                        return Some((Err(exit_error(&tool, code, &stderr)), ReadState::Done));
                    }
                }
            }
        });
        Ok(Box::pin(stream))
    }

    async fn exec_write(
        &self,
        id: &str,
        spec: ExecSpec,
        mut data: ByteStream,
    ) -> Result<ExecOutput, Error> {
        let docker = self.docker()?.clone();
        let tool = check_exec(&spec)?;
        let created = docker
            .create_exec(id, exec_options(&spec, true))
            .await
            .map_err(Self::node)?;
        let started = docker
            .start_exec(&created.id, None)
            .await
            .map_err(Self::node)?;
        let StartExecResults::Attached {
            mut output,
            mut input,
        } = started
        else {
            return Err(Error::Node("exec did not attach".into()));
        };
        let writer = tokio::spawn(async move {
            let mut failure = None;
            while let Some(chunk) = data.next().await {
                match chunk {
                    Ok(bytes) => {
                        if let Err(error) = input.write_all(&bytes).await {
                            failure = Some(Error::Node(format!("stdin closed early: {error}")));
                            break;
                        }
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            input.shutdown().await.ok();
            failure
        });
        let mut text = Vec::new();
        while let Some(item) = output.next().await {
            match item.map_err(Self::node)? {
                LogOutput::StdOut { message } | LogOutput::StdErr { message } => {
                    push_tail(&mut text, &message)
                }
                _ => {}
            }
        }
        let input_error = writer
            .await
            .map_err(|error| Error::Node(format!("stdin task failed: {error}")))?;
        let code = docker
            .inspect_exec(&created.id)
            .await
            .map_err(Self::node)?
            .exit_code
            .unwrap_or(-1);
        if code != 0 {
            return Err(exit_error(&tool, code, &text));
        }
        if let Some(error) = input_error {
            return Err(error);
        }
        Ok(ExecOutput {
            exit_code: code,
            output: String::from_utf8_lossy(&text).into_owned(),
        })
    }

    async fn archive_read(&self, source: &ArchiveSource) -> Result<ByteStream, Error> {
        source.validate()?;
        let helper = self.start_helper(source, true).await?;
        let spec = ExecSpec {
            command: vec![
                "tar".into(),
                "-C".into(),
                "/data".into(),
                "-cf".into(),
                "-".into(),
                ".".into(),
            ],
            ..ExecSpec::default()
        };
        let stream = match self.exec_read(&helper.id, spec).await {
            Ok(stream) => stream,
            Err(error) => {
                drop(helper);
                return Err(error);
            }
        };
        Ok(Box::pin(stream.map(move |item| {
            let _keep = &helper;
            item
        })))
    }

    async fn archive_write(&self, target: &ArchiveSource, data: ByteStream) -> Result<(), Error> {
        target.validate()?;
        let helper = self.start_helper(target, false).await?;
        let spec = ExecSpec {
            command: vec![
                "tar".into(),
                "-C".into(),
                "/data".into(),
                "-xf".into(),
                "-".into(),
            ],
            ..ExecSpec::default()
        };
        let result = self.exec_write(&helper.id, spec, data).await;
        drop(helper);
        result.map(|_| ())
    }

    async fn remove_volume(&self, name: &str) -> Result<(), Error> {
        ArchiveSource::Volume {
            name: name.to_owned(),
        }
        .validate()?;
        self.docker()?
            .remove_volume(name, None::<bollard::query_parameters::RemoveVolumeOptions>)
            .await
            .map_err(Self::node)
    }

    async fn read_file(&self, relative_path: &str) -> Result<Vec<u8>, Error> {
        let path = confined_path(&self.root, relative_path)?;
        tokio::fs::read(&path).await.map_err(Self::node)
    }

    async fn container_stats(&self) -> Result<Vec<rudder_core::ContainerStats>, Error> {
        let docker = self.docker()?;
        let mut filters = HashMap::new();
        filters.insert("label".to_owned(), vec!["rudder.managed=true".to_owned()]);
        let options = bollard::query_parameters::ListContainersOptionsBuilder::new()
            .all(false)
            .filters(&filters)
            .build();
        let summaries = docker
            .list_containers(Some(options))
            .await
            .map_err(Self::node)?;
        let reads = summaries.into_iter().filter_map(|summary| {
            let id = summary.id?;
            let name = summary
                .names
                .as_ref()
                .and_then(|names| names.first())
                .map(|name| name.trim_start_matches('/').to_owned())
                .unwrap_or_default();
            let labels: Vec<(String, String)> =
                summary.labels.unwrap_or_default().into_iter().collect();
            Some(async move {
                let options = bollard::query_parameters::StatsOptionsBuilder::new()
                    .stream(false)
                    .one_shot(false)
                    .build();
                let stats = docker.stats(&id, Some(options)).next().await?.ok()?;
                let restart_count = docker
                    .inspect_container(&id, None)
                    .await
                    .ok()
                    .and_then(|info| info.restart_count)
                    .unwrap_or(0);
                Some(container_stats_from(id, name, labels, restart_count, stats))
            })
        });
        Ok(futures_util::future::join_all(reads)
            .await
            .into_iter()
            .flatten()
            .collect())
    }

    async fn prune(&self) -> Result<u64, Error> {
        let docker = self.docker()?;
        let mut filters = HashMap::new();
        filters.insert("dangling".to_owned(), vec!["true".to_owned()]);
        let images = docker
            .prune_images(Some(
                bollard::query_parameters::PruneImagesOptionsBuilder::new()
                    .filters(&filters)
                    .build(),
            ))
            .await
            .map_err(Self::node)?;
        let build = docker
            .prune_build(None::<bollard::query_parameters::PruneBuildOptions>)
            .await
            .map_err(Self::node)?;
        let reclaimed = images.space_reclaimed.unwrap_or(0) + build.space_reclaimed.unwrap_or(0);
        Ok(u64::try_from(reclaimed).unwrap_or(0))
    }

    async fn host_stats(&self) -> Result<rudder_core::HostStats, Error> {
        let proc_root = self.host_proc.clone();
        let disk = self.root.clone();
        let previous = *self
            .cpu_previous
            .lock()
            .map_err(|_| Error::Node("host stats lock".into()))?;
        let (stats, next) = tokio::task::spawn_blocking(move || {
            let mut previous = previous;
            let disk = if disk.exists() {
                disk
            } else {
                PathBuf::from("/")
            };
            rudder_core::read_host_stats(&proc_root, &disk, &mut previous).map(|s| (s, previous))
        })
        .await
        .map_err(Self::node)??;
        if let Ok(mut slot) = self.cpu_previous.lock() {
            *slot = next;
        }
        Ok(stats)
    }

    async fn list_containers(&self) -> Result<Vec<ContainerInfo>, Error> {
        let options = bollard::query_parameters::ListContainersOptionsBuilder::new()
            .all(false)
            .build();
        let summaries = self
            .docker()?
            .list_containers(Some(options))
            .await
            .map_err(Self::node)?;
        Ok(summaries
            .into_iter()
            .filter_map(|summary| {
                let name = summary
                    .names
                    .as_ref()
                    .and_then(|names| names.first())
                    .map(|name| name.trim_start_matches('/').to_owned())?;
                Some(ContainerInfo {
                    id: summary.id?,
                    name,
                    image: summary.image.unwrap_or_default(),
                    labels: summary.labels.unwrap_or_default().into_iter().collect(),
                })
            })
            .collect())
    }

    async fn follow_logs(&self, id: &str, since_unix: i64) -> Result<LogLineStream, Error> {
        let options = LogsOptionsBuilder::new()
            .follow(true)
            .stdout(true)
            .stderr(true)
            .timestamps(true)
            .since(i32::try_from(since_unix).unwrap_or(0))
            .build();
        let frames = self.docker()?.logs(id, Some(options));
        let lines = frames.flat_map(|frame| {
            let items: Vec<Result<LogLine, Error>> = match frame {
                Ok(output) => {
                    let (stream, message) = match output {
                        LogOutput::StdErr { message } => (LogStreamKind::Stderr, message),
                        LogOutput::StdOut { message } | LogOutput::Console { message } => {
                            (LogStreamKind::Stdout, message)
                        }
                        LogOutput::StdIn { .. } => return futures_util::stream::iter(Vec::new()),
                    };
                    String::from_utf8_lossy(&message)
                        .split('\n')
                        .filter(|line| !line.trim().is_empty())
                        .map(|line| {
                            let (timestamp, text) = match line.split_once(' ') {
                                Some((ts, rest))
                                    if rudder_core::parse_docker_timestamp(ts).is_some() =>
                                {
                                    (Some(ts.to_owned()), rest.to_owned())
                                }
                                _ => (None, line.to_owned()),
                            };
                            Ok(LogLine {
                                stream,
                                timestamp,
                                text,
                            })
                        })
                        .collect()
                }
                Err(error) => vec![Err(Self::node(error))],
            };
            futures_util::stream::iter(items)
        });
        Ok(Box::pin(lines))
    }

    async fn terminal(
        &self,
        id: &str,
        cols: u16,
        rows: u16,
        mut input: TerminalInputStream,
    ) -> Result<ByteStream, Error> {
        let docker = self.docker()?.clone();
        let exec = docker
            .create_exec(
                id,
                CreateExecOptions {
                    attach_stdin: Some(true),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    tty: Some(true),
                    env: Some(vec!["TERM=xterm-256color".to_owned()]),
                    cmd: Some(
                        TERMINAL_SHELL
                            .iter()
                            .map(|part| (*part).to_owned())
                            .collect(),
                    ),
                    ..Default::default()
                },
            )
            .await
            .map_err(Self::node)?;
        let started = docker
            .start_exec(
                &exec.id,
                Some(bollard::exec::StartExecOptions {
                    detach: false,
                    tty: true,
                    output_capacity: None,
                }),
            )
            .await
            .map_err(Self::node)?;
        let StartExecResults::Attached {
            output,
            input: mut stdin,
        } = started
        else {
            return Err(Error::Node("terminal exec did not attach".into()));
        };
        let resize = |docker: Docker, exec_id: String, cols: u16, rows: u16| async move {
            let options = bollard::exec::ResizeExecOptions {
                height: rows.max(1),
                width: cols.max(1),
            };
            if let Err(error) = docker.resize_exec(&exec_id, options).await {
                tracing::debug!(%error, "terminal resize failed");
            }
        };
        resize(docker.clone(), exec.id.clone(), cols, rows).await;
        let exec_id = exec.id.clone();
        tokio::spawn(async move {
            while let Some(event) = input.next().await {
                match event {
                    TerminalInput::Data(bytes) => {
                        if stdin.write_all(&bytes).await.is_err() {
                            break;
                        }
                        let _ = stdin.flush().await;
                    }
                    TerminalInput::Resize { cols, rows } => {
                        resize(docker.clone(), exec_id.clone(), cols, rows).await;
                    }
                }
            }
            let _ = stdin.shutdown().await;
        });
        Ok(Box::pin(output.filter_map(|frame| async move {
            match frame {
                Ok(LogOutput::StdIn { .. }) => None,
                Ok(
                    LogOutput::StdOut { message }
                    | LogOutput::StdErr { message }
                    | LogOutput::Console { message },
                ) => Some(Ok(message)),
                Err(error) => Some(Err(Self::node(error))),
            }
        })))
    }

    async fn cache_purge(
        &self,
        request: &rudder_core::CachePurge,
    ) -> Result<rudder_core::CachePurgeResult, Error> {
        let bytes = self
            .cache_request(reqwest::Method::POST, "/purge", Some(request))
            .await?;
        serde_json::from_slice(&bytes).map_err(Self::node)
    }

    async fn cache_stats(&self) -> Result<rudder_core::CacheStats, Error> {
        let bytes = self
            .cache_request(reqwest::Method::GET, "/stats", None)
            .await?;
        serde_json::from_slice(&bytes).map_err(Self::node)
    }
}

enum ReadState {
    Running {
        output: std::pin::Pin<
            Box<dyn futures_util::Stream<Item = Result<LogOutput, bollard::errors::Error>> + Send>,
        >,
        stderr: Vec<u8>,
        docker: Docker,
        exec_id: String,
        tool: String,
    },
    Done,
}

/// Removes a helper container when the last stream handle is dropped.
struct Helper {
    docker: Docker,
    id: String,
}

impl Drop for Helper {
    fn drop(&mut self) {
        let docker = self.docker.clone();
        let id = std::mem::take(&mut self.id);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let options = RemoveContainerOptionsBuilder::default().force(true).build();
                if let Err(error) = docker.remove_container(&id, Some(options)).await {
                    tracing::warn!(%error, container = %id, "helper container was not removed");
                }
            });
        }
    }
}

const TAIL_LIMIT: usize = 8 * 1024;

fn push_tail(buffer: &mut Vec<u8>, bytes: &[u8]) {
    buffer.extend_from_slice(bytes);
    if buffer.len() > TAIL_LIMIT {
        let cut = buffer.len() - TAIL_LIMIT;
        buffer.drain(..cut);
    }
}

fn exit_error(tool: &str, code: i64, stderr: &[u8]) -> Error {
    let detail = String::from_utf8_lossy(stderr).trim().to_owned();
    if code == 126 || code == 127 {
        return Error::Node(format!(
            "{tool} is not available in the container (exit {code}): {detail}"
        ));
    }
    Error::Node(format!("{tool} exited with code {code}: {detail}"))
}

fn check_exec(spec: &ExecSpec) -> Result<String, Error> {
    let Some(first) = spec.command.first() else {
        return Err(Error::Validation("exec command is empty".into()));
    };
    if spec.command.iter().any(|part| part.contains('\0')) {
        return Err(Error::Validation(
            "exec arguments cannot contain NUL".into(),
        ));
    }
    for (key, value) in &spec.env {
        validate_env_key(key)?;
        if value.contains('\0') {
            return Err(Error::Validation("exec env cannot contain NUL".into()));
        }
    }
    Ok(first.clone())
}

fn exec_options(spec: &ExecSpec, stdin: bool) -> CreateExecOptions<String> {
    CreateExecOptions {
        cmd: Some(spec.command.clone()),
        env: if spec.env.is_empty() {
            None
        } else {
            Some(
                spec.env
                    .iter()
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect(),
            )
        },
        user: spec.user.clone(),
        attach_stdin: Some(stdin),
        attach_stdout: Some(true),
        attach_stderr: Some(true),
        ..CreateExecOptions::default()
    }
}

impl LocalDriver {
    async fn ensure_network(&self, name: &str) -> Result<(), Error> {
        let docker = self.docker()?;
        if docker.inspect_network(name, None).await.is_ok() {
            return Ok(());
        }
        let created = docker
            .create_network(bollard::models::NetworkCreateRequest {
                name: name.to_owned(),
                driver: Some("bridge".into()),
                ..Default::default()
            })
            .await;
        match created {
            Ok(_) => Ok(()),
            Err(error) if docker.inspect_network(name, None).await.is_ok() => {
                tracing::debug!(%error, "network was created concurrently");
                Ok(())
            }
            Err(error) => Err(Self::node(error)),
        }
    }

    async fn ensure_image(&self, image: &str) -> Result<(), Error> {
        if self.docker()?.inspect_image(image).await.is_ok() {
            return Ok(());
        }
        self.pull_image(image, None).await
    }

    /// Start a throwaway `alpine` container with `source` mounted at `/data`.
    async fn start_helper(&self, source: &ArchiveSource, read_only: bool) -> Result<Helper, Error> {
        self.ensure_image(HELPER_IMAGE).await?;
        let mount = match source {
            ArchiveSource::Volume { name } => name.clone(),
            ArchiveSource::HostPath { path } => path.clone(),
        };
        let name = format!(
            "rudder-helper-{}",
            hex::encode(rudder_core::random_bytes::<6>())
        );
        let spec = ContainerSpec {
            name,
            image: HELPER_IMAGE.into(),
            env: Vec::new(),
            command: vec!["sleep".into(), "3600".into()],
            ports: Vec::new(),
            volumes: vec![VolumeMount {
                source: mount,
                target: "/data".into(),
                read_only,
            }],
            memory_bytes: None,
            nano_cpus: None,
            restart: "no".into(),
            network: Some("none".into()),
            labels: vec![("rudder.helper".into(), "true".into())],
        };
        let id = self.create_container(&spec).await?;
        let helper = Helper {
            docker: self.docker()?.clone(),
            id: id.clone(),
        };
        self.start_container(&id).await?;
        Ok(helper)
    }

    async fn inspect(&self, id: &str) -> Result<bollard::models::ContainerInspectResponse, Error> {
        self.docker()?
            .inspect_container(id, None)
            .await
            .map_err(Self::node)
    }
}

fn container_ip(inspected: &bollard::models::ContainerInspectResponse) -> Option<String> {
    let networks = inspected.network_settings.as_ref()?.networks.as_ref()?;
    if let Some(endpoint) = networks.get(PROXY_NETWORK) {
        if let Some(ip) = endpoint.ip_address.as_ref().filter(|ip| !ip.is_empty()) {
            return Some(ip.clone());
        }
    }
    networks
        .values()
        .find_map(|endpoint| endpoint.ip_address.clone().filter(|ip| !ip.is_empty()))
}

fn http_status_ok(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(bytes);
    let mut parts = text.split_whitespace();
    let _version = parts.next();
    matches!(
        parts.next().and_then(|code| code.chars().next()),
        Some('2' | '3')
    )
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

fn container_stats_from(
    id: String,
    name: String,
    labels: Vec<(String, String)>,
    restart_count: i64,
    stats: bollard::models::ContainerStatsResponse,
) -> rudder_core::ContainerStats {
    let cpu = stats.cpu_stats.as_ref();
    let precpu = stats.precpu_stats.as_ref();
    let total = |c: Option<&bollard::models::ContainerCpuStats>| {
        c.and_then(|c| c.cpu_usage.as_ref())
            .and_then(|u| u.total_usage)
            .unwrap_or(0)
    };
    let system = |c: Option<&bollard::models::ContainerCpuStats>| {
        c.and_then(|c| c.system_cpu_usage).unwrap_or(0)
    };
    let memory = stats.memory_stats.as_ref();
    // Match `docker stats`: page cache that can be reclaimed is not counted.
    let cache = memory
        .and_then(|m| m.stats.as_ref())
        .and_then(|s| {
            s.get("inactive_file")
                .or_else(|| s.get("total_inactive_file"))
        })
        .copied()
        .unwrap_or(0);
    let usage = memory
        .and_then(|m| m.usage)
        .unwrap_or(0)
        .saturating_sub(cache);
    let (rx, tx) = stats
        .networks
        .as_ref()
        .map(|nets| {
            nets.values().fold((0u64, 0u64), |(rx, tx), n| {
                (rx + n.rx_bytes.unwrap_or(0), tx + n.tx_bytes.unwrap_or(0))
            })
        })
        .unwrap_or((0, 0));
    let (read, write) = stats
        .blkio_stats
        .as_ref()
        .and_then(|b| b.io_service_bytes_recursive.as_ref())
        .map(|entries| {
            entries.iter().fold((0u64, 0u64), |(r, w), e| {
                match e.op.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    Some("read") => (r + e.value.unwrap_or(0), w),
                    Some("write") => (r, w + e.value.unwrap_or(0)),
                    _ => (r, w),
                }
            })
        })
        .unwrap_or((0, 0));
    rudder_core::ContainerStats {
        id,
        name,
        cpu_percent: rudder_core::container_cpu_percent(
            total(cpu),
            total(precpu),
            system(cpu),
            system(precpu),
        ),
        memory_bytes: usage,
        memory_limit_bytes: memory.and_then(|m| m.limit).unwrap_or(0),
        net_rx_bytes: rx,
        net_tx_bytes: tx,
        block_read_bytes: read,
        block_write_bytes: write,
        restart_count,
        labels,
    }
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
