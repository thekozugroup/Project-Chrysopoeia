//! `/api/hardware`.

use axum::Json;
use axum::extract::State;
use szalinski_core::HardwareInfo;

use crate::services::hardware;
use crate::state::AppState;

/// `GET /api/hardware`: the detected hardware, or a placeholder with a
/// "Checking your hardware…" hint while the first detection runs.
pub async fn get(State(state): State<AppState>) -> Json<HardwareInfo> {
    Json(hardware::current_or_placeholder(&state))
}

/// `POST /api/hardware/detect`: detect again (takes a few seconds).
pub async fn detect(State(state): State<AppState>) -> Json<HardwareInfo> {
    let info = hardware::detect(&state).await;
    Json((*info).clone())
}
