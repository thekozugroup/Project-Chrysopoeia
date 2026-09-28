//! `/api/system`: facts about the running server the UI uses to explain
//! settings (e.g. which folder "automatic" means for the work folder).

use axum::Json;
use axum::extract::State;
use chrysopoeia_core::SystemInfo;

use crate::services::hardware::in_container;
use crate::state::AppState;

/// `GET /api/system`
pub async fn get(State(state): State<AppState>) -> Json<SystemInfo> {
    let config = &state.config;
    Json(SystemInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        default_temp_dir: config.temp_dir.as_ref().map(|d| d.display().to_string()),
        browse_roots: config
            .browse_roots
            .iter()
            .map(|r| r.display().to_string())
            .collect(),
        data_dir: std::path::absolute(&config.data_dir)
            .unwrap_or_else(|_| config.data_dir.clone())
            .display()
            .to_string(),
        in_container: in_container(),
    })
}
