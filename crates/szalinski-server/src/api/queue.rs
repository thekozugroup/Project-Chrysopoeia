//! `/api/queue`.

use axum::Json;
use axum::extract::State;
use szalinski_core::QueueState;

use crate::error::ApiResult;
use crate::services::{dispatcher, queue};
use crate::state::AppState;

/// `GET /api/queue`
pub async fn get(State(state): State<AppState>) -> ApiResult<Json<QueueState>> {
    Ok(Json(dispatcher::queue_state(&state).await?))
}

/// `POST /api/queue/pause`
pub async fn pause(State(state): State<AppState>) -> ApiResult<Json<QueueState>> {
    dispatcher::set_paused(&state, true).await?;
    Ok(Json(dispatcher::queue_state(&state).await?))
}

/// `POST /api/queue/resume`
pub async fn resume(State(state): State<AppState>) -> ApiResult<Json<QueueState>> {
    dispatcher::set_paused(&state, false).await?;
    Ok(Json(dispatcher::queue_state(&state).await?))
}

/// `POST /api/queue/stop`
pub async fn stop(State(state): State<AppState>) -> ApiResult<Json<QueueState>> {
    Ok(Json(queue::stop_now(&state).await?))
}
