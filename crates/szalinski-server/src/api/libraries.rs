//! `/api/libraries` and `/api/scan`.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};
use szalinski_core::{Goal, Library, TranscodeProfile};
use uuid::Uuid;

use super::extract::{ApiJson, ApiPath};
use crate::db;
use crate::error::{ApiError, ApiResult};
use crate::services::library::{self, ScanStartError};
use crate::services::library_admin::{self, LibraryPatch, NewLibrary};
use crate::state::AppState;

/// Body of `POST /api/libraries`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    pub path: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub profile: Option<TranscodeProfile>,
    #[serde(default)]
    pub goal: Option<Goal>,
}

/// Body of `PATCH /api/libraries/{id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchBody {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub profile: Option<TranscodeProfile>,
}

/// `GET /api/libraries`
pub async fn list(State(state): State<AppState>) -> ApiResult<Json<Vec<Library>>> {
    Ok(Json(library::view_all(&state).await?))
}

/// `POST /api/libraries`
pub async fn create(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<CreateBody>,
) -> ApiResult<(StatusCode, Json<Library>)> {
    let lib = library_admin::create(
        &state,
        NewLibrary {
            path: body.path,
            name: body.name,
            profile: body.profile,
            goal: body.goal,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(lib)))
}

fn not_found() -> ApiError {
    ApiError::not_found("library_not_found", "There's no library with that id.")
}

/// `GET /api/libraries/{id}`
pub async fn get(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<Json<Library>> {
    library::view_by_id(&state, id)
        .await?
        .map(Json)
        .ok_or_else(not_found)
}

/// `PATCH /api/libraries/{id}`
pub async fn update(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
    ApiJson(body): ApiJson<PatchBody>,
) -> ApiResult<Json<Library>> {
    let lib = library_admin::update(
        &state,
        id,
        LibraryPatch {
            name: body.name,
            enabled: body.enabled,
            profile: body.profile,
        },
    )
    .await?;
    Ok(Json(lib))
}

/// `DELETE /api/libraries/{id}`
pub async fn delete(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<StatusCode> {
    library_admin::delete(&state, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/libraries/{id}/scan`
pub async fn scan(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let lib = db::libraries::get(state.db.pool(), id)
        .await?
        .ok_or_else(not_found)?;
    if !lib.enabled {
        return Err(ApiError::conflict(
            "library_disabled",
            "This library is turned off. Turn it on to scan it.",
        ));
    }
    match library::start_scan(&state, id) {
        Ok(()) => Ok((StatusCode::ACCEPTED, Json(json!({ "started": true })))),
        Err(ScanStartError::AlreadyScanning) => Err(ApiError::conflict(
            "scan_running",
            "This library is already being scanned.",
        )),
    }
}

/// `POST /api/libraries/{id}/relearn-mounts`: take the drive mounted now
/// where one the library's folders were seen mounted from (another one
/// put there on purpose) as the usual one.
pub async fn relearn_mounts(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<Uuid>,
) -> ApiResult<Json<Library>> {
    Ok(Json(library_admin::relearn_mounts(&state, id).await?))
}

/// `POST /api/scan`
pub async fn scan_all(State(state): State<AppState>) -> ApiResult<(StatusCode, Json<Value>)> {
    let started = library::scan_all(&state).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(json!({ "started": true, "libraries": started })),
    ))
}
