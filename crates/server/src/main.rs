//! Rudder control-plane process.

fn main() -> anyhow::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("openapi") {
        println!("{}", rudder_server::openapi_json());
        return Ok(());
    }
    let config = rudder_server::Config::load().inspect_err(|error| {
        eprintln!("configuration error: {error}");
    })?;
    rudder_server::telemetry::init(&config);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(rudder_server::run(config))
}
