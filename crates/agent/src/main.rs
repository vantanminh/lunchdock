use std::path::PathBuf;

use rudder_agent::{AgentConfig, LogWriterFactory};

const DEFAULT_DATA_DIR: &str = "/var/lib/rudder-agent";
const DEFAULT_NODE_DIR: &str = "/var/lib/rudder";

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let command = args.next().unwrap_or_default();
    match command.as_str() {
        "--version" | "-V" | "version" => {
            println!("rudder-agent {}", rudder_core::VERSION);
            Ok(())
        }
        "" | "--help" | "-h" | "help" => {
            print_help();
            Ok(())
        }
        "enroll" => {
            let mut server = None;
            let mut token = None;
            let mut data_dir = PathBuf::from(DEFAULT_DATA_DIR);
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--server" => server = Some(value(&mut args, "--server")?),
                    "--token" => token = Some(value(&mut args, "--token")?),
                    "--data-dir" => data_dir = PathBuf::from(value(&mut args, "--data-dir")?),
                    other => anyhow::bail!("unknown argument {other}"),
                }
            }
            let server = server.ok_or_else(|| anyhow::anyhow!("missing --server"))?;
            let token = token.ok_or_else(|| anyhow::anyhow!("missing --token"))?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let identity = runtime.block_on(rudder_agent::enroll(&server, &token, &data_dir))?;
            println!(
                "enrolled as server {} (identity in {})",
                identity.meta.server_id,
                data_dir.display()
            );
            Ok(())
        }
        "run" => {
            let mut config = AgentConfig {
                data_dir: PathBuf::from(DEFAULT_DATA_DIR),
                node_dir: PathBuf::from(DEFAULT_NODE_DIR),
                docker_socket: None,
                cache_admin_url: std::env::var("RUDDER_CACHE_ADMIN_URL").ok(),
                host_proc: std::env::var_os("RUDDER_HOST_PROC").map(PathBuf::from),
                binary: std::env::var_os("RUDDER_AGENT_BINARY").map(PathBuf::from),
                update_grace: std::time::Duration::from_secs(
                    std::env::var("RUDDER_AGENT_UPDATE_GRACE_SECS")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(120),
                ),
            };
            while let Some(arg) = args.next() {
                match arg.as_str() {
                    "--data-dir" => {
                        config.data_dir = PathBuf::from(value(&mut args, "--data-dir")?)
                    }
                    "--node-dir" => {
                        config.node_dir = PathBuf::from(value(&mut args, "--node-dir")?)
                    }
                    "--docker-socket" => {
                        config.docker_socket =
                            Some(PathBuf::from(value(&mut args, "--docker-socket")?))
                    }
                    "--cache-admin-url" => {
                        config.cache_admin_url = Some(value(&mut args, "--cache-admin-url")?)
                    }
                    other => anyhow::bail!("unknown argument {other}"),
                }
            }
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .json()
                .with_writer(LogWriterFactory)
                .try_init()
                .ok();
            // Two workers keep the idle footprint small; the agent mostly waits on I/O.
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()?;
            runtime.block_on(rudder_agent::run(config))
        }
        other => {
            print_help();
            anyhow::bail!("unknown command {other}")
        }
    }
}

fn value(args: &mut impl Iterator<Item = String>, flag: &str) -> anyhow::Result<String> {
    args.next()
        .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
}

fn print_help() {
    println!(
        "rudder-agent {version}

Usage:
  rudder-agent enroll --server <panel URL> --token <join token> [--data-dir {data}]
  rudder-agent run [--data-dir {data}] [--node-dir {node}] [--docker-socket <path>]
                   [--cache-admin-url <url>]
  rudder-agent --version

`enroll` creates a private key, exchanges the one-time join token for a client
certificate, and saves both in the data directory. `run` connects to the panel
and keeps reconnecting until stopped.

When the panel sends a new agent binary, `run` replaces its own executable
(RUDDER_AGENT_BINARY overrides the path) and exits with code 75 so the service
manager restarts it. If the new binary cannot reach the panel within
RUDDER_AGENT_UPDATE_GRACE_SECS (default 120), the previous binary is restored.",
        version = rudder_core::VERSION,
        data = DEFAULT_DATA_DIR,
        node = DEFAULT_NODE_DIR,
    );
}
