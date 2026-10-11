//! Rudder CLI. API commands are filled in once the REST surface is stable.

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None | Some("--help") | Some("-h") => {
            print_help();
            Ok(())
        }
        Some("--version") | Some("-V") => {
            println!("rudder {}", rudder_core::VERSION);
            Ok(())
        }
        Some("login" | "deploy" | "logs" | "cache") => {
            print_help();
            anyhow::bail!(
                "command is registered; pass --api and a token once the client is configured"
            )
        }
        Some(other) => anyhow::bail!("unknown command {other}"),
    }
}

fn print_help() {
    println!(
        "rudder {version}\n\nUsage:\n  rudder login\n  rudder deploy\n  rudder logs\n  rudder cache purge\n",
        version = rudder_core::VERSION
    );
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_constant_matches_the_product() {
        assert_eq!(rudder_core::PRODUCT, "rudder");
        assert!(!rudder_core::VERSION.is_empty());
    }
}
