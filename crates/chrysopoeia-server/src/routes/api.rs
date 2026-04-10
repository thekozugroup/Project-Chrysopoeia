//! REST API route handlers backed by SQLite.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post, put},
};
use serde::Deserialize;

use crate::db;
use crate::state::AppState;

/// Build the API route tree.
pub fn api_routes() -> Router<AppState> {
    Router::new()
        .route("/health", get(health))
        .route("/stats", get(stats))
        .route("/files", get(list_files))
        .route("/scan", post(trigger_scan))
        .route("/process/start", post(start_processing))
        .route("/process/stop", post(stop_processing))
        .route("/hardware", get(hardware))
        .route("/libraries", get(list_libraries))
        .route("/libraries", post(add_library))
        .route("/libraries/{id}", put(update_library))
        .route("/libraries/{id}", delete(delete_library))
        .route("/config", get(get_config))
        .route("/config", patch(update_config))
}

// --- Health ---

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// --- Stats ---

async fn stats(State(state): State<AppState>) -> impl IntoResponse {
    match db::get_stats(&state.db).await {
        Ok(s) => (StatusCode::OK, Json(s)),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

// --- Files ---

#[derive(Debug, Deserialize)]
struct FileQuery {
    status: Option<String>,
    library: Option<String>,
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn list_files(
    State(state): State<AppState>,
    Query(q): Query<FileQuery>,
) -> impl IntoResponse {
    let limit = q.limit.unwrap_or(100);
    let offset = q.offset.unwrap_or(0);
    match db::get_files(&state.db, q.status.as_deref(), q.library.as_deref(), limit, offset).await
    {
        Ok(files) => (StatusCode::OK, Json(serde_json::json!(files))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

// --- Scan ---

async fn trigger_scan(State(state): State<AppState>) -> impl IntoResponse {
    // Scan all enabled library paths in background
    let db = state.db.clone();
    tokio::spawn(async move {
        let libs = match db::get_libraries(&db).await {
            Ok(l) => l,
            Err(e) => {
                tracing::error!("Failed to get libraries for scan: {e}");
                return;
            }
        };

        let mut total_found = 0u64;
        for lib in &libs {
            let enabled = lib["enabled"].as_bool().unwrap_or(false);
            let path = lib["path"].as_str().unwrap_or("");
            if !enabled || path.is_empty() {
                continue;
            }

            tracing::info!("Scanning library: {path}");
            match chrysopoeia_scanner::scan_directory(std::path::Path::new(path)).await {
                Ok(files) => {
                    let count = files.len();
                    let now = chrono::Utc::now().to_rfc3339();
                    for f in &files {
                        let filename = std::path::Path::new(&f.path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("");
                        let res_str = f.format.resolution.as_ref().map(|r| format!("{}x{}", r.width, r.height));

                        let status = if chrysopoeia_core::codec::is_open_format(&f.format) {
                            "skipped"
                        } else {
                            "pending"
                        };

                        if let Err(e) = db::upsert_file(
                            &db,
                            &f.id.to_string(),
                            &f.path,
                            path,
                            filename,
                            &f.format.container,
                            f.format.video_codec.as_deref(),
                            f.format.audio_codec.as_deref(),
                            res_str.as_deref(),
                            f.format.duration_secs,
                            f.size as i64,
                            f.format.video_bitrate.map(|b| (b / 1000) as i64),
                            status,
                            &now,
                        ).await {
                            tracing::warn!("Failed to insert file {}: {e}", f.path);
                        }
                    }
                    total_found += count as u64;
                    tracing::info!("Scanned {path}: found {count} media files");
                }
                Err(e) => {
                    tracing::error!("Scan failed for {path}: {e}");
                }
            }
        }
        tracing::info!("Scan complete: {total_found} total files found");
    });

    Json(serde_json::json!({"status": "scan_started"}))
}

// --- Processing control ---

async fn start_processing(State(state): State<AppState>) -> impl IntoResponse {
    let mut processing = state.processing.write().await;
    *processing = true;
    Json(serde_json::json!({"processing": true}))
}

async fn stop_processing(State(state): State<AppState>) -> impl IntoResponse {
    let mut processing = state.processing.write().await;
    *processing = false;
    // Cancel active jobs
    state.engine.cancel_all().await;
    Json(serde_json::json!({"processing": false}))
}

// --- Hardware ---

async fn hardware(State(state): State<AppState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "gpu_name": state.hardware_info.gpu_name,
        "gpu_vendor": state.hardware_info.gpu_vendor,
        "formats": state.hardware_info.formats,
        "cpu_cores": state.hardware_info.cpu_cores,
        "ram_gb": state.hardware_info.ram_gb,
    }))
}

// --- Libraries ---

async fn list_libraries(State(state): State<AppState>) -> impl IntoResponse {
    match db::get_libraries(&state.db).await {
        Ok(libs) => (StatusCode::OK, Json(serde_json::json!(libs))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct AddLibraryRequest {
    path: String,
}

async fn add_library(
    State(state): State<AppState>,
    Json(req): Json<AddLibraryRequest>,
) -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    match db::insert_library(&state.db, &id, &req.path, &now).await {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"id": id, "path": req.path})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

#[derive(Debug, Deserialize)]
struct UpdateLibraryRequest {
    output_video: Option<String>,
    output_audio: Option<String>,
    output_container: Option<String>,
    crf: Option<i32>,
    skip_open_formats: Option<bool>,
}

async fn update_library(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<UpdateLibraryRequest>,
) -> impl IntoResponse {
    match db::update_library(
        &state.db,
        &id,
        &req.output_video.unwrap_or_else(|| "av1".into()),
        &req.output_audio.unwrap_or_else(|| "opus".into()),
        &req.output_container.unwrap_or_else(|| "mkv".into()),
        req.crf.unwrap_or(28),
        req.skip_open_formats.unwrap_or(true),
    )
    .await
    {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"updated": true}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

async fn delete_library(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    match db::delete_library(&state.db, &id).await {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"deleted": true}))),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        ),
    }
}

// --- Config ---

async fn get_config(State(state): State<AppState>) -> impl IntoResponse {
    let config = state.config.read().await;
    Json(serde_json::json!(*config))
}

async fn update_config(
    State(state): State<AppState>,
    Json(patch): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut config = state.config.write().await;
    // Merge patch into config
    if let (Some(existing), Some(new)) = (config.as_object_mut(), patch.as_object()) {
        for (k, v) in new {
            existing.insert(k.clone(), v.clone());
        }
    }
    Json(serde_json::json!(*config))
}
