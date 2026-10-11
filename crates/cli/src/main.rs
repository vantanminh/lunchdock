fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    };
    let ctx = rudder_cli::Context::from_env();
    let mut stdout = std::io::stdout();
    if let Err(error) = runtime.block_on(rudder_cli::run(args, &ctx, &mut stdout)) {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}
