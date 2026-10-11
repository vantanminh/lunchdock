//! Rudder node agent.
//!
//! The agent dials the panel over gRPC with a client certificate, runs the
//! panel's [`NodeDriver`] calls against the local Docker Engine, and ships
//! container logs and metrics. Collection keeps running while the panel is
//! unreachable; see [`telemetry`] for the buffering rules.

mod collect;
mod host;
pub mod identity;
mod session;
pub mod telemetry;

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use rudder_core::NodeDriver;
use rudder_node::LocalDriver;

pub use identity::{enroll, Identity};
pub use telemetry::Buffer;

const CURSOR_FILE: &str = "log-cursors.json";
const STATS_INTERVAL: Duration = Duration::from_secs(10);

/// `rudder-agent run` settings.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// Identity and cursors, `/var/lib/rudder-agent`.
    pub data_dir: PathBuf,
    /// Root for files the panel writes, `/var/lib/rudder`.
    pub node_dir: PathBuf,
    pub docker_socket: Option<PathBuf>,
    pub cache_admin_url: Option<String>,
    pub host_proc: Option<PathBuf>,
}

/// Settings for [`run_with`].
pub struct RunOptions {
    pub identity: Identity,
    pub data_dir: PathBuf,
    pub stats_interval: Duration,
}

/// Run the agent with the local Docker driver until SIGINT or SIGTERM.
pub async fn run(config: AgentConfig) -> anyhow::Result<()> {
    let identity = Identity::load(&config.data_dir).with_context(|| {
        format!(
            "not enrolled; run `rudder-agent enroll --server <panel> --token <token> --data-dir {}`",
            config.data_dir.display()
        )
    })?;
    std::fs::create_dir_all(&config.node_dir)
        .with_context(|| format!("create {}", config.node_dir.display()))?;
    let mut driver = LocalDriver::connect(&config.node_dir, config.docker_socket.as_deref());
    if let Some(proc) = &config.host_proc {
        driver = driver.with_host_proc(proc);
    }
    if let Some(url) = &config.cache_admin_url {
        driver = driver.with_cache_admin(url);
    }
    let buffer = Arc::new(Buffer::with_cursors(collect::load_cursors(
        &config.data_dir.join(CURSOR_FILE),
    )));
    collect::ship_own_logs(buffer.clone());
    tracing::info!(
        version = rudder_core::VERSION,
        server_id = %identity.meta.server_id,
        panel = %identity.meta.grpc_url,
        "rudder-agent starting"
    );
    run_with(
        RunOptions {
            identity,
            data_dir: config.data_dir,
            stats_interval: STATS_INTERVAL,
        },
        Arc::new(driver),
        buffer,
        shutdown_signal(),
    )
    .await;
    tracing::info!("rudder-agent stopped");
    Ok(())
}

/// Run the session and collectors on any driver until `shutdown` resolves.
/// Dropping the returned future also stops every task it started.
pub async fn run_with(
    options: RunOptions,
    driver: Arc<dyn NodeDriver>,
    buffer: Arc<Buffer>,
    shutdown: impl Future<Output = ()>,
) {
    let tasks = Tasks(vec![
        tokio::spawn(collect::stats(
            driver.clone(),
            buffer.clone(),
            options.stats_interval,
        )),
        tokio::spawn(collect::logs(
            driver.clone(),
            buffer.clone(),
            options.data_dir.join(CURSOR_FILE),
        )),
        tokio::spawn(
            Arc::new(session::Session {
                identity: options.identity,
                driver,
                buffer,
            })
            .run(),
        ),
    ]);
    shutdown.await;
    drop(tasks);
}

struct Tasks(Vec<tokio::task::JoinHandle<()>>);

impl Drop for Tasks {
    fn drop(&mut self) {
        for task in &self.0 {
            task.abort();
        }
    }
}

/// `tracing_subscriber` writer that prints to stdout and ships lines to the panel.
#[derive(Clone, Copy, Default)]
pub struct LogWriterFactory;

pub async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
}
