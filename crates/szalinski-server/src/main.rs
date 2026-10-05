//! Szalinski entry point.

use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use szalinski_server::Cli;

/// How long the process waits, once the server has stopped, for work still
/// running on blocking threads. A system call stuck on a network share that
/// stopped answering never returns; waiting for it would turn every
/// `docker stop` into a kill.
const EXIT_GRACE: Duration = Duration::from_secs(2);

fn main() -> ExitCode {
    let cli = Cli::parse();
    let config = match cli.resolve() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("szalinski: {e:#}");
            return ExitCode::from(2);
        }
    };
    szalinski_server::init_tracing(&config.log_level);
    szalinski_server::install_panic_hook();
    szalinski_server::http::raise_open_file_limit();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("szalinski: could not start: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(szalinski_server::run(config));
    runtime.shutdown_timeout(EXIT_GRACE);
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("{e:#}");
            ExitCode::FAILURE
        }
    }
}
