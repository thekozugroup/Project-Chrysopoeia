//! REST API route handlers.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{delete, get, post, put},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::state::AppState;
use chrysopeia_core::models::*;
use chrysopeia_core::profile;

/// Build the API route tree.
pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/stats", get(stats))
        .route("/files", get(list_files))
        .route("/files/{id}", get(get_file))
        .route("/jobs", post(create_job))
        .route("/jobs", get(list_jobs))
        .route("/jobs/{id}", get(get_job))
        .route("/jobs/{id}", delete(cancel_job))
        .route("/hardware", get(hardware))
        .route("/profiles", get(profiles))
        .route("/scan", post(trigger_scan))
        .route("/config", get(get_config))
        .route("/config", put(update_config))
}

/// Pagination query parameters.
#[derive(Debug, Deserialize)]
pub struct PaginationParams {
    pub page: Option<u32>,
    pub per_page: Option<u32>,
}

/// Health check response.
#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
    version: &'static str,
}

/// GET /api/health - Basic health check.
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

/// GET /api/stats - Library statistics.
async fn stats(State(state): State<AppState>) -> Json<LibraryStats> {
    // TODO: Query database for real stats
    let _ = state;
    Json(LibraryStats {
        total_files: 0,
        transcoded: 0,
        pending: 0,
        total_size: 0,
        saved_size: 0,
    })
}

/// GET /api/files - List media files with pagination.
async fn list_files(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<MediaFile>> {
    let _ = (state, params);
    // TODO: Query database with pagination
    Json(vec![])
}

/// GET /api/files/:id - Get a single media file.
async fn get_file(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Json<Option<MediaFile>> {
    let _ = (state, id);
    // TODO: Query database by ID
    Json(None)
}

/// Request body for creating a transcode job.
#[derive(Debug, Deserialize)]
pub struct CreateJobRequest {
    pub media_file_id: Uuid,
    pub profile: Option<String>,
    pub target_codec: Option<String>,
    pub target_container: Option<String>,
}

/// POST /api/jobs - Create a new transcode job.
async fn create_job(
    State(state): State<AppState>,
    Json(req): Json<CreateJobRequest>,
) -> Json<TranscodeJob> {
    let _ = state;
    Json(TranscodeJob {
        id: Uuid::new_v4(),
        media_file_id: req.media_file_id,
        status: TranscodeStatus::Pending,
        progress: 0,
        target_codec: req.target_codec.unwrap_or_else(|| "av1".to_string()),
        target_container: req.target_container.unwrap_or_else(|| "mkv".to_string()),
        hw_accel: false,
        started_at: None,
        completed_at: None,
        error_msg: None,
        output_path: None,
        size_reduction_pct: None,
    })
}

/// GET /api/jobs - List transcode jobs.
async fn list_jobs(
    State(state): State<AppState>,
    Query(params): Query<PaginationParams>,
) -> Json<Vec<TranscodeJob>> {
    let _ = (state, params);
    // TODO: Query database
    Json(vec![])
}

/// GET /api/jobs/:id - Get a single job.
async fn get_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Json<Option<TranscodeJob>> {
    let _ = (state, id);
    // TODO: Query database
    Json(None)
}

/// DELETE /api/jobs/:id - Cancel a job.
async fn cancel_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Json<serde_json::Value> {
    let _ = (state, id);
    // TODO: Cancel via engine
    Json(serde_json::json!({"cancelled": true}))
}

/// GET /api/hardware - Detected hardware capabilities.
async fn hardware(State(state): State<AppState>) -> Json<Vec<HardwareCapability>> {
    Json((*state.capabilities).clone())
}

/// GET /api/profiles - Available transcode profiles.
async fn profiles() -> Json<Vec<chrysopeia_core::profile::TranscodeProfile>> {
    Json(profile::default_profiles())
}

/// POST /api/scan - Trigger a library scan.
async fn trigger_scan(State(state): State<AppState>) -> Json<serde_json::Value> {
    let _ = state;
    // TODO: Trigger scan via scanner handle
    Json(serde_json::json!({"status": "scan_started"}))
}

/// GET /api/config - Get current configuration.
async fn get_config(
    State(state): State<AppState>,
) -> Json<chrysopeia_core::config::AppConfig> {
    let config = state.config.read().await;
    Json(config.clone())
}

/// PUT /api/config - Update configuration.
async fn update_config(
    State(state): State<AppState>,
    Json(new_config): Json<chrysopeia_core::config::AppConfig>,
) -> Json<chrysopeia_core::config::AppConfig> {
    let mut config = state.config.write().await;
    *config = new_config.clone();
    Json(new_config)
}
