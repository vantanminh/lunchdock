use std::net::SocketAddr;
use std::path::PathBuf;

use rudder_cache::{run, CacheConfig};

fn main() -> anyhow::Result<()> {
    let mut listen: SocketAddr = "0.0.0.0:8080".parse().expect("default listen");
    let mut admin_listen: SocketAddr = "127.0.0.1:9090".parse().expect("default admin");
    let mut rules = None;
    let mut token = None;
    let mut memory = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| {
            args.next()
                .ok_or_else(|| anyhow::anyhow!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--version" | "-V" => {
                println!("rudder-cache {}", rudder_core::VERSION);
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--listen" => listen = value("--listen")?.parse()?,
            "--admin-listen" => admin_listen = value("--admin-listen")?.parse()?,
            "--rules" => rules = Some(PathBuf::from(value("--rules")?)),
            "--token-file" => token = Some(PathBuf::from(value("--token-file")?)),
            "--memory-bytes" => memory = Some(value("--memory-bytes")?.parse::<u64>()?),
            other => anyhow::bail!("unknown argument {other}"),
        }
    }
    let Some(rules_path) = rules else {
        print_help();
        anyhow::bail!("missing --rules");
    };
    let token_path = token.unwrap_or_else(|| {
        rules_path
            .parent()
            .map(|dir| dir.join("admin.token"))
            .unwrap_or_else(|| PathBuf::from("admin.token"))
    });
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .try_init()
        .ok();
    run(CacheConfig {
        listen,
        admin_listen,
        rules_path,
        token_path,
        memory_max_bytes: memory,
    })
}

fn print_help() {
    println!(
        "rudder-cache {version}\n\nUsage: rudder-cache --rules <rules.json> [--listen 0.0.0.0:8080] \
         [--admin-listen 127.0.0.1:9090] [--token-file <path>] [--memory-bytes <n>]\n\n\
         The admin token file defaults to admin.token next to the rules file and is created \
         with mode 0600 when missing.",
        version = rudder_core::VERSION
    );
}
