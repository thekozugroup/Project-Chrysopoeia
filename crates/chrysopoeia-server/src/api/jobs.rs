//! `/api/jobs`.

use axum::Json;
use axum::extract::State;
use chrysopoeia_core::Job;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::ListResponse;
use super::extract::{ApiJson, ApiPath, ApiQuery, OptionalJson};
use crate::db;
use crate::db::jobs::JobFilter;
use crate::error::{ApiError, ApiResult};
use crate::services::queue::{self, PriorityChange};
use crate::state::AppState;

/// Query of `GET /api/jobs`.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub state: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// `GET /api/jobs`
pub async fn list(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> ApiResult<Json<ListResponse<Job>>> {
    let filter = match q.state.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => JobFilter::All,
        Some(s) => JobFilter::parse(s).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_state",
                "Filter jobs by active, running, queued or history.",
            )
        })?,
    };
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let (items, total) =
        db::jobs::list(state.db.pool(), filter, limit, q.offset.unwrap_or(0)).await?;
    Ok(Json(ListResponse { items, total }))
}

/// `GET /api/jobs/{id}`
pub async fn get(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<Json<Job>> {
    db::jobs::get(state.db.pool(), id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("job_not_found", "There's no job with that id."))
}

/// `POST /api/jobs/{id}/cancel`
pub async fn cancel(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<Json<Job>> {
    Ok(Json(queue::cancel_job(&state, id).await?))
}

/// Body of `POST /api/jobs/{id}/priority`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriorityBody {
    #[serde(default)]
    pub priority: Option<i32>,
    #[serde(default, rename = "move")]
    pub move_to: Option<String>,
}

/// `POST /api/jobs/{id}/priority`
pub async fn priority(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<PriorityBody>,
) -> ApiResult<Json<Job>> {
    let change = match (body.priority, body.move_to.as_deref()) {
        (_, Some("top")) => PriorityChange::Top,
        (_, Some(_)) => {
            return Err(ApiError::bad_request(
                "invalid_request",
                "The only supported move is \"top\".",
            ));
        }
        (Some(p), None) => PriorityChange::Set(p),
        (None, None) => {
            return Err(ApiError::bad_request(
                "invalid_request",
                "Send a priority number or {\"move\": \"top\"}.",
            ));
        }
    };
    Ok(Json(queue::set_priority(&state, id, change).await?))
}

/// Body of `POST /api/jobs/clear`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClearBody {
    #[serde(default)]
    pub state: Option<String>,
}

/// `POST /api/jobs/clear`
pub async fn clear(
    State(state): State<AppState>,
    OptionalJson(body): OptionalJson<ClearBody>,
) -> ApiResult<Json<Value>> {
    match body.state.as_deref() {
        None | Some("history") => {}
        Some(_) => {
            return Err(ApiError::bad_request(
                "invalid_state",
                "Only finished jobs can be cleared (state \"history\").",
            ));
        }
    }
    let affected = queue::clear_history(&state).await?;
    Ok(Json(json!({ "affected": affected })))
}
