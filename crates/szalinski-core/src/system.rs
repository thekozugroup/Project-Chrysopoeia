//! Facts about the running server that the UI needs to explain settings.

use serde::{Deserialize, Serialize};

/// Returned by `GET /api/system`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub version: String,
    /// Image build label from `SZALINSKI_VERSION` (e.g. `edge-1a2b3c4`),
    /// when it differs from `version`.
    #[serde(default)]
    pub build: Option<String>,
    /// Scratch folder used when `Settings::temp_dir` is unset (`--temp-dir` /
    /// `TEMP_DIR`, e.g. `/temp` in Docker). `None` means next to each file.
    pub default_temp_dir: Option<String>,
    /// Roots the folder picker may show.
    pub browse_roots: Vec<String>,
    /// Folder holding the database.
    pub data_dir: String,
    pub in_container: bool,
}
