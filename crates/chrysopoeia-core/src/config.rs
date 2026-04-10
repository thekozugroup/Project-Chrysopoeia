//! Application configuration for Chrysopeia.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Top-level application configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// Directories to scan for media files.
    pub library_paths: Vec<PathBuf>,
    /// Default target codec for transcoding.
    pub default_codec: String,
    /// Default target container format.
    pub default_container: String,
    /// Maximum number of concurrent transcode jobs.
    pub max_concurrent_jobs: usize,
    /// Whether to prefer hardware acceleration.
    pub prefer_hw_accel: bool,
    /// Quality presets available to the user.
    pub quality_presets: Vec<QualityPreset>,
    /// Path to the SQLite database.
    pub database_path: PathBuf,
    /// HTTP server listen port.
    pub server_port: u16,
    /// Whether to automatically scan on startup.
    pub auto_scan: bool,
    /// Whether to delete original files after successful transcode.
    pub delete_originals: bool,
}

/// A named quality preset for transcoding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityPreset {
    /// Human-readable name (e.g. "High Quality", "Fast").
    pub name: String,
    /// CRF or quality value (codec-dependent).
    pub quality_value: u32,
    /// Encoding speed preset (e.g. 0-13 for AV1).
    pub speed_preset: u32,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            library_paths: vec![],
            default_codec: "av1".to_string(),
            default_container: "mkv".to_string(),
            max_concurrent_jobs: 2,
            prefer_hw_accel: true,
            quality_presets: vec![
                QualityPreset {
                    name: "High Quality".to_string(),
                    quality_value: 24,
                    speed_preset: 4,
                },
                QualityPreset {
                    name: "Balanced".to_string(),
                    quality_value: 30,
                    speed_preset: 6,
                },
                QualityPreset {
                    name: "Fast".to_string(),
                    quality_value: 35,
                    speed_preset: 10,
                },
            ],
            database_path: PathBuf::from("chrysopoeia.db"),
            server_port: 8080,
            auto_scan: true,
            delete_originals: false,
        }
    }
}
