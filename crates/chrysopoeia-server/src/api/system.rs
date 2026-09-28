//! `/api/system`: facts about the running server the UI uses to explain
//! settings (e.g. which folder "automatic" means for the work folder).

use axum::Json;
use axum::extract::State;
use chrysopoeia_core::SystemInfo;

use crate::services::hardware::in_container;
use crate::state::AppState;

/// The image build label from `CHRYSOPOEIA_VERSION` (e.g. `edge-1a2b3c4`),
/// when it is set and differs from the version.
pub fn build_label() -> Option<String> {
    build_label_from(std::env::var("CHRYSOPOEIA_VERSION").ok())
}

fn build_label_from(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != env!("CARGO_PKG_VERSION"))
}

/// `GET /api/system`
pub async fn get(State(state): State<AppState>) -> Json<SystemInfo> {
    let config = &state.config;
    Json(SystemInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        build: build_label(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_labels_differ_from_the_version() {
        assert_eq!(
            build_label_from(Some(" edge-1a2b3c4 ".into())).as_deref(),
            Some("edge-1a2b3c4")
        );
        assert_eq!(build_label_from(Some(String::new())), None);
        assert_eq!(build_label_from(None), None);
        assert_eq!(
            build_label_from(Some(env!("CARGO_PKG_VERSION").into())),
            None
        );
    }
}
