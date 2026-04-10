//! SQLite database with WAL mode, migrations, and CRUD operations.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

/// Initialize the SQLite database with WAL mode and run migrations.
pub async fn initialize_database(path: &Path) -> anyhow::Result<SqlitePool> {
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    run_migrations(&pool).await?;

    Ok(pool)
}

async fn run_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS media_files (
            id TEXT PRIMARY KEY NOT NULL,
            path TEXT NOT NULL UNIQUE,
            library_path TEXT NOT NULL,
            filename TEXT NOT NULL,
            format TEXT NOT NULL DEFAULT '',
            video_codec TEXT,
            audio_codec TEXT,
            resolution TEXT,
            duration_secs REAL,
            size_bytes INTEGER NOT NULL,
            bitrate_kbps INTEGER,
            status TEXT NOT NULL DEFAULT 'pending',
            progress INTEGER NOT NULL DEFAULT 0,
            speed TEXT,
            eta_secs INTEGER,
            output_size_bytes INTEGER,
            error_message TEXT,
            scanned_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS library_paths (
            id TEXT PRIMARY KEY NOT NULL,
            path TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 1,
            output_video TEXT NOT NULL DEFAULT 'av1',
            output_audio TEXT NOT NULL DEFAULT 'opus',
            output_container TEXT NOT NULL DEFAULT 'mkv',
            crf INTEGER NOT NULL DEFAULT 28,
            skip_open_formats INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS config (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        )",
    )
    .execute(pool)
    .await?;

    // Index for fast status filtering
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_media_files_status ON media_files(status)",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_media_files_library ON media_files(library_path)",
    )
    .execute(pool)
    .await?;

    tracing::info!("Database migrations completed");
    Ok(())
}

// ---------------------------------------------------------------------------
// File operations
// ---------------------------------------------------------------------------

/// Insert a scanned media file. On conflict (same path), update the metadata.
pub async fn upsert_file(
    pool: &SqlitePool,
    id: &str,
    path: &str,
    library_path: &str,
    filename: &str,
    format: &str,
    video_codec: Option<&str>,
    audio_codec: Option<&str>,
    resolution: Option<&str>,
    duration_secs: Option<f64>,
    size_bytes: i64,
    bitrate_kbps: Option<i64>,
    status: &str,
    now: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO media_files (id, path, library_path, filename, format, video_codec, audio_codec, resolution, duration_secs, size_bytes, bitrate_kbps, status, scanned_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)
         ON CONFLICT(path) DO UPDATE SET
           video_codec = ?6, audio_codec = ?7, resolution = ?8,
           duration_secs = ?9, size_bytes = ?10, bitrate_kbps = ?11,
           updated_at = ?13",
    )
    .bind(id).bind(path).bind(library_path).bind(filename)
    .bind(format).bind(video_codec).bind(audio_codec).bind(resolution)
    .bind(duration_secs).bind(size_bytes).bind(bitrate_kbps)
    .bind(status).bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Update a file's transcode status and progress.
pub async fn update_file_status(
    pool: &SqlitePool,
    path: &str,
    status: &str,
    progress: i32,
    speed: Option<&str>,
    eta_secs: Option<i64>,
    output_size_bytes: Option<i64>,
    error_message: Option<&str>,
    now: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE media_files SET status = ?1, progress = ?2, speed = ?3, eta_secs = ?4, output_size_bytes = ?5, error_message = ?6, updated_at = ?7 WHERE path = ?8",
    )
    .bind(status).bind(progress).bind(speed).bind(eta_secs)
    .bind(output_size_bytes).bind(error_message).bind(now).bind(path)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get aggregate stats.
pub async fn get_stats(pool: &SqlitePool) -> anyhow::Result<serde_json::Value> {
    let row = sqlx::query(
        "SELECT
            COUNT(*) as total,
            SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END) as pending,
            SUM(CASE WHEN status = 'queued' THEN 1 ELSE 0 END) as queued,
            SUM(CASE WHEN status = 'transcoding' THEN 1 ELSE 0 END) as transcoding,
            SUM(CASE WHEN status = 'complete' THEN 1 ELSE 0 END) as complete,
            SUM(CASE WHEN status = 'skipped' THEN 1 ELSE 0 END) as skipped,
            SUM(CASE WHEN status = 'error' THEN 1 ELSE 0 END) as errored,
            COALESCE(SUM(size_bytes), 0) as total_size,
            COALESCE(SUM(CASE WHEN status = 'complete' AND output_size_bytes IS NOT NULL THEN size_bytes - output_size_bytes ELSE 0 END), 0) as saved
         FROM media_files",
    )
    .fetch_one(pool)
    .await?;

    Ok(serde_json::json!({
        "total_files": row.get::<i64, _>("total"),
        "pending": row.get::<i64, _>("pending"),
        "queued": row.get::<i64, _>("queued"),
        "transcoding": row.get::<i64, _>("transcoding"),
        "complete": row.get::<i64, _>("complete"),
        "skipped": row.get::<i64, _>("skipped"),
        "errored": row.get::<i64, _>("errored"),
        "total_size_bytes": row.get::<i64, _>("total_size"),
        "saved_bytes": row.get::<i64, _>("saved"),
    }))
}

/// Get all files, optionally filtered by status and/or library.
pub async fn get_files(
    pool: &SqlitePool,
    status: Option<&str>,
    library_id: Option<&str>,
    limit: i64,
    offset: i64,
) -> anyhow::Result<Vec<serde_json::Value>> {
    // Build dynamic query
    let mut sql = String::from("SELECT * FROM media_files WHERE 1=1");
    if status.is_some() {
        sql.push_str(" AND status = ?");
    }
    if library_id.is_some() {
        sql.push_str(" AND library_path = (SELECT path FROM library_paths WHERE id = ?)");
    }
    sql.push_str(" ORDER BY CASE status WHEN 'transcoding' THEN 0 WHEN 'queued' THEN 1 WHEN 'error' THEN 2 WHEN 'pending' THEN 3 WHEN 'complete' THEN 4 WHEN 'skipped' THEN 5 END");
    sql.push_str(" LIMIT ? OFFSET ?");

    // Use raw query since we're building dynamically
    let mut query = sqlx::query(&sql);
    if let Some(s) = status {
        query = query.bind(s);
    }
    if let Some(lid) = library_id {
        query = query.bind(lid);
    }
    query = query.bind(limit).bind(offset);

    let rows = query.fetch_all(pool).await?;
    let files: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>("id"),
                "path": r.get::<String, _>("path"),
                "library_path": r.get::<String, _>("library_path"),
                "filename": r.get::<String, _>("filename"),
                "format": r.get::<String, _>("format"),
                "video_codec": r.get::<Option<String>, _>("video_codec"),
                "audio_codec": r.get::<Option<String>, _>("audio_codec"),
                "resolution": r.get::<Option<String>, _>("resolution"),
                "duration_secs": r.get::<Option<f64>, _>("duration_secs"),
                "size_bytes": r.get::<i64, _>("size_bytes"),
                "bitrate_kbps": r.get::<Option<i64>, _>("bitrate_kbps"),
                "status": r.get::<String, _>("status"),
                "progress": r.get::<i32, _>("progress"),
                "speed": r.get::<Option<String>, _>("speed"),
                "eta_secs": r.get::<Option<i64>, _>("eta_secs"),
                "output_size_bytes": r.get::<Option<i64>, _>("output_size_bytes"),
                "error_message": r.get::<Option<String>, _>("error_message"),
                "scanned_at": r.get::<String, _>("scanned_at"),
            })
        })
        .collect();

    Ok(files)
}

// ---------------------------------------------------------------------------
// Library operations
// ---------------------------------------------------------------------------

/// Insert a library path.
pub async fn insert_library(
    pool: &SqlitePool,
    id: &str,
    path: &str,
    now: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO library_paths (id, path, created_at) VALUES (?1, ?2, ?3)",
    )
    .bind(id).bind(path).bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// Get all library paths.
pub async fn get_libraries(pool: &SqlitePool) -> anyhow::Result<Vec<serde_json::Value>> {
    let rows = sqlx::query("SELECT * FROM library_paths ORDER BY created_at")
        .fetch_all(pool)
        .await?;

    Ok(rows
        .iter()
        .map(|r| {
            serde_json::json!({
                "id": r.get::<String, _>("id"),
                "path": r.get::<String, _>("path"),
                "enabled": r.get::<bool, _>("enabled"),
                "output_video": r.get::<String, _>("output_video"),
                "output_audio": r.get::<String, _>("output_audio"),
                "output_container": r.get::<String, _>("output_container"),
                "crf": r.get::<i32, _>("crf"),
                "skip_open_formats": r.get::<bool, _>("skip_open_formats"),
            })
        })
        .collect())
}

/// Update a library's transcode config.
pub async fn update_library(
    pool: &SqlitePool,
    id: &str,
    output_video: &str,
    output_audio: &str,
    output_container: &str,
    crf: i32,
    skip_open: bool,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE library_paths SET output_video = ?1, output_audio = ?2, output_container = ?3, crf = ?4, skip_open_formats = ?5 WHERE id = ?6",
    )
    .bind(output_video).bind(output_audio).bind(output_container)
    .bind(crf).bind(skip_open).bind(id)
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete a library path.
pub async fn delete_library(pool: &SqlitePool, id: &str) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM library_paths WHERE id = ?1")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Config operations
// ---------------------------------------------------------------------------

/// Get a config value.
pub async fn get_config_value(pool: &SqlitePool, key: &str) -> anyhow::Result<Option<String>> {
    let row = sqlx::query("SELECT value FROM config WHERE key = ?1")
        .bind(key)
        .fetch_optional(pool)
        .await?;
    Ok(row.map(|r| r.get::<String, _>("value")))
}

/// Set a config value (upsert).
pub async fn set_config_value(pool: &SqlitePool, key: &str, value: &str) -> anyhow::Result<()> {
    sqlx::query(
        "INSERT INTO config (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
    )
    .bind(key).bind(value)
    .execute(pool)
    .await?;
    Ok(())
}
