//! Chrysopoeia server entry point.

use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Chrysopoeia - media transcoding server.
#[derive(Parser, Debug)]
#[command(name = "chrysopoeia", version, about)]
struct Cli {
    /// Port to listen on.
    #[arg(short, long, default_value_t = 8080)]
    port: u16,

    /// Path to the SQLite database.
    #[arg(short, long, default_value = "chrysopoeia.db")]
    database: PathBuf,

    /// Library directories to scan for media files.
    #[arg(short, long)]
    library: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "chrysopoeia=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();

    // Initialize database
    let db_pool = chrysopeia_server::db::initialize_database(&cli.database).await?;
    tracing::info!("Database initialized at {}", cli.database.display());

    // Detect hardware
    let capabilities = chrysopeia_hwdetect::detect_hardware().await?;
    tracing::info!("Detected {} hardware capabilities", capabilities.len());

    // Detect system info for API
    let cpu_cores = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(1);

    let gpu_name = capabilities.first().map(|c| c.device_name.clone());
    let hw_info = chrysopeia_server::state::HardwareApiInfo {
        gpu_name,
        gpu_vendor: None, // TODO: detect vendor from capabilities
        formats: vec![], // TODO: populate from detected encoders
        cpu_cores,
        ram_gb: 0, // TODO: detect RAM
    };

    // Create broadcast channel for progress events
    let (event_tx, _) = tokio::sync::broadcast::channel(256);

    // Initialize engine
    let max_concurrent = 2;
    let engine = chrysopeia_worker::TranscodeEngine::new(
        capabilities,
        max_concurrent,
        event_tx,
    );

    // Build state
    let state = chrysopeia_server::state::AppState::new(db_pool, engine, hw_info);

    // Build router
    let app = chrysopeia_server::routes::build_router(state);

    // Graceful shutdown
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", cli.port)).await?;
    tracing::info!(
        port = cli.port,
        "Chrysopoeia server listening on http://0.0.0.0:{}",
        cli.port
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    tracing::info!("Server shut down gracefully");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    tracing::info!("Shutdown signal received, stopping...");
}
