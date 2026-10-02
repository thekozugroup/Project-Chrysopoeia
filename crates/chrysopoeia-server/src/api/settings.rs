//! `/api/settings`.

use axum::Json;
use axum::extract::State;
use chrysopoeia_core::{FolderStatus, Settings};
use serde_json::Value;

use super::extract::ApiJson;
use crate::error::ApiResult;
use crate::services::settings;
use crate::state::AppState;

/// `GET /api/settings`
pub async fn get(State(state): State<AppState>) -> Json<Settings> {
    Json(state.settings())
}

/// `PATCH /api/settings`
pub async fn update(
    State(state): State<AppState>,
    ApiJson(patch): ApiJson<Value>,
) -> ApiResult<Json<Settings>> {
    Ok(Json(settings::patch(&state, patch).await?))
}

/// `GET /api/settings/folders`: the output and work folders in use, and
/// whether the drives and shares they sit on are connected as they were.
pub async fn folders(State(state): State<AppState>) -> Json<Vec<FolderStatus>> {
    Json(settings::folder_statuses(&state).await)
}

/// `POST /api/settings/relearn-mounts`: take the drive mounted now where
/// the output or work folder's drive was (another one put there on
/// purpose) as the usual one.
pub async fn relearn_mounts(State(state): State<AppState>) -> ApiResult<Json<Vec<FolderStatus>>> {
    Ok(Json(settings::relearn_folders(&state).await?))
}
