//! `/api/activity`.

use axum::Json;
use axum::extract::State;
use chrysopoeia_core::ActivityEntry;
use serde::{Deserialize, Serialize};

use super::extract::ApiQuery;
use crate::db;
use crate::error::ApiResult;
use crate::state::AppState;

/// Query of `GET /api/activity`.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub limit: Option<u32>,
    /// Only entries older than this id.
    pub before: Option<i64>,
}

/// Response of `GET /api/activity`.
#[derive(Debug, Serialize)]
pub struct ActivityList {
    pub items: Vec<ActivityEntry>,
}

/// `GET /api/activity`
pub async fn list(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> ApiResult<Json<ActivityList>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let items = db::activity::list(state.db.pool(), limit, q.before).await?;
    Ok(Json(ActivityList { items }))
}
