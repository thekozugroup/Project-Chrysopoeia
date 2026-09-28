//! Chrysopoeia entry point.

use std::process::ExitCode;

use chrysopoeia_server::Cli;
use clap::Parser;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match cli.resolve() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("chrysopoeia: {e:#}");
            return ExitCode::from(2);
        }
    };
    chrysopoeia_server::init_tracing(&config.log_level);
    chrysopoeia_server::install_panic_hook();
    match chrysopoeia_server::run(config).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}
