//! Rudder control-plane process.

fn main() -> anyhow::Result<()> {
    let config = rudder_server::Config::load().inspect_err(|error| {
        eprintln!("configuration error: {error}");
    })?;
    rudder_server::telemetry::init(&config);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(rudder_server::run(config))
}
