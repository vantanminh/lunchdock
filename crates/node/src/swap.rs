//! Recreate a container from a newer image and put the old one back on failure.
//!
//! The old container is stopped and renamed to `<name>-previous` rather than
//! removed, so a rollback is a rename and a start. Settings are copied from the
//! old container, except values the old image supplied (env, command, labels),
//! which the new image's defaults replace.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, EndpointSettings, ImageConfig,
    NetworkConnectRequest, NetworkingConfig,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, RenameContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use rudder_core::{
    validate_image_ref, validate_name, Error, NodeDriver, ReadyProbe, ReplaceContainer,
    ReplaceReport,
};

use crate::LocalDriver;

const POLL: Duration = Duration::from_secs(2);
/// A `Running` probe passes once the container has stayed up this long.
const RUNNING_GRACE: Duration = Duration::from_secs(5);
const STOP_TIMEOUT: i32 = 30;
const FAILURE_LOG_LINES: u32 = 60;

/// A swap in progress: `name` runs the new image, `previous` is the stopped original.
#[derive(Debug, Clone)]
pub struct Swap {
    pub name: String,
    pub previous: String,
    pub previous_image: String,
    pub image: String,
}

impl LocalDriver {
    /// Stop `name`, keep it as `<name>-previous`, and start a copy on `image`.
    pub async fn swap_container(&self, name: &str, image: &str) -> Result<Swap, Error> {
        validate_name(name)?;
        validate_image_ref(image)?;
        let docker = self.docker()?;
        let previous = format!("{name}-previous");
        let current = match docker.inspect_container(name, None).await {
            Ok(current) => current,
            Err(error) => {
                if docker.inspect_container(&previous, None).await.is_ok() {
                    return Err(Error::Node(format!(
                        "{name} is missing but {previous} exists, so an earlier update did not finish; rename {previous} back to {name} and start it"
                    )));
                }
                return Err(Self::node(error));
            }
        };
        if docker.inspect_container(&previous, None).await.is_ok() {
            self.remove_named(&previous).await?;
        }
        let previous_image = current
            .config
            .as_ref()
            .and_then(|config| config.image.clone())
            .unwrap_or_default();
        let defaults = match current.image.as_deref() {
            Some(id) => docker
                .inspect_image(id)
                .await
                .ok()
                .and_then(|image| image.config),
            None => None,
        };
        let (body, extra_networks) = clone_body(&current, image, defaults.as_ref());

        let stop = StopContainerOptionsBuilder::default()
            .t(STOP_TIMEOUT)
            .build();
        docker
            .stop_container(name, Some(stop))
            .await
            .map_err(Self::node)?;
        self.rename(name, &previous).await?;
        let swap = Swap {
            name: name.to_owned(),
            previous,
            previous_image,
            image: image.to_owned(),
        };
        if let Err(error) = self.create_copy(name, body, extra_networks).await {
            let _ = self.remove_named(name).await;
            self.restore_previous(&swap).await?;
            return Err(error);
        }
        if let Err(error) = docker.start_container(name, None).await {
            let _ = self.remove_named(name).await;
            self.restore_previous(&swap).await?;
            return Err(Error::Node(format!("start {name} on {image}: {error}")));
        }
        Ok(swap)
    }

    /// Wait until `name` passes `probe`, or fail with the reason.
    pub async fn wait_ready(
        &self,
        name: &str,
        probe: &ReadyProbe,
        timeout: Duration,
    ) -> Result<(), String> {
        let started = Instant::now();
        let mut last = String::from("not checked yet");
        loop {
            let state = self
                .docker()
                .map_err(|e| e.to_string())?
                .inspect_container(name, None)
                .await
                .map_err(|e| e.to_string())?;
            let running = state
                .state
                .as_ref()
                .and_then(|s| s.running)
                .unwrap_or(false);
            let restarts = state.restart_count.unwrap_or(0);
            if !running || restarts > 0 {
                let code = state.state.as_ref().and_then(|s| s.exit_code).unwrap_or(0);
                if !running && started.elapsed() > Duration::from_secs(1) {
                    return Err(format!("{name} exited with code {code}"));
                }
                if restarts > 0 {
                    return Err(format!("{name} restarted {restarts} times"));
                }
            } else {
                let passed = match probe {
                    ReadyProbe::Running => started.elapsed() >= RUNNING_GRACE,
                    ReadyProbe::Http { port, path } => {
                        match self.http_ready(name, *port, path).await {
                            Ok(ok) => {
                                last = format!("GET {path} on port {port} did not answer 2xx");
                                ok
                            }
                            Err(error) => {
                                last = error.to_string();
                                false
                            }
                        }
                    }
                    ReadyProbe::Exec { command, expect } => {
                        match self.exec(name, command.clone()).await {
                            Ok(output) => {
                                let matched = expect
                                    .as_deref()
                                    .is_none_or(|text| output.output.contains(text));
                                last = format!(
                                    "probe exited {}: {}",
                                    output.exit_code,
                                    output.output.trim().chars().take(300).collect::<String>()
                                );
                                output.exit_code == 0 && matched
                            }
                            Err(error) => {
                                last = error.to_string();
                                false
                            }
                        }
                    }
                };
                if passed {
                    return Ok(());
                }
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "{name} was not ready after {}s: {last}",
                    timeout.as_secs()
                ));
            }
            tokio::time::sleep(POLL).await;
        }
    }

    /// Keep the new container and discard the previous one.
    pub async fn finish_swap(&self, swap: &Swap) -> Result<(), Error> {
        self.remove_named(&swap.previous).await
    }

    /// Remove the new container and start the previous one under its name.
    /// Returns the new container's last output.
    pub async fn revert_swap(&self, swap: &Swap) -> Result<String, Error> {
        let logs = self
            .container_logs(&swap.name, FAILURE_LOG_LINES)
            .await
            .unwrap_or_default();
        self.remove_named(&swap.name).await?;
        self.restore_previous(swap).await?;
        Ok(logs)
    }

    /// Start a one-off container with `source`'s mounts, env, and networks but a
    /// different image and command. Used for the panel's updater.
    pub async fn run_clone(
        &self,
        source: &str,
        name: &str,
        image: &str,
        command: Vec<String>,
    ) -> Result<String, Error> {
        validate_name(name)?;
        validate_image_ref(image)?;
        let docker = self.docker()?;
        let current = docker
            .inspect_container(source, None)
            .await
            .map_err(Self::node)?;
        let defaults = match current.image.as_deref() {
            Some(id) => docker
                .inspect_image(id)
                .await
                .ok()
                .and_then(|image| image.config),
            None => None,
        };
        let (mut body, extra) = clone_body(&current, image, defaults.as_ref());
        body.cmd = Some(command);
        body.entrypoint = None;
        body.labels = None;
        body.healthcheck = None;
        body.exposed_ports = None;
        if let Some(host) = body.host_config.as_mut() {
            host.port_bindings = None;
            host.restart_policy = None;
        }
        if docker.inspect_container(name, None).await.is_ok() {
            self.remove_named(name).await?;
        }
        let id = self.create_copy(name, body, extra).await?;
        docker
            .start_container(name, None)
            .await
            .map_err(Self::node)?;
        Ok(id)
    }

    async fn create_copy(
        &self,
        name: &str,
        body: ContainerCreateBody,
        extra_networks: Vec<(String, EndpointSettings)>,
    ) -> Result<String, Error> {
        let docker = self.docker()?;
        let options = CreateContainerOptionsBuilder::default().name(name).build();
        let created = docker
            .create_container(Some(options), body)
            .await
            .map_err(|error| Error::Node(format!("create {name}: {error}")))?;
        for (network, endpoint) in extra_networks {
            docker
                .connect_network(
                    &network,
                    NetworkConnectRequest {
                        container: created.id.clone(),
                        endpoint_config: Some(endpoint),
                    },
                )
                .await
                .map_err(|error| Error::Node(format!("connect {name} to {network}: {error}")))?;
        }
        Ok(created.id)
    }

    async fn restore_previous(&self, swap: &Swap) -> Result<(), Error> {
        self.rename(&swap.previous, &swap.name).await?;
        self.docker()?
            .start_container(&swap.name, None)
            .await
            .map_err(Self::node)
    }

    async fn rename(&self, from: &str, to: &str) -> Result<(), Error> {
        let options = RenameContainerOptionsBuilder::default().name(to).build();
        self.docker()?
            .rename_container(from, options)
            .await
            .map_err(|error| Error::Node(format!("rename {from} to {to}: {error}")))
    }

    async fn remove_named(&self, name: &str) -> Result<(), Error> {
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        match self.docker()?.remove_container(name, Some(options)).await {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(error) => Err(Error::Node(format!("remove {name}: {error}"))),
        }
    }

    pub(crate) async fn replace(&self, request: &ReplaceContainer) -> Result<ReplaceReport, Error> {
        let timeout = Duration::from_secs(request.timeout_secs.clamp(10, 1800));
        let swap = self.swap_container(&request.name, &request.image).await?;
        let mut log = format!(
            "started {} on {} (was {})\n",
            swap.name, swap.image, swap.previous_image
        );
        match self.wait_ready(&swap.name, &request.probe, timeout).await {
            Ok(()) => {
                self.finish_swap(&swap).await?;
                log.push_str("ready; removed the previous container\n");
                Ok(ReplaceReport {
                    previous_image: swap.previous_image,
                    image: swap.image,
                    rolled_back: false,
                    log,
                })
            }
            Err(reason) => {
                log.push_str(&format!("{reason}; rolling back\n"));
                let output = self.revert_swap(&swap).await?;
                log.push_str(&format!(
                    "restored {} on {}\n--- last output of the new container ---\n{output}",
                    swap.name, swap.previous_image
                ));
                Ok(ReplaceReport {
                    previous_image: swap.previous_image,
                    image: swap.image,
                    rolled_back: true,
                    log,
                })
            }
        }
    }
}

/// Build a create request equal to `current` but on `image`. Networks other
/// than the primary one are returned separately, to connect before start.
fn clone_body(
    current: &ContainerInspectResponse,
    image: &str,
    defaults: Option<&ImageConfig>,
) -> (ContainerCreateBody, Vec<(String, EndpointSettings)>) {
    let config = current.config.clone().unwrap_or_default();
    let defaults = defaults.cloned().unwrap_or_default();
    let unless_default = |value: Option<Vec<String>>, default: &Option<Vec<String>>| {
        if value == *default {
            None
        } else {
            value
        }
    };
    let env = config.env.map(|env| {
        let inherited = defaults.env.clone().unwrap_or_default();
        env.into_iter()
            .filter(|entry| !inherited.contains(entry))
            .collect::<Vec<_>>()
    });
    let labels = config.labels.map(|labels| {
        let inherited = defaults.labels.clone().unwrap_or_default();
        labels
            .into_iter()
            .filter(|(key, value)| inherited.get(key) != Some(value))
            .collect::<HashMap<_, _>>()
    });
    let id = current.id.clone().unwrap_or_default();
    let short_id: String = id.chars().take(12).collect();
    let hostname = config
        .hostname
        .filter(|name| !name.is_empty() && *name != short_id);
    let host_config = current.host_config.clone();
    let primary = host_config
        .as_ref()
        .and_then(|host| host.network_mode.clone())
        .unwrap_or_default();
    let mut primary_endpoint = HashMap::new();
    let mut extra = Vec::new();
    let shares_namespace =
        primary == "host" || primary == "none" || primary.starts_with("container:");
    if !shares_namespace {
        let networks = current
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.clone())
            .unwrap_or_default();
        let mut names: Vec<_> = networks.keys().cloned().collect();
        names.sort();
        for network in names {
            let endpoint = &networks[&network];
            let aliases = endpoint.aliases.clone().map(|aliases| {
                aliases
                    .into_iter()
                    .filter(|alias| *alias != short_id && *alias != id)
                    .collect::<Vec<_>>()
            });
            let settings = EndpointSettings {
                aliases: aliases.filter(|a| !a.is_empty()),
                ipam_config: endpoint.ipam_config.clone(),
                links: endpoint.links.clone(),
                driver_opts: endpoint.driver_opts.clone(),
                ..EndpointSettings::default()
            };
            let is_primary = network == primary
                || (primary == "default" && network == "bridge")
                || (primary.is_empty() && primary_endpoint.is_empty());
            if is_primary && primary_endpoint.is_empty() {
                primary_endpoint.insert(network, settings);
            } else {
                extra.push((network, settings));
            }
        }
    }
    let body = ContainerCreateBody {
        hostname,
        domainname: config.domainname.filter(|d| !d.is_empty()),
        user: config
            .user
            .filter(|user| Some(user) != defaults.user.as_ref()),
        attach_stdin: config.attach_stdin,
        attach_stdout: config.attach_stdout,
        attach_stderr: config.attach_stderr,
        tty: config.tty,
        open_stdin: config.open_stdin,
        stdin_once: config.stdin_once,
        env,
        cmd: unless_default(config.cmd, &defaults.cmd),
        entrypoint: unless_default(config.entrypoint, &defaults.entrypoint),
        image: Some(image.to_owned()),
        working_dir: config
            .working_dir
            .filter(|dir| Some(dir) != defaults.working_dir.as_ref()),
        labels,
        stop_signal: config.stop_signal,
        stop_timeout: config.stop_timeout,
        host_config,
        networking_config: if primary_endpoint.is_empty() {
            None
        } else {
            Some(NetworkingConfig {
                endpoints_config: Some(primary_endpoint),
            })
        },
        ..ContainerCreateBody::default()
    };
    (body, extra)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bollard::models::{ContainerConfig, HostConfig, NetworkSettings};

    #[test]
    fn clone_keeps_settings_and_drops_image_defaults() {
        let id = "0123456789abcdef0123".to_owned();
        let current = ContainerInspectResponse {
            id: Some(id.clone()),
            config: Some(ContainerConfig {
                hostname: Some("0123456789ab".into()),
                image: Some("ghcr.io/acme/panel:1".into()),
                env: Some(vec![
                    "PATH=/usr/bin".into(),
                    "DATABASE_URL=postgres://db/rudder".into(),
                ]),
                cmd: Some(vec!["rudder-server".into()]),
                labels: Some(HashMap::from([
                    ("org.opencontainers.image.version".into(), "1".into()),
                    ("rudder.role".into(), "panel".into()),
                ])),
                ..ContainerConfig::default()
            }),
            host_config: Some(HostConfig {
                network_mode: Some("rudder-internal".into()),
                binds: Some(vec!["/var/run/docker.sock:/var/run/docker.sock".into()]),
                ..HostConfig::default()
            }),
            network_settings: Some(NetworkSettings {
                networks: Some(HashMap::from([
                    (
                        "rudder-internal".into(),
                        EndpointSettings {
                            aliases: Some(vec!["0123456789ab".into(), "panel".into()]),
                            ip_address: Some("172.18.0.5".into()),
                            ..EndpointSettings::default()
                        },
                    ),
                    ("rudder-proxy".into(), EndpointSettings::default()),
                ])),
                ..NetworkSettings::default()
            }),
            ..ContainerInspectResponse::default()
        };
        let defaults = ImageConfig {
            env: Some(vec!["PATH=/usr/bin".into()]),
            cmd: Some(vec!["rudder-server".into()]),
            labels: Some(HashMap::from([(
                "org.opencontainers.image.version".into(),
                "1".into(),
            )])),
            ..ImageConfig::default()
        };
        let (body, extra) = clone_body(&current, "ghcr.io/acme/panel@sha256:abc", Some(&defaults));
        assert_eq!(body.image.as_deref(), Some("ghcr.io/acme/panel@sha256:abc"));
        assert_eq!(
            body.env,
            Some(vec!["DATABASE_URL=postgres://db/rudder".into()])
        );
        assert_eq!(body.cmd, None);
        assert_eq!(body.hostname, None);
        let labels = body.labels.unwrap();
        assert_eq!(labels.len(), 1);
        assert_eq!(labels["rudder.role"], "panel");
        assert_eq!(
            body.host_config.unwrap().binds.unwrap(),
            vec!["/var/run/docker.sock:/var/run/docker.sock".to_owned()]
        );
        let endpoints = body.networking_config.unwrap().endpoints_config.unwrap();
        let primary = &endpoints["rudder-internal"];
        assert_eq!(primary.aliases, Some(vec!["panel".to_owned()]));
        assert_eq!(primary.ip_address, None);
        assert_eq!(extra.len(), 1);
        assert_eq!(extra[0].0, "rudder-proxy");
    }
}
