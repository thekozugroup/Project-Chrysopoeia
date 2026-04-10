//! SQLite database setup and migrations using sqlx.

use std::path::Path;

use sqlx::SqlitePool;

/// Initialize the SQLite database, creating tables if they don't exist.
pub async fn initialize_database(path: &Path) -> anyhow::Result<SqlitePool> {
    let db_url = format!("sqlite:{}?mode=rwc", path.display());
    let pool = SqlitePool::connect(&db_url).await?;

    run_migrations(&pool).await?;

    Ok(pool)
}

/// Run database migrations to create the required tables.
async fn run_migrations(pool: &SqlitePool) -> anyhow::Result<()> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS media_files (
            id TEXT PRIMARY KEY NOT NULL,
            path TEXT NOT NULL UNIQUE,
            size INTEGER NOT NULL,
            container TEXT NOT NULL,
            video_codec TEXT,
            audio_codec TEXT,
            resolution_width INTEGER,
            resolution_height INTEGER,
            video_bitrate INTEGER,
            audio_bitrate INTEGER,
            duration_secs REAL,
            status TEXT NOT NULL DEFAULT 'pending',
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS transcode_jobs (
            id TEXT PRIMARY KEY NOT NULL,
            media_file_id TEXT NOT NULL REFERENCES media_files(id),
            status TEXT NOT NULL DEFAULT 'pending',
            progress INTEGER NOT NULL DEFAULT 0,
            target_codec TEXT NOT NULL,
            target_container TEXT NOT NULL,
            hw_accel INTEGER NOT NULL DEFAULT 0,
            started_at TEXT,
            completed_at TEXT,
            error_msg TEXT,
            output_path TEXT,
            size_reduction_pct REAL
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS chat_messages (
            id TEXT PRIMARY KEY NOT NULL,
            role TEXT NOT NULL,
            content TEXT NOT NULL,
            timestamp TEXT NOT NULL,
            job_id TEXT REFERENCES transcode_jobs(id)
        );
        "#,
    )
    .execute(pool)
    .await?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS config (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
        );
        "#,
    )
    .execute(pool)
    .await?;

    tracing::info!("Database migrations completed");
    Ok(())
}
