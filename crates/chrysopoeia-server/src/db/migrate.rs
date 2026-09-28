//! Schema migrations, versioned with `PRAGMA user_version`.
//!
//! Version 1 is the schema in `docs/ARCHITECTURE.md`. A database at version 0
//! (the prototype's `media_files`/`library_paths`/`config` tables, or an
//! interrupted first start) only ever held re-scannable data, so every table
//! in it is dropped and the schema is created from scratch. Later versions
//! are applied step by step on top of version 1 and keep the data.

use anyhow::{Context, bail};
use sqlx::{Row, SqlitePool};

/// Schema version this build writes.
pub const SCHEMA_VERSION: i64 = 3;

const SCHEMA_V1: &[&str] = &[
    "CREATE TABLE settings (
        key   TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    )",
    "CREATE TABLE libraries (
        id           TEXT PRIMARY KEY NOT NULL,
        name         TEXT NOT NULL,
        path         TEXT NOT NULL UNIQUE,
        enabled      INTEGER NOT NULL DEFAULT 1,
        profile      TEXT NOT NULL,
        last_scan_at TEXT,
        created_at   TEXT NOT NULL
    )",
    "CREATE TABLE files (
        id                  TEXT PRIMARY KEY NOT NULL,
        library_id          TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        path                TEXT NOT NULL UNIQUE,
        relative_path       TEXT NOT NULL,
        file_name           TEXT NOT NULL,
        size_bytes          INTEGER NOT NULL,
        modified_at         TEXT NOT NULL,
        status              TEXT NOT NULL,
        probe               TEXT,
        container           TEXT,
        video_codec         TEXT,
        audio_codec         TEXT,
        resolution          TEXT,
        hdr                 TEXT,
        duration_secs       REAL,
        bit_rate            INTEGER,
        original_size_bytes INTEGER,
        saved_bytes         INTEGER,
        skip_reason         TEXT,
        error               TEXT,
        job_id              TEXT,
        scanned_at          TEXT NOT NULL,
        updated_at          TEXT NOT NULL
    )",
    "CREATE INDEX idx_files_library_status ON files(library_id, status)",
    "CREATE INDEX idx_files_status ON files(status)",
    "CREATE TABLE jobs (
        id          TEXT PRIMARY KEY NOT NULL,
        file_id     TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        library_id  TEXT NOT NULL,
        file_name   TEXT NOT NULL,
        file_path   TEXT NOT NULL,
        state       TEXT NOT NULL,
        stage       TEXT NOT NULL,
        priority    INTEGER NOT NULL DEFAULT 0,
        progress    REAL NOT NULL DEFAULT 0,
        fps         REAL,
        speed       REAL,
        eta_secs    INTEGER,
        encoder     TEXT,
        hw_api      TEXT,
        attempt     INTEGER NOT NULL DEFAULT 0,
        input_size  INTEGER NOT NULL DEFAULT 0,
        output_size INTEGER,
        error       TEXT,
        skip_reason TEXT,
        validation  TEXT,
        command     TEXT,
        log_tail    TEXT,
        created_at  TEXT NOT NULL,
        started_at  TEXT,
        finished_at TEXT
    )",
    "CREATE INDEX idx_jobs_queue ON jobs(state, priority DESC, created_at)",
    "CREATE INDEX idx_jobs_file ON jobs(file_id)",
    "CREATE TABLE activity (
        id         INTEGER PRIMARY KEY AUTOINCREMENT,
        at         TEXT NOT NULL,
        level      TEXT NOT NULL,
        message    TEXT NOT NULL,
        file_id    TEXT,
        job_id     TEXT,
        library_id TEXT
    )",
    "CREATE TABLE savings (
        date        TEXT PRIMARY KEY NOT NULL,
        saved_bytes INTEGER NOT NULL DEFAULT 0,
        files       INTEGER NOT NULL DEFAULT 0
    )",
];

/// Version 2: indexes for the job history, the job list and a file's jobs
/// (so those pages don't sort the whole table), and `finished_at` on every
/// finished job (the history is ordered by it).
const MIGRATION_V2: &[&str] = &[
    "UPDATE jobs SET finished_at = COALESCE(finished_at, started_at, created_at) \
     WHERE state IN ('done', 'skipped', 'failed', 'cancelled') AND finished_at IS NULL",
    "CREATE INDEX IF NOT EXISTS idx_jobs_finished ON jobs(finished_at) \
     WHERE state IN ('done', 'skipped', 'failed', 'cancelled')",
    "CREATE INDEX IF NOT EXISTS idx_jobs_created ON jobs(created_at)",
    "DROP INDEX IF EXISTS idx_jobs_file",
    "CREATE INDEX idx_jobs_file ON jobs(file_id, created_at)",
];

/// Version 3: `jobs.notes`, a JSON array of plain-language notes about
/// compromises a finished conversion made (NULL when there are none).
const MIGRATION_V3: &[&str] = &["ALTER TABLE jobs ADD COLUMN notes TEXT"];

/// Steps applied on top of version 1, in order: (version reached, statements).
const MIGRATIONS: &[(i64, &[&str])] = &[(2, MIGRATION_V2), (3, MIGRATION_V3)];

/// Bring the database to [`SCHEMA_VERSION`]. Safe to run on every start.
pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *conn)
        .await?;
    if version == SCHEMA_VERSION {
        return Ok(());
    }
    if version > SCHEMA_VERSION {
        bail!(
            "This database was created by a newer version of Chrysopoeia (schema {version}). \
             Update Chrysopoeia, or move the database file away to start fresh."
        );
    }
    if version < 1 {
        create_v1(&mut conn).await?;
    }
    for (target, statements) in MIGRATIONS {
        if *target <= version {
            continue;
        }
        let mut tx = sqlx::Connection::begin(&mut *conn).await?;
        for statement in *statements {
            sqlx::query(statement)
                .execute(&mut *tx)
                .await
                .with_context(|| format!("could not update the database to version {target}"))?;
        }
        sqlx::query(&format!("PRAGMA user_version = {target}"))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        tracing::info!(version = target, "database updated");
    }
    Ok(())
}

/// Replace whatever a version-0 database holds with the version 1 schema.
async fn create_v1(conn: &mut sqlx::SqliteConnection) -> anyhow::Result<()> {
    // Dropping tables that reference each other needs foreign keys off, and
    // the pragma cannot change inside a transaction.
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await?;
    let result = async {
        let mut tx = sqlx::Connection::begin(&mut *conn).await?;
        let tables: Vec<String> = sqlx::query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(|r| r.try_get::<String, _>("name"))
        .collect::<Result<_, _>>()?;
        for table in &tables {
            tracing::info!(table, "dropping a table from an older database version");
            let sql = format!("DROP TABLE IF EXISTS \"{}\"", table.replace('"', "\"\""));
            sqlx::query(&sql).execute(&mut *tx).await?;
        }
        for statement in SCHEMA_V1 {
            sqlx::query(statement).execute(&mut *tx).await?;
        }
        sqlx::query("PRAGMA user_version = 1")
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        anyhow::Ok(())
    }
    .await;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await?;
    result.context("could not create the database tables")?;
    tracing::info!("database schema created");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;

    async fn table_names(pool: &SqlitePool) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
             ORDER BY name",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn fresh_database_gets_the_current_schema() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).await.unwrap();
        let v: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        assert_eq!(
            table_names(db.pool()).await,
            [
                "activity",
                "files",
                "jobs",
                "libraries",
                "savings",
                "settings"
            ]
        );
        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(fk, 1);
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[tokio::test]
    async fn rerun_is_idempotent_and_keeps_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let db = Db::open(&path).await.unwrap();
        sqlx::query("INSERT INTO settings (key, value) VALUES ('k', 'v')")
            .execute(db.pool())
            .await
            .unwrap();
        migrate(db.pool()).await.unwrap();
        db.close().await;
        let db = Db::open(&path).await.unwrap();
        let v: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'k'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(v, "v");
    }

    #[tokio::test]
    async fn prototype_tables_are_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            for sql in [
                "CREATE TABLE media_files (id TEXT PRIMARY KEY, library_path TEXT, status TEXT)",
                "CREATE TABLE library_paths (id TEXT PRIMARY KEY, path TEXT)",
                "CREATE TABLE config (key TEXT PRIMARY KEY, value TEXT)",
                "INSERT INTO media_files VALUES ('a', '/m', 'done')",
            ] {
                sqlx::query(sql).execute(&pool).await.unwrap();
            }
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        let names = table_names(db.pool()).await;
        assert!(!names.contains(&"media_files".to_string()));
        assert!(!names.contains(&"library_paths".to_string()));
        assert!(!names.contains(&"config".to_string()));
        assert!(names.contains(&"files".to_string()));
    }

    #[tokio::test]
    async fn version_1_is_upgraded_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            let mut conn = pool.acquire().await.unwrap();
            create_v1(&mut conn).await.unwrap();
            for sql in [
                "INSERT INTO libraries (id, name, path, profile, created_at) \
                 VALUES ('l', 'Movies', '/m', '{}', '2026-01-01T00:00:00.000Z')",
                "INSERT INTO files (id, library_id, path, relative_path, file_name, size_bytes, \
                 modified_at, status, scanned_at, updated_at) VALUES ('f', 'l', '/m/a.mkv', \
                 'a.mkv', 'a.mkv', 1, 'x', 'done', 'x', 'x')",
                "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, stage, \
                 created_at) VALUES ('j', 'f', 'l', 'a.mkv', '/m/a.mkv', 'failed', 'waiting', \
                 '2026-01-02T00:00:00.000Z')",
            ] {
                sqlx::query(sql).execute(&mut *conn).await.unwrap();
            }
            drop(conn);
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        let v: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let finished: Option<String> =
            sqlx::query_scalar("SELECT finished_at FROM jobs WHERE id = 'j'")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(finished.as_deref(), Some("2026-01-02T00:00:00.000Z"));
        // Version 3 added job notes; old jobs have none.
        let job = crate::db::jobs::get(db.pool(), uuid::Uuid::nil())
            .await
            .unwrap();
        assert!(job.is_none());
        let notes: Option<String> = sqlx::query_scalar("SELECT notes FROM jobs WHERE id = 'j'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(notes, None);
        // The job pages read an index in order instead of sorting the table.
        for filter in [
            crate::db::jobs::JobFilter::History,
            crate::db::jobs::JobFilter::Queued,
            crate::db::jobs::JobFilter::All,
        ] {
            let plan = crate::db::jobs::explain_list(db.pool(), filter).await;
            assert!(
                !plan.iter().any(|d| d.contains("TEMP B-TREE")),
                "{filter:?}: {plan:?}"
            );
        }
        let plan: Vec<String> = sqlx::query(
            "EXPLAIN QUERY PLAN SELECT id FROM jobs WHERE file_id = 'f' \
             ORDER BY created_at DESC, rowid DESC LIMIT 10",
        )
        .fetch_all(db.pool())
        .await
        .unwrap()
        .iter()
        .map(|r| r.get::<String, _>("detail"))
        .collect();
        assert!(!plan.iter().any(|d| d.contains("TEMP B-TREE")), "{plan:?}");
    }

    #[tokio::test]
    async fn newer_schema_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            sqlx::query("PRAGMA user_version = 99")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        }
        let err = Db::open(&path).await.unwrap_err().to_string();
        assert!(err.contains("newer version"), "{err}");
    }
}
