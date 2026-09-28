//! `/api/settings`.

use axum::Json;
use axum::extract::State;
use chrysopoeia_core::Settings;
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
