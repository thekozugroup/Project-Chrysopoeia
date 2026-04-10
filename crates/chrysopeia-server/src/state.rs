//! Application state shared across request handlers.

use std::sync::Arc;

use chrysopeia_worker::TranscodeEngine;
use sqlx::SqlitePool;
use tokio::sync::RwLock;

/// Hardware info for the API (serializable).
#[derive(Debug, Clone, serde::Serialize)]
pub struct HardwareApiInfo {
    pub gpu_name: Option<String>,
    pub gpu_vendor: Option<String>,
    pub formats: Vec<serde_json::Value>,
    pub cpu_cores: usize,
    pub ram_gb: u64,
}

/// Shared application state, passed to all Axum handlers.
#[derive(Clone)]
pub struct AppState {
    /// SQLite connection pool.
    pub db: SqlitePool,
    /// Transcode engine for job processing.
    pub engine: Arc<TranscodeEngine>,
    /// Application configuration as JSON (mutable at runtime).
    pub config: Arc<RwLock<serde_json::Value>>,
    /// Hardware info for API responses.
    pub hardware_info: Arc<HardwareApiInfo>,
    /// Whether processing is active.
    pub processing: Arc<RwLock<bool>>,
}

impl AppState {
    /// Create new application state.
    pub fn new(
        db: SqlitePool,
        engine: TranscodeEngine,
        hardware_info: HardwareApiInfo,
    ) -> Self {
        let default_config = serde_json::json!({
            "default_video": "av1",
            "default_audio": "opus",
            "default_container": "mkv",
            "default_crf": 28,
            "default_skip_open": true,
            "concurrent_jobs": 2,
            "auto_scan": true,
            "auto_transcode": true,
        });

        Self {
            db,
            engine: Arc::new(engine),
            config: Arc::new(RwLock::new(default_config)),
            hardware_info: Arc::new(hardware_info),
            processing: Arc::new(RwLock::new(false)),
        }
    }
}
