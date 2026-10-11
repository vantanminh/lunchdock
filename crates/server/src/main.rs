//! Rudder control-plane process.
//!
//! `rudder-server` serves the panel. `rudder-server backup` writes an encrypted
//! panel backup and `rudder-server restore` loads one into an empty database
//! before the panel starts.

use std::path::PathBuf;

use anyhow::{bail, Context};

const USAGE: &str = "usage:
  rudder-server                      serve the panel
  rudder-server openapi              print the OpenAPI document
  rudder-server backup --out FILE [--passphrase-file FILE] [--include-history]
  rudder-server restore --file FILE [--passphrase-file FILE] [--master-key-file FILE] [--force]
  rudder-server self-update ...      internal: run by the panel's updater container

backup reads the passphrase from --passphrase-file or RUDDER_BACKUP_PASSPHRASE.
restore reads it from --passphrase-file or RUDDER_RESTORE_PASSPHRASE, restores into
DATABASE_URL, and writes the backup's master key to --master-key-file
(default RUDDER_MASTER_KEY_FILE).";

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("openapi") => {
            println!("{}", rudder_server::openapi_json());
            Ok(())
        }
        Some("backup") => runtime()?.block_on(backup(&args[1..])),
        Some("restore") => runtime()?.block_on(restore(&args[1..])),
        Some("self-update") => runtime()?.block_on(self_update(&args[1..])),
        Some("--help" | "-h" | "help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("--version") => {
            println!("rudder-server {}", rudder_core::VERSION);
            Ok(())
        }
        Some(other) => bail!("unknown command '{other}'\n{USAGE}"),
        None => {
            let config = rudder_server::Config::load().inspect_err(|error| {
                eprintln!("configuration error: {error}");
            })?;
            rudder_server::telemetry::init(&config);
            runtime()?.block_on(rudder_server::run(config))
        }
    }
}

fn runtime() -> anyhow::Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
}

struct Flags {
    values: Vec<(String, Option<String>)>,
}

impl Flags {
    fn parse(args: &[String], with_value: &[&str], switches: &[&str]) -> anyhow::Result<Self> {
        let mut values = Vec::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            if with_value.contains(&arg.as_str()) {
                let value = iter
                    .next()
                    .with_context(|| format!("{arg} needs a value"))?;
                values.push((arg.clone(), Some(value.clone())));
            } else if switches.contains(&arg.as_str()) {
                values.push((arg.clone(), None));
            } else {
                bail!("unknown option '{arg}'\n{USAGE}");
            }
        }
        Ok(Self { values })
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(key, _)| key == name)
            .and_then(|(_, value)| value.as_deref())
    }

    fn switch(&self, name: &str) -> bool {
        self.values.iter().any(|(key, _)| key == name)
    }
}

fn passphrase(flags: &Flags, env: &str) -> anyhow::Result<Option<String>> {
    if let Some(path) = flags.value("--passphrase-file") {
        let text = std::fs::read_to_string(path).with_context(|| format!("read {path}"))?;
        return Ok(Some(text.trim_end_matches(['\r', '\n']).to_owned()));
    }
    Ok(std::env::var(env).ok().filter(|value| !value.is_empty()))
}

async fn backup(args: &[String]) -> anyhow::Result<()> {
    let flags = Flags::parse(
        args,
        &["--out", "--passphrase-file"],
        &["--include-history"],
    )?;
    let out = PathBuf::from(flags.value("--out").context("--out is required")?);
    let passphrase = passphrase(&flags, "RUDDER_BACKUP_PASSPHRASE")?
        .context("set --passphrase-file or RUDDER_BACKUP_PASSPHRASE")?;
    let config = rudder_server::Config::load()?;
    let written = rudder_server::backup_panel(
        &config,
        &passphrase,
        flags.switch("--include-history"),
        &out,
    )
    .await?;
    eprintln!("panel backup written: {written} bytes");
    Ok(())
}

async fn restore(args: &[String]) -> anyhow::Result<()> {
    let flags = Flags::parse(
        args,
        &["--file", "--passphrase-file", "--master-key-file"],
        &["--force"],
    )?;
    let archive = PathBuf::from(flags.value("--file").context("--file is required")?);
    let master_key_out = flags
        .value("--master-key-file")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("RUDDER_MASTER_KEY_FILE").map(PathBuf::from))
        .context("set --master-key-file or RUDDER_MASTER_KEY_FILE")?;
    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL is required")?;
    let pg_restore = std::env::var_os("RUDDER_PG_RESTORE")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("pg_restore"), PathBuf::from);
    let report = rudder_server::restore_panel(rudder_server::RestoreOptions {
        archive,
        passphrase: passphrase(&flags, "RUDDER_RESTORE_PASSPHRASE")?,
        database_url,
        master_key_out: master_key_out.clone(),
        force: flags.switch("--force"),
        pg_restore,
        known_migration: rudder_server::known_migration(),
    })
    .await?;
    eprintln!(
        "restored the panel backup from {} (Rudder {}, schema {}){}; master key written to {}",
        report.manifest.created_at.to_rfc3339(),
        report.manifest.rudder_version,
        report.manifest.migration,
        if report.replaced_existing {
            ", replacing the previous database"
        } else {
            ""
        },
        master_key_out.display()
    );
    Ok(())
}

async fn self_update(args: &[String]) -> anyhow::Result<()> {
    let flags = Flags::parse(
        args,
        &[
            "--container",
            "--image",
            "--history",
            "--backup",
            "--from-version",
            "--to-version",
            "--migration",
        ],
        &[],
    )?;
    let required = |name: &str| {
        flags
            .value(name)
            .map(str::to_owned)
            .with_context(|| format!("{name} is required"))
    };
    let config = rudder_server::Config::load()?;
    rudder_server::telemetry::init(&config);
    let args = rudder_server::UpdaterArgs {
        container: required("--container")?,
        image: required("--image")?,
        history: required("--history")?
            .parse()
            .context("--history is not a UUID")?,
        backup: PathBuf::from(required("--backup")?),
        from_version: required("--from-version")?,
        to_version: required("--to-version")?,
        migration: required("--migration")?
            .parse()
            .context("--migration is not a number")?,
    };
    if rudder_server::run_updater(config, args).await? {
        Ok(())
    } else {
        bail!("the update did not succeed; see the panel's update history")
    }
}
