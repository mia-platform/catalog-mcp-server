use crate::{cli::Cli, configuration::Configuration};

mod cli;
mod configuration;
mod logger;
mod server;
mod signal;
mod spec;
mod tracing;

fn run() -> Result<(), Box<dyn std::error::Error>> {
    logger::try_init(env!("CARGO_BIN_NAME"))?;
    tracing::try_init()?;

    // let span = tracing::info_span!("server_initialization");
    // let _enter = span.enter();

    let cli = Cli::parse_args();
    let configuration = Configuration::from(&cli);

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .inspect_err(|err| tracing::error!(?err, "cannot build async runtime"))?;

    rt.block_on(async {
        signal::register_shutdown_listeners();

        Ok::<_, anyhow::Error>(server::try_init(configuration).await?)
    })?;

    Ok(())
}

fn main() {
    if let Err(err) = run() {
        tracing::error!(?err, "application error");
        std::process::exit(1);
    }
}
