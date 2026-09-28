//! REST API under `/api`. Endpoints and shapes follow the table in
//! `docs/ARCHITECTURE.md`. Every error is `{"error", "code"}` JSON, including
//! unknown paths and wrong methods.

pub mod activity;
pub mod extract;
pub mod files;
pub mod fs;
pub mod hardware;
pub mod jobs;
pub mod libraries;
pub mod overview;
pub mod presets;
pub mod queue;
pub mod settings;

use axum::extract::DefaultBodyLimit;
use axum::extract::OriginalUri;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::state::AppState;

/// Largest accepted request body.
pub const BODY_LIMIT: usize = 1024 * 1024;

/// `{"items": [...], "total": n}`.
#[derive(Debug, Serialize)]
pub struct ListResponse<T> {
    pub items: Vec<T>,
    pub total: u64,
}

/// `GET /api/health`
pub async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

/// JSON 404 for unknown API paths.
pub async fn not_found(OriginalUri(uri): OriginalUri) -> ApiError {
    ApiError::not_found(
        "not_found",
        format!("There's no API endpoint at {}.", uri.path()),
    )
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        "This API endpoint doesn't accept that kind of request.",
    )
}

/// The `/api` routes (to be nested under `/api`).
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/overview", get(overview::get))
        .route("/libraries", get(libraries::list).post(libraries::create))
        .route(
            "/libraries/{id}",
            get(libraries::get)
                .patch(libraries::update)
                .delete(libraries::delete),
        )
        .route("/libraries/{id}/scan", post(libraries::scan))
        .route("/scan", post(libraries::scan_all))
        .route("/files", get(files::list))
        .route("/files/bulk", post(files::bulk))
        .route("/files/{id}", get(files::get))
        .route("/files/{id}/queue", post(files::queue_file))
        .route("/files/{id}/skip", post(files::skip_file))
        .route("/jobs", get(jobs::list))
        .route("/jobs/clear", post(jobs::clear))
        .route("/jobs/{id}", get(jobs::get))
        .route("/jobs/{id}/cancel", post(jobs::cancel))
        .route("/jobs/{id}/priority", post(jobs::priority))
        .route("/queue", get(queue::get))
        .route("/queue/pause", post(queue::pause))
        .route("/queue/resume", post(queue::resume))
        .route("/queue/stop", post(queue::stop))
        .route("/settings", get(settings::get).patch(settings::update))
        .route("/hardware", get(hardware::get))
        .route("/hardware/detect", post(hardware::detect))
        .route("/presets", get(presets::get))
        .route("/fs/browse", get(fs::browse))
        .route("/activity", get(activity::list))
        .route("/ws", get(crate::ws::handler))
        .fallback(not_found)
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
}
