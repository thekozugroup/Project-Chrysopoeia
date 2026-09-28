//! Chrysopoeia server: HTTP API, WebSocket events, library scanning service,
//! job queue, and static hosting of the web UI. See `docs/ARCHITECTURE.md`.
//!
//! The media crates (hardware detection, scanner, worker) are reached through
//! [`toolkit::MediaToolkit`], so the whole server can run against a fake
//! toolkit in tests.

pub mod api;
pub mod app;
pub mod config;
pub mod db;
pub mod error;
pub mod format;
pub mod services;
pub mod state;
pub mod toolkit;
pub mod web;
pub mod ws;

#[cfg(test)]
mod tests;

use std::sync::Arc;

pub use config::{Cli, Config};
pub use state::AppState;
pub use toolkit::{MediaToolkit, RealToolkit, Toolkit};

/// Run the server with the real media tools until SIGINT/SIGTERM.
pub async fn run(config: Config) -> anyhow::Result<()> {
    let toolkit = Toolkit::new(Arc::new(RealToolkit::new(config.ffprobe.clone())));
    app::run(config, toolkit).await
}

/// Route panics through the log (with their location) instead of bare
/// stderr, so they show up in `docker logs` next to everything else.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());
        let location = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        tracing::error!(%location, "internal error (panic): {message}");
    }));
}

/// Set up logging: `RUST_LOG` when set, else `level` (e.g. `info`). Colors
/// only on a terminal (and never with `NO_COLOR`), so `docker logs` stays
/// readable.
pub fn init_tracing(level: &str) {
    use std::io::IsTerminal;
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(format!("{level},sqlx=warn,hyper=warn,tower_http=info")))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_ansi(color)
        .try_init();
}
