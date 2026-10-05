//! `/api/files`.

use axum::Json;
use axum::extract::State;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use szalinski_core::{FileStatus, Job, MediaFile};
use uuid::Uuid;

use super::ListResponse;
use super::extract::{ApiJson, ApiPath, ApiQuery, OptionalJson};
use crate::db;
use crate::db::files::{FileQuery, SortKey};
use crate::error::{ApiError, ApiResult};
use crate::services::queue::{self, BulkAction, BulkSelection};
use crate::state::AppState;

/// Default and maximum page sizes.
pub const DEFAULT_LIMIT: u32 = 100;
/// Largest page `GET /api/files` returns.
pub const MAX_LIMIT: u32 = 500;

/// Query of `GET /api/files`.
#[derive(Debug, Default, Deserialize)]
pub struct ListQuery {
    pub status: Option<String>,
    pub library: Option<String>,
    pub q: Option<String>,
    pub sort: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// Parse `pending,failed` style status lists.
pub fn parse_statuses(values: &[&str]) -> ApiResult<Vec<FileStatus>> {
    let mut out = Vec::new();
    for v in values.iter().flat_map(|v| v.split(',')) {
        let v = v.trim();
        if v.is_empty() {
            continue;
        }
        let status = FileStatus::parse(v).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_status",
                format!(
                    "\"{v}\" isn't a file status. Use pending, queued, processing, done, \
                     skipped or failed."
                ),
            )
        })?;
        if !out.contains(&status) {
            out.push(status);
        }
    }
    Ok(out)
}

/// Parse an optional library id.
pub fn parse_library(value: Option<&str>) -> ApiResult<Option<Uuid>> {
    match value.map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(v) => Uuid::parse_str(v)
            .map(Some)
            .map_err(|_| ApiError::bad_request("invalid_library", "That library id isn't valid.")),
    }
}

/// `GET /api/files`
pub async fn list(
    State(state): State<AppState>,
    ApiQuery(q): ApiQuery<ListQuery>,
) -> ApiResult<Json<ListResponse<MediaFile>>> {
    let statuses = parse_statuses(&q.status.as_deref().into_iter().collect::<Vec<_>>())?;
    let library = parse_library(q.library.as_deref())?;
    let (sort, descending) = match q.sort.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None => (SortKey::Name, false),
        Some(s) => SortKey::parse(s).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_sort",
                "Sort by name, size, updated or status (add - in front for descending).",
            )
        })?,
    };
    let query = FileQuery {
        statuses,
        library,
        search: q.q.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        sort,
        descending,
        limit: q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        offset: q.offset.unwrap_or(0),
    };
    let (items, total) = db::files::list(state.db.pool(), &query).await?;
    Ok(Json(ListResponse { items, total }))
}

/// Response of `GET /api/files/{id}`.
#[derive(Debug, Serialize)]
pub struct FileDetail {
    pub file: MediaFile,
    pub jobs: Vec<Job>,
}

/// `GET /api/files/{id}`
pub async fn get(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<Json<FileDetail>> {
    let file = db::files::get(state.db.pool(), id, true)
        .await?
        .ok_or_else(|| ApiError::not_found("file_not_found", "There's no file with that id."))?;
    let jobs = db::jobs::for_file(state.db.pool(), id, 10).await?;
    Ok(Json(FileDetail { file, jobs }))
}

/// Body of `POST /api/files/{id}/queue`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueBody {
    #[serde(default)]
    pub priority: Option<i32>,
    /// "Convert anyway": convert even a file the goal would leave as it is
    /// (already efficient or in the target format), whatever the result's
    /// size. Verification still applies.
    #[serde(default)]
    pub force: bool,
}

/// `POST /api/files/{id}/queue`
pub async fn queue_file(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    OptionalJson(body): OptionalJson<QueueBody>,
) -> ApiResult<Json<Job>> {
    Ok(Json(
        queue::queue_file(&state, id, body.priority, body.force).await?,
    ))
}

/// `POST /api/files/{id}/skip`
pub async fn skip_file(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<Json<MediaFile>> {
    Ok(Json(queue::skip_file(&state, id).await?))
}

/// A status filter given as `"failed"`, `"pending,failed"` or an array.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum StatusFilter {
    One(String),
    Many(Vec<String>),
}

/// Body of `POST /api/files/bulk`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BulkBody {
    pub action: BulkAction,
    #[serde(default)]
    pub ids: Option<Vec<Uuid>>,
    #[serde(default)]
    pub library: Option<Uuid>,
    #[serde(default)]
    pub status: Option<StatusFilter>,
}

/// `POST /api/files/bulk`
pub async fn bulk(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<BulkBody>,
) -> ApiResult<Json<Value>> {
    let statuses = match &body.status {
        None => Vec::new(),
        Some(StatusFilter::One(s)) => parse_statuses(&[s.as_str()])?,
        Some(StatusFilter::Many(v)) => {
            parse_statuses(&v.iter().map(String::as_str).collect::<Vec<_>>())?
        }
    };
    let outcome = queue::bulk(
        &state,
        body.action,
        BulkSelection {
            ids: body.ids,
            library: body.library,
            statuses,
        },
    )
    .await?;
    Ok(Json(json!({
        "affected": outcome.affected,
        "left_out": outcome.left_out,
    })))
}
