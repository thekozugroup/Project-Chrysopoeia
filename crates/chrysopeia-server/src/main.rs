//! Chrysopeia server entry point.

use std::path::PathBuf;

use clap::Parser;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// Chrysopeia - media transcoding server with chat interface.
#[derive(Parser, Debug)]
#[command(name = "chrysopeia", version, about)]
struct Cli {
    /// Port to listen on.
    #[arg(short, long, default_value_t = 8080)]
    port: u16,

    /// Path to the SQLite database.
    #[arg(short, long, default_value = "chrysopeia.db")]
    database: PathBuf,

    /// Library directories to scan for media files.
    #[arg(short, long)]
    library: Vec<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| "chrysopeia=info,tower_http=debug".into()))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let cli = Cli::parse();
    tracing::info!("Starting Chrysopeia server on port {}", cli.port);

    // Initialize database
    let db_pool = chrysopeia_server::db::initialize_database(&cli.database).await?;
    tracing::info!("Database initialized at {}", cli.database.display());

    // Detect hardware capabilities
    let capabilities = chrysopeia_hwdetect::detect_hardware().await?;
    tracing::info!("Detected {} hardware capabilities", capabilities.len());

    // Create progress channel for worker <-> server communication
    let (progress_tx, progress_rx) = tokio::sync::mpsc::channel(256);

    // Initialize transcode engine
    let engine = chrysopeia_worker::TranscodeEngine::new(
        capabilities.clone(),
        2, // max concurrent jobs
        progress_tx,
    );

    // Build application state
    let config = chrysopeia_core::AppConfig::default();
    let state = chrysopeia_server::state::AppState::new(
        db_pool,
        engine,
        config,
        capabilities,
    );

    // Build router
    let app = chrysopeia_server::routes::build_router(state);

    // Start server
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", cli.port)).await?;
    tracing::info!("Listening on http://0.0.0.0:{}", cli.port);
    axum::serve(listener, app).await?;

    Ok(())
}
