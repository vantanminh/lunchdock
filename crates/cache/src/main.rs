use std::net::SocketAddr;
use std::path::PathBuf;

use rudder_cache::{read_rules, CacheConfig};

fn main() -> anyhow::Result<()> {
    let mut listen: SocketAddr = "0.0.0.0:8080".parse().expect("default listen");
    let mut admin_listen: SocketAddr = "127.0.0.1:9090".parse().expect("default admin");
    let mut rules = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("rudder-cache {}", rudder_core::VERSION);
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--listen" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--listen needs a value"))?;
                listen = value.parse()?;
            }
            "--admin-listen" => {
                let value = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--admin-listen needs a value"))?;
                admin_listen = value.parse()?;
            }
            "--rules" => {
                rules = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow::anyhow!("--rules needs a value"))?,
                ));
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let Some(rules_path) = rules else {
        print_help();
        anyhow::bail!("missing --rules");
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .try_init()
        .ok();
    let config = CacheConfig {
        listen,
        admin_listen,
        rules_path,
    };
    let rules = read_rules(&config)?;
    tracing::info!(
        listen = %config.listen,
        admin = %config.admin_listen,
        rules_are_array = rules.is_array(),
        rules_are_object = rules.is_object(),
        "cache rules loaded"
    );
    Ok(())
}

fn print_help() {
    println!(
        "rudder-cache {version}\n\nUsage: rudder-cache --rules <rules.json> [--listen 0.0.0.0:8080] [--admin-listen 127.0.0.1:9090]",
        version = rudder_core::VERSION
    );
}
