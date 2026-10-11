use std::path::PathBuf;

use rudder_agent::AgentConfig;

fn main() -> anyhow::Result<()> {
    let mut server = None;
    let mut data_dir = PathBuf::from("/var/lib/rudder-agent");
    let mut join_token = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("rudder-agent {}", rudder_core::VERSION);
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--server" => {
                server = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--server needs a value"))?,
                );
            }
            "--data-dir" => {
                data_dir = PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--data-dir needs a value"))?,
                );
            }
            "--join-token" => {
                join_token = Some(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--join-token needs a value"))?,
                );
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let Some(server) = server else {
        print_help();
        anyhow::bail!("missing --server");
    };
    let config = AgentConfig {
        server,
        data_dir,
        join_token,
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .try_init()
        .ok();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(rudder_agent::run(config))
}

fn print_help() {
    println!(
        "rudder-agent {version}\n\nUsage: rudder-agent --server <https://panel:7443> [--data-dir <path>] [--join-token <token>]",
        version = rudder_core::VERSION
    );
}
