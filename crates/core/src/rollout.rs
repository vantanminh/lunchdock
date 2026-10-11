//! Zero-downtime replacement of a container.
//!
//! The new container is started and checked before routing changes. A failed
//! health check deletes the new container and leaves the previous one running.

use std::time::{Duration, Instant};

use crate::{ContainerSpec, Error, NodeDriver, RegistryAuth};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthKind {
    None,
    Docker,
    Http,
    Tcp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HealthSpec {
    pub kind: HealthKind,
    pub timeout_secs: u64,
    pub path: String,
    pub port: u16,
}

impl HealthSpec {
    pub fn none(timeout_secs: u64) -> Self {
        Self {
            kind: HealthKind::None,
            timeout_secs,
            path: "/".into(),
            port: 80,
        }
    }
}

/// What to start. Environment values are not included in logs by callers.
pub struct RolloutPlan {
    pub container_name: String,
    pub image: String,
    pub env: Vec<(String, String)>,
    pub command: Vec<String>,
    pub ports: Vec<crate::PortMapping>,
    pub volumes: Vec<crate::VolumeMount>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub restart: String,
    pub network: Option<String>,
    pub labels: Vec<(String, String)>,
    pub auth: Option<RegistryAuth>,
    pub health: HealthSpec,
}

pub struct StartedContainer {
    pub id: String,
    pub name: String,
}

#[derive(Debug)]
pub struct RolloutOutput {
    pub container_id: String,
    pub container_name: String,
    pub retire_error: Option<String>,
}

/// Pull, create, and wait. On failure the new container is removed.
pub async fn start_replacement(
    driver: &dyn NodeDriver,
    plan: &RolloutPlan,
) -> Result<StartedContainer, Error> {
    crate::validate_name(&plan.container_name)?;
    crate::validate_image_ref(&plan.image)?;
    driver.pull_image(&plan.image, plan.auth.clone()).await?;
    let spec = ContainerSpec {
        name: plan.container_name.clone(),
        image: plan.image.clone(),
        env: plan.env.clone(),
        command: plan.command.clone(),
        ports: plan.ports.clone(),
        volumes: plan.volumes.clone(),
        memory_bytes: plan.memory_bytes,
        nano_cpus: plan.nano_cpus,
        restart: plan.restart.clone(),
        network: plan.network.clone(),
        labels: plan.labels.clone(),
    };
    let id = driver.create_container(&spec).await?;
    if let Err(error) = driver.start_container(&id).await {
        let _ = driver.remove_container(&id, true).await;
        return Err(error);
    }
    if let Err(error) = wait_healthy(driver, &id, &plan.health).await {
        let _ = driver.stop_container(&id, 1).await;
        let _ = driver.remove_container(&id, true).await;
        return Err(error);
    }
    Ok(StartedContainer {
        id,
        name: plan.container_name.clone(),
    })
}

pub async fn retire_container(driver: &dyn NodeDriver, id: &str) -> Result<(), Error> {
    driver.stop_container(id, 10).await?;
    driver.remove_container(id, true).await
}

/// Start the replacement, run `switch` (routing), then stop the previous container.
///
/// `switch` runs only after the new container is healthy. If it fails, the new
/// container is removed and the previous one is left in place.
pub async fn rollout<S, Fut>(
    driver: &dyn NodeDriver,
    plan: &RolloutPlan,
    previous_id: Option<&str>,
    switch: S,
) -> Result<RolloutOutput, Error>
where
    S: FnOnce(&StartedContainer) -> Fut,
    Fut: std::future::Future<Output = Result<(), Error>>,
{
    let started = start_replacement(driver, plan).await?;
    if let Err(error) = switch(&started).await {
        let _ = retire_container(driver, &started.id).await;
        return Err(error);
    }
    let mut retire_error = None;
    if let Some(previous) = previous_id {
        if previous != started.id {
            if let Err(error) = retire_container(driver, previous).await {
                retire_error = Some(error.to_string());
            }
        }
    }
    Ok(RolloutOutput {
        container_id: started.id,
        container_name: started.name,
        retire_error,
    })
}

async fn wait_healthy(driver: &dyn NodeDriver, id: &str, health: &HealthSpec) -> Result<(), Error> {
    let timeout = Duration::from_secs(health.timeout_secs);
    let started = Instant::now();
    loop {
        match probe(driver, id, health).await? {
            Probe::Ready => return Ok(()),
            Probe::Failed => {
                return Err(Error::Node("container health check failed".into()));
            }
            Probe::Pending => {}
        }
        if started.elapsed() >= timeout {
            return Err(Error::Node("health check timed out".into()));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

enum Probe {
    Ready,
    Pending,
    Failed,
}

async fn probe(driver: &dyn NodeDriver, id: &str, health: &HealthSpec) -> Result<Probe, Error> {
    match health.kind {
        HealthKind::None => {
            if driver.container_running(id).await? {
                Ok(Probe::Ready)
            } else {
                Ok(Probe::Pending)
            }
        }
        HealthKind::Docker => match driver.docker_health(id).await? {
            crate::HealthState::Healthy => Ok(Probe::Ready),
            crate::HealthState::Unhealthy => Ok(Probe::Failed),
            crate::HealthState::Starting => Ok(Probe::Pending),
            crate::HealthState::None => {
                if driver.container_running(id).await? {
                    Ok(Probe::Ready)
                } else {
                    Ok(Probe::Pending)
                }
            }
        },
        HealthKind::Http => {
            if driver.http_ready(id, health.port, &health.path).await? {
                Ok(Probe::Ready)
            } else {
                Ok(Probe::Pending)
            }
        }
        HealthKind::Tcp => {
            if driver.tcp_ready(id, health.port).await? {
                Ok(Probe::Ready)
            } else {
                Ok(Probe::Pending)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::sync::Mutex;

    struct Fake {
        events: Mutex<Vec<String>>,
        running: bool,
        http: bool,
    }

    impl Fake {
        fn push(&self, event: &str) {
            self.events.lock().expect("events").push(event.to_owned());
        }
    }

    #[async_trait]
    impl NodeDriver for Fake {
        async fn docker_info(&self) -> Result<crate::DockerInfo, Error> {
            Err(Error::Node("unused".into()))
        }
        async fn pull_image(&self, _image: &str, _auth: Option<RegistryAuth>) -> Result<(), Error> {
            self.push("pull");
            Ok(())
        }
        async fn create_container(&self, spec: &ContainerSpec) -> Result<String, Error> {
            self.push("create");
            Ok(format!("id-{}", spec.name))
        }
        async fn start_container(&self, _id: &str) -> Result<(), Error> {
            self.push("start");
            Ok(())
        }
        async fn stop_container(&self, id: &str, _timeout_secs: i64) -> Result<(), Error> {
            self.push(&format!("stop:{id}"));
            Ok(())
        }
        async fn restart_container(&self, _id: &str) -> Result<(), Error> {
            Ok(())
        }
        async fn remove_container(&self, id: &str, _force: bool) -> Result<(), Error> {
            self.push(&format!("remove:{id}"));
            Ok(())
        }
        async fn container_running(&self, _id: &str) -> Result<bool, Error> {
            Ok(self.running)
        }
        async fn container_logs(&self, _id: &str, _tail: u32) -> Result<String, Error> {
            Ok(String::new())
        }
        async fn exec(&self, _id: &str, _command: Vec<String>) -> Result<crate::ExecOutput, Error> {
            Err(Error::Node("unused".into()))
        }
        async fn write_file(&self, _relative_path: &str, _bytes: &[u8]) -> Result<(), Error> {
            self.push("write");
            Ok(())
        }
        async fn compose(
            &self,
            _project: &str,
            _file: &str,
            _action: crate::ComposeAction,
        ) -> Result<String, Error> {
            Ok(String::new())
        }
        async fn docker_health(&self, _id: &str) -> Result<crate::HealthState, Error> {
            Ok(crate::HealthState::Healthy)
        }
        async fn http_ready(&self, _id: &str, _port: u16, _path: &str) -> Result<bool, Error> {
            Ok(self.http)
        }
        async fn tcp_ready(&self, _id: &str, _port: u16) -> Result<bool, Error> {
            Ok(true)
        }
        async fn image_digest(&self, _image: &str) -> Result<String, Error> {
            Ok("sha256:abc".into())
        }
    }

    fn plan(kind: HealthKind) -> RolloutPlan {
        RolloutPlan {
            container_name: "web-2".into(),
            image: "ghcr.io/acme/web:1".into(),
            env: vec![("TOKEN".into(), "secret".into())],
            command: Vec::new(),
            ports: Vec::new(),
            volumes: Vec::new(),
            memory_bytes: None,
            nano_cpus: None,
            restart: "unless-stopped".into(),
            network: Some("rudder-proxy".into()),
            labels: Vec::new(),
            auth: Some(RegistryAuth {
                server: "ghcr.io".into(),
                username: "octocat".into(),
                password: "hidden".into(),
            }),
            health: HealthSpec {
                kind,
                timeout_secs: 0,
                path: "/health".into(),
                port: 8080,
            },
        }
    }

    #[tokio::test]
    async fn healthy_rollout_switches_before_stopping_the_old_container() {
        let driver = Fake {
            events: Mutex::new(Vec::new()),
            running: true,
            http: true,
        };
        let output = rollout(
            &driver,
            &plan(HealthKind::Http),
            Some("id-web-1"),
            |_| async {
                driver.push("switch");
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(output.container_id, "id-web-2");
        assert!(output.retire_error.is_none());
        let events = driver.events.lock().expect("events").clone();
        let switch = events.iter().position(|event| event == "switch").unwrap();
        let stop_old = events
            .iter()
            .position(|event| event == "stop:id-web-1")
            .unwrap();
        assert!(switch < stop_old);
        assert!(!events
            .iter()
            .any(|event| event.contains("secret") || event.contains("hidden")));
    }

    #[tokio::test]
    async fn failed_health_keeps_the_old_container() {
        let driver = Fake {
            events: Mutex::new(Vec::new()),
            running: false,
            http: false,
        };
        let mut switched = false;
        let error = rollout(&driver, &plan(HealthKind::Http), Some("id-web-1"), |_| {
            switched = true;
            async { Ok(()) }
        })
        .await
        .unwrap_err();
        assert_eq!(error, Error::Node("health check timed out".into()));
        assert!(!switched);
        let events = driver.events.lock().expect("events").clone();
        assert!(events.iter().any(|event| event == "remove:id-web-2"));
        assert!(!events.iter().any(|event| event == "stop:id-web-1"));
    }

    #[tokio::test]
    async fn failed_switch_removes_the_new_container() {
        let driver = Fake {
            events: Mutex::new(Vec::new()),
            running: true,
            http: true,
        };
        let error = rollout(
            &driver,
            &plan(HealthKind::None),
            Some("id-web-1"),
            |_| async { Err(Error::Node("routing write failed".into())) },
        )
        .await
        .unwrap_err();
        assert_eq!(error, Error::Node("routing write failed".into()));
        let events = driver.events.lock().expect("events").clone();
        assert!(events.iter().any(|event| event == "remove:id-web-2"));
        assert!(!events.iter().any(|event| event == "stop:id-web-1"));
    }
}
