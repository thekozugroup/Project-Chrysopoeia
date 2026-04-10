//! Application state shared across request handlers.

use std::sync::Arc;

use chrysopeia_core::config::AppConfig;
use chrysopeia_core::models::HardwareCapability;
use chrysopeia_worker::TranscodeEngine;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

/// Shared application state, passed to all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    /// SQLite connection pool.
    pub db: SqlitePool,
    /// Transcode engine for job processing.
    pub engine: Arc<TranscodeEngine>,
    /// Application configuration (mutable at runtime).
    pub config: Arc<RwLock<AppConfig>>,
    /// Detected hardware capabilities.
    pub capabilities: Arc<Vec<HardwareCapability>>,
}

impl AppState {
    /// Create new application state.
    pub fn new(
        db: SqlitePool,
        engine: TranscodeEngine,
        config: AppConfig,
        capabilities: Vec<HardwareCapability>,
    ) -> Self {
        Self {
            db,
            engine: Arc::new(engine),
            config: Arc::new(RwLock::new(config)),
            capabilities: Arc::new(capabilities),
        }
    }
}
