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
pub const SCHEMA_VERSION: i64 = 14;

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

/// Version 4: `jobs.final_path`, where a running job puts its result, so
/// start-up recovery can finish a replacement a crash interrupted.
const MIGRATION_V4: &[&str] = &["ALTER TABLE jobs ADD COLUMN final_path TEXT"];

/// Version 5:
/// - `jobs.force`: the job converts the file anyway ("Convert anyway").
/// - `libraries.settling`: files the last scan left for later because they
///   were still being copied.
/// - `savings` keyed by day and library, so removing a library removes its
///   share of the history (the chart then describes the same files as the
///   total above it). The old daily rows can't be split by library and may
///   hold savings of libraries removed before (even when one library is
///   left), so the history is rebuilt from the conversions still on
///   record: each file's conversion is kept by history trimming, so they
///   cover the 30 days the chart shows (unless the history was cleared).
const MIGRATION_V5: &[&str] = &[
    "ALTER TABLE jobs ADD COLUMN force INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE libraries ADD COLUMN settling INTEGER NOT NULL DEFAULT 0",
    "CREATE TABLE savings_by_library (
        date        TEXT NOT NULL,
        library_id  TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        saved_bytes INTEGER NOT NULL DEFAULT 0,
        files       INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (date, library_id)
    )",
    "INSERT INTO savings_by_library (date, library_id, saved_bytes, files) \
     SELECT substr(j.finished_at, 1, 10), j.library_id, \
            SUM(j.input_size - j.output_size), COUNT(*) \
     FROM jobs j JOIN libraries l ON l.id = j.library_id \
     WHERE j.state = 'done' AND j.output_size IS NOT NULL AND j.finished_at IS NOT NULL \
     GROUP BY substr(j.finished_at, 1, 10), j.library_id",
    "DROP TABLE savings",
    "ALTER TABLE savings_by_library RENAME TO savings",
];

/// The kind of problem behind an `error` recorded before version 6, from
/// its wording (the messages of that version are fixed and known). Order
/// matters: a damaged original can be the last error of several attempts,
/// and a failed move can quote a full disk. Anything else is `other`.
macro_rules! problem_from_error {
    () => {
        "CASE \
            WHEN error LIKE 'The file is no longer at %' \
              OR error LIKE 'The file no longer exists%' \
              OR error LIKE 'The original file is no longer there%' \
              OR error LIKE 'This file is no longer in the library%' THEN 'source_changed' \
            WHEN error LIKE '%The original file appears damaged or incomplete%' \
              OR error LIKE 'This file can''t be read as a video%' \
              OR error LIKE 'The file is empty%' \
              OR error LIKE 'Could not read the file%' \
              OR error LIKE 'Reading this file took longer%' \
              OR error LIKE '%doesn''t have permission to read this file%' \
              THEN 'unreadable_source' \
            WHEN error LIKE 'The new file %' \
              OR error LIKE 'All % attempts failed. Last error: The new file %' \
              THEN 'verification' \
            WHEN error LIKE 'Not enough free space%' \
              OR error LIKE '%ran out of space%' \
              OR error LIKE '%disk quota%' \
              OR error LIKE '%No space left on device%' THEN 'disk_full' \
            WHEN error LIKE 'Could not use the temp folder%' THEN 'work_folder' \
            WHEN error LIKE '%converting on the CPU instead is turned off%' \
              OR error LIKE 'No working encoder was found%' THEN 'hardware_unavailable' \
            WHEN error LIKE '%doesn''t have permission to write in%' \
              OR error LIKE '%already exists next to the original%' \
              OR error LIKE '%already exists in the output folder%' \
              OR error LIKE 'Output to a separate folder is on%' \
              OR error LIKE 'Could not put the new file in place%' THEN 'destination' \
            WHEN error LIKE '% stopped with exit code %' \
              OR error LIKE '% was stopped by the system%' \
              OR error LIKE '% stopped responding for %' \
              OR error LIKE '% finished but wrote no output%' THEN 'encoder' \
            ELSE 'other' END"
    };
}

/// Version 6: `jobs.problem` and `files.problem`, the kind of problem behind
/// an `error` (`ProblemKind` as snake_case text), so the UI can group
/// problems and offer the right fix without reading sentences. Errors
/// recorded before are sorted by their wording (see `problem_from_error`).
const MIGRATION_V6: &[&str] = &[
    "ALTER TABLE jobs ADD COLUMN problem TEXT",
    "ALTER TABLE files ADD COLUMN problem TEXT",
    concat!(
        "UPDATE jobs SET problem = ",
        problem_from_error!(),
        " WHERE error IS NOT NULL"
    ),
    concat!(
        "UPDATE files SET problem = ",
        problem_from_error!(),
        " WHERE error IS NOT NULL"
    ),
];

/// Version 7: portrait videos get the resolution label of the same picture
/// turned sideways (1080x1920 is 1080p, not 1440p), as
/// `szalinski_core::media::resolution_label` now gives it. Only rows whose
/// main picture (the first video track that isn't cover art) is taller than
/// wide change; the `CASE` is that function for a landscape picture.
const MIGRATION_V7: &[&str] = &["UPDATE files SET resolution = COALESCE(( \
        SELECT CASE \
            WHEN MAX(w, h) >= 7000 OR MIN(w, h) >= 4000 THEN '8K' \
            WHEN MAX(w, h) >= 3200 OR MIN(w, h) >= 2000 THEN '4K' \
            WHEN MAX(w, h) >= 2300 OR MIN(w, h) >= 1400 THEN '1440p' \
            WHEN MAX(w, h) >= 1700 OR MIN(w, h) >= 1000 THEN '1080p' \
            WHEN MAX(w, h) >= 1200 OR MIN(w, h) >= 700 THEN '720p' \
            WHEN MAX(w, h) >= 1000 OR MIN(w, h) >= 560 THEN '576p' \
            WHEN MIN(w, h) >= 470 THEN '480p' \
            ELSE 'SD' END \
        FROM (SELECT json_extract(s.value, '$.width') AS w, \
                     json_extract(s.value, '$.height') AS h \
              FROM json_each(files.probe, '$.streams') AS s \
              WHERE json_extract(s.value, '$.kind') = 'video' \
                AND COALESCE(json_extract(s.value, '$.is_attached_pic'), 0) = 0 \
              ORDER BY s.key LIMIT 1) \
        WHERE w IS NOT NULL AND h IS NOT NULL AND h > w \
     ), resolution) \
     WHERE probe IS NOT NULL AND resolution IS NOT NULL AND json_valid(probe)"];

/// Version 8: `library_mounts`, the drives and shares mounted inside a
/// library folder that scans have seen. When one of them is unmounted, its
/// mount point is left behind empty; a scan then keeps its files listed
/// (with their state, such as "Skipped by you") instead of removing them.
const MIGRATION_V8: &[&str] = &["CREATE TABLE library_mounts (
        library_id TEXT NOT NULL REFERENCES libraries(id) ON DELETE CASCADE,
        path       TEXT NOT NULL,
        PRIMARY KEY (library_id, path)
    )"];

/// Version 9:
/// - `jobs.freed_bytes`: the disk space a finished conversion released (see
///   `Job::freed_bytes`). Not guessed for conversions finished before: those
///   jobs keep `NULL`.
/// - `activity.problem`: the kind of problem an entry about a failed file
///   is about (`ProblemKind` as snake_case text). The failures already in
///   the feed take it from the failed job they refer to.
const MIGRATION_V9: &[&str] = &[
    "ALTER TABLE jobs ADD COLUMN freed_bytes INTEGER",
    "ALTER TABLE activity ADD COLUMN problem TEXT",
    "UPDATE activity SET problem = (SELECT j.problem FROM jobs j \
         WHERE j.id = activity.job_id AND j.state = 'failed' AND j.error IS NOT NULL) \
     WHERE level = 'error' AND job_id IS NOT NULL",
];

/// Version 10:
/// - `files.verdict_profile`: the goal (profile JSON) the file's `pending`
///   or `skipped` verdict was decided with, so a verdict made under another
///   goal is decided again (see `services::library::redecide`).
/// - `jobs.profile`: the goal (profile JSON) a job ran with, written when it
///   starts.
///
/// Verdicts recorded before are taken to be the library's current goal's,
/// except a file a job skipped by the size rule ("Only 4% smaller — kept
/// the original") in a library whose goal has no size rule now: that verdict
/// came from an earlier goal, so it is left unknown and decided again by
/// the next scan.
const MIGRATION_V10: &[&str] = &[
    "ALTER TABLE files ADD COLUMN verdict_profile TEXT",
    "ALTER TABLE jobs ADD COLUMN profile TEXT",
    "UPDATE files SET verdict_profile = \
         (SELECT l.profile FROM libraries l WHERE l.id = files.library_id) \
     WHERE status IN ('pending', 'skipped') AND NOT ( \
         status = 'skipped' \
         AND EXISTS (SELECT 1 FROM jobs j WHERE j.id = files.job_id AND j.state = 'skipped' \
             AND j.output_size IS NOT NULL AND j.skip_reason IS files.skip_reason) \
         AND EXISTS (SELECT 1 FROM libraries l WHERE l.id = files.library_id \
             AND COALESCE(CASE WHEN json_valid(l.profile) \
                 THEN json_type(l.profile, '$.min_savings_pct') ELSE 'unknown' END, 'null') \
                 = 'null'))",
];

/// Version 11: `jobs.placing` marks a job whose new file was still being
/// put in place when the job ended (a share that stopped answering, or a
/// stop, left that step to go on by itself) or when the server stopped
/// (1), so that what is on the disk tells whether it got there before
/// anything else touches the job's backup of the original, and one whose
/// new file was found in place, with only its backup left to remove (2);
/// see `services::dispatcher::settle`. `jobs.placing_size` is the new
/// file's size and `jobs.placing_original_size` the original's (`NULL`:
/// not known). Only marked jobs are indexed.
const MIGRATION_V11: &[&str] = &[
    "ALTER TABLE jobs ADD COLUMN placing INTEGER NOT NULL DEFAULT 0",
    "ALTER TABLE jobs ADD COLUMN placing_size INTEGER",
    "ALTER TABLE jobs ADD COLUMN placing_original_size INTEGER",
    "CREATE INDEX idx_jobs_placing ON jobs(placing) WHERE placing > 0",
];

/// Version 12: `folder_mounts`, the mount points each folder Szalinski
/// uses (a library folder, the output folder, the work folder) was seen on
/// or under. An unmounted share leaves its mount point behind as an
/// ordinary folder; while one of these isn't mounted, its folder counts as
/// not connected, so nothing takes that folder for the share (see
/// `services::share_mounts`). Nothing is known for the folders in use
/// before: they are learned from the next look at them.
const MIGRATION_V12: &[&str] = &["CREATE TABLE folder_mounts (
        folder TEXT NOT NULL,
        mount  TEXT NOT NULL,
        PRIMARY KEY (folder, mount)
    )"];

/// Version 13: what was mounted at each remembered mount point
/// (`folder_mounts.fstype`, `source` and `root`, from
/// `/proc/self/mountinfo`), so another filesystem mounted at that place (a
/// tmpfs, the bare folder bind-mounted onto itself) isn't taken for the
/// share; and `jobs.final_mount`, the mount the folder a job's new file goes
/// into was on when it started (JSON: `point`, `fstype`, `source`, `root`),
/// so its new file is only looked for on that mount when its placing is
/// settled. The mount points remembered before are noted with what is
/// mounted there at the next look at them.
const MIGRATION_V13: &[&str] = &[
    "ALTER TABLE folder_mounts ADD COLUMN fstype TEXT",
    "ALTER TABLE folder_mounts ADD COLUMN source TEXT",
    "ALTER TABLE folder_mounts ADD COLUMN root TEXT",
    "ALTER TABLE jobs ADD COLUMN final_mount TEXT",
];

/// Version 14: `jobs.attempts`, every way of converting the file a job
/// tried with how each ended (JSON `JobAttempt[]`: encoder, decoding path,
/// time taken, command, the failed check or ffmpeg's error and a short log
/// tail), written as each attempt ends; and how a running job's progress
/// was worked out while transcoding (`jobs.progress_basis`: `time`,
/// `frames` or `unknown`), with the frames encoded so far and the seconds
/// the attempt has run (`jobs.frames`, `jobs.elapsed_secs`). Jobs from
/// before have no attempts recorded (`NULL`, read as none).
const MIGRATION_V14: &[&str] = &[
    "ALTER TABLE jobs ADD COLUMN attempts TEXT",
    "ALTER TABLE jobs ADD COLUMN progress_basis TEXT",
    "ALTER TABLE jobs ADD COLUMN frames INTEGER",
    "ALTER TABLE jobs ADD COLUMN elapsed_secs INTEGER",
];

/// Steps applied on top of version 1, in order: (version reached, statements).
const MIGRATIONS: &[(i64, &[&str])] = &[
    (2, MIGRATION_V2),
    (3, MIGRATION_V3),
    (4, MIGRATION_V4),
    (5, MIGRATION_V5),
    (6, MIGRATION_V6),
    (7, MIGRATION_V7),
    (8, MIGRATION_V8),
    (9, MIGRATION_V9),
    (10, MIGRATION_V10),
    (11, MIGRATION_V11),
    (12, MIGRATION_V12),
    (13, MIGRATION_V13),
    (14, MIGRATION_V14),
];

/// Bring the database to [`SCHEMA_VERSION`]. Safe to run on every start.
pub async fn migrate(pool: &SqlitePool) -> anyhow::Result<()> {
    migrate_to(pool, SCHEMA_VERSION).await
}

/// Bring the database towards [`SCHEMA_VERSION`] step by step, stopping at
/// version `last` (tests build older databases with it).
async fn migrate_to(pool: &SqlitePool, last: i64) -> anyhow::Result<()> {
    let mut conn = pool.acquire().await?;
    let version: i64 = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *conn)
        .await?;
    if version > SCHEMA_VERSION {
        bail!(
            "This database was created by a newer version of Szalinski (schema {version}). \
             Update Szalinski, or move the database file away to start fresh."
        );
    }
    if version >= last {
        return Ok(());
    }
    if version < 1 {
        create_v1(&mut conn).await?;
    }
    for (target, statements) in MIGRATIONS {
        if *target <= version || *target > last {
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
        tracing::debug!(version = target, "database updated");
    }
    // A new database is simply created; only an upgrade is worth a line.
    if version >= 1 {
        tracing::info!(from = version, to = last, "database updated");
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
                "folder_mounts",
                "jobs",
                "libraries",
                "library_mounts",
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

    /// A version 4 database with `libraries` and `jobs` rows, and daily
    /// savings of `saved` bytes on 2026-09-01.
    async fn version_4_db(path: &std::path::Path, libraries: &[&str], saved: i64) {
        let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
            .await
            .unwrap();
        migrate_to(&pool, 4).await.unwrap();
        for (i, lib) in libraries.iter().enumerate() {
            for sql in [
                format!(
                    "INSERT INTO libraries (id, name, path, profile, created_at) \
                     VALUES ('{lib}', '{lib}', '/m/{lib}', '{{}}', '2026-01-01T00:00:00.000Z')"
                ),
                format!(
                    "INSERT INTO files (id, library_id, path, relative_path, file_name, \
                     size_bytes, modified_at, status, saved_bytes, scanned_at, updated_at) \
                     VALUES ('f{i}', '{lib}', '/m/{lib}/a.mkv', 'a.mkv', 'a.mkv', 60, 'x', \
                     'done', 40, 'x', 'x')"
                ),
                format!(
                    "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, \
                     stage, input_size, output_size, created_at, finished_at) VALUES ('j{i}', \
                     'f{i}', '{lib}', 'a.mkv', '/m/{lib}/a.mkv', 'done', 'finalizing', 100, 60, \
                     '2026-09-01T10:00:00.000Z', '2026-09-01T11:00:00.000Z')"
                ),
            ] {
                sqlx::query(&sql).execute(&pool).await.unwrap();
            }
        }
        sqlx::query("INSERT INTO savings (date, saved_bytes, files) VALUES ('2026-09-01', ?, 3)")
            .bind(saved)
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    async fn savings_rows(pool: &SqlitePool) -> Vec<(String, String, i64, i64)> {
        sqlx::query_as(
            "SELECT date, library_id, saved_bytes, files FROM savings ORDER BY library_id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn version_5_keys_savings_by_library() {
        // One library left, but the old daily total (1234) also holds the
        // savings of libraries removed before: rebuilt from its own
        // conversions, so the chart matches the total space saved.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("one.db");
        version_4_db(&path, &["a"], 1234).await;
        let db = Db::open(&path).await.unwrap();
        assert_eq!(
            savings_rows(db.pool()).await,
            [("2026-09-01".to_string(), "a".to_string(), 40, 1)]
        );
        let force: i64 = sqlx::query_scalar("SELECT force FROM jobs WHERE id = 'j0'")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(force, 0);

        // Several: rebuilt per library from the finished conversions.
        let path = dir.path().join("two.db");
        version_4_db(&path, &["a", "b"], 999).await;
        let db = Db::open(&path).await.unwrap();
        assert_eq!(
            savings_rows(db.pool()).await,
            [
                ("2026-09-01".to_string(), "a".to_string(), 40, 1),
                ("2026-09-01".to_string(), "b".to_string(), 40, 1),
            ]
        );
        // Removing a library removes its share.
        sqlx::query("DELETE FROM libraries WHERE id = 'b'")
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(savings_rows(db.pool()).await.len(), 1);
    }

    /// Errors as the version before 6 wrote them, and the kind each gets.
    const ROWS: &[(&str, Option<&str>, Option<&str>)] = &[
        (
            "a01",
            Some("The file is no longer at /m/a.mkv. It may have been moved."),
            Some("source_changed"),
        ),
        (
            "a02",
            Some("The original file is no longer there"),
            Some("source_changed"),
        ),
        (
            "a03",
            Some("The file no longer exists"),
            Some("source_changed"),
        ),
        (
            "b01",
            Some("The original file appears damaged or incomplete (it stops after 0.1 s)."),
            Some("unreadable_source"),
        ),
        (
            "b02",
            Some(
                "All 2 attempts failed. Last error: The original file appears damaged or \
                 incomplete (it stops after 0.1 s). It was left unchanged.",
            ),
            Some("unreadable_source"),
        ),
        (
            "b03",
            Some("Could not read the file: Permission denied (os error 13)"),
            Some("unreadable_source"),
        ),
        (
            "b04",
            Some(
                "Reading this file took longer than 60 seconds. It may be damaged, or the \
                 drive may be very slow.",
            ),
            Some("unreadable_source"),
        ),
        (
            "b05",
            Some("Szalinski doesn't have permission to read this file."),
            Some("unreadable_source"),
        ),
        (
            "b06",
            Some("This file can't be read as a video: its MKV header is damaged."),
            Some("unreadable_source"),
        ),
        (
            "c01",
            Some("The new file is shorter than the original (0.1 s instead of 8.0 s)"),
            Some("verification"),
        ),
        (
            "c02",
            Some(
                "All 2 attempts failed. Last error: The new file doesn't look like the \
                 original",
            ),
            Some("verification"),
        ),
        (
            "d01",
            Some("libx264 stopped with exit code 1: Invalid argument"),
            Some("encoder"),
        ),
        (
            "d02",
            Some("libsvtav1 finished but wrote no output"),
            Some("encoder"),
        ),
        (
            "e01",
            Some(
                "libx264 stopped with exit code 228: The disk ran out of space while writing \
                 the new file (No space left on device)",
            ),
            Some("disk_full"),
        ),
        (
            "e02",
            Some("Not enough free space in /temp for this file (needs about 1.1 GB)"),
            Some("disk_full"),
        ),
        (
            "e03",
            Some(
                "Could not put the new file in place, so the original was kept: No space left \
                 on device (os error 28)",
            ),
            Some("disk_full"),
        ),
        (
            "f01",
            Some("Could not use the temp folder /temp/work: File exists (os error 17)"),
            Some("work_folder"),
        ),
        (
            "g01",
            Some(
                "The NVIDIA GPU isn't available, and converting on the CPU instead is turned \
                 off, so this file wasn't converted.",
            ),
            Some("hardware_unavailable"),
        ),
        (
            "g02",
            Some("No working encoder was found for H.264. Check the hardware settings"),
            Some("hardware_unavailable"),
        ),
        (
            "h01",
            Some(
                "Szalinski doesn't have permission to write in /m/ro, so the converted file \
                 can't be put there (in Docker, the PUID/PGID user needs write access).",
            ),
            Some("destination"),
        ),
        (
            "h02",
            Some("A file named \"Clip.mkv\" already exists next to the original"),
            Some("destination"),
        ),
        (
            "h03",
            Some("A file named \"Clip.mkv\" already exists in the output folder"),
            Some("destination"),
        ),
        (
            "h04",
            Some("Output to a separate folder is on, but no output folder is set"),
            Some("destination"),
        ),
        (
            "h05",
            Some(
                "Could not put the new file in place, so the original was kept: Invalid \
                 cross-device link (os error 18)",
            ),
            Some("destination"),
        ),
        (
            "i01",
            Some("Could not start ffmpeg: No such file or directory (os error 2)"),
            Some("other"),
        ),
        ("j01", None, None),
    ];

    #[tokio::test]
    async fn version_6_sorts_recorded_errors_into_problem_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v5.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            migrate_to(&pool, 5).await.unwrap();
            sqlx::query(
                "INSERT INTO libraries (id, name, path, profile, created_at) \
                 VALUES ('l', 'Movies', '/m', '{}', '2026-01-01T00:00:00.000Z')",
            )
            .execute(&pool)
            .await
            .unwrap();
            for (i, (id, error, _)) in ROWS.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO files (id, library_id, path, relative_path, file_name, \
                     size_bytes, modified_at, status, error, scanned_at, updated_at) \
                     VALUES (?, 'l', ?, 'x', 'x', 1, 'x', ?, ?, 'x', 'x')",
                )
                .bind(id)
                .bind(format!("/m/{i}.mkv"))
                .bind(if error.is_some() { "failed" } else { "done" })
                .bind(error)
                .execute(&pool)
                .await
                .unwrap();
                sqlx::query(
                    "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, \
                     stage, error, created_at) VALUES (?, ?, 'l', 'x', 'x', ?, 'waiting', ?, \
                     '2026-01-02T00:00:00.000Z')",
                )
                .bind(format!("j{id}"))
                .bind(id)
                .bind(if error.is_some() { "failed" } else { "done" })
                .bind(error)
                .execute(&pool)
                .await
                .unwrap();
            }
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        let files: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT id, problem FROM files ORDER BY id")
                .fetch_all(db.pool())
                .await
                .unwrap();
        let expected: Vec<(String, Option<String>)> = ROWS
            .iter()
            .map(|(id, _, p)| ((*id).to_string(), p.map(str::to_string)))
            .collect();
        assert_eq!(files, expected);
        let jobs: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT substr(id, 2), problem FROM jobs ORDER BY id")
                .fetch_all(db.pool())
                .await
                .unwrap();
        assert_eq!(jobs, expected);
    }

    /// Portrait videos are relabelled like the same picture turned
    /// sideways; everything else keeps its label.
    #[tokio::test]
    async fn version_7_relabels_portrait_videos() {
        use szalinski_core::media::resolution_label;
        use szalinski_core::{ProbeInfo, StreamInfo, StreamKind};
        let video = |w: u32, h: u32, cover: bool| StreamInfo {
            kind: Some(StreamKind::Video),
            codec: "h264".into(),
            width: Some(w),
            height: Some(h),
            is_attached_pic: cover,
            ..Default::default()
        };
        let probe = |streams: Vec<StreamInfo>| {
            serde_json::to_string(&ProbeInfo {
                streams,
                ..Default::default()
            })
            .unwrap()
        };
        // (probe, label before, label after)
        let mut rows: Vec<(Option<String>, Option<String>, Option<String>)> = Vec::new();
        // Every class boundary, turned on its side.
        for (w, h) in [
            (7680, 4320),
            (6999, 4000),
            (3840, 2160),
            (3200, 1000),
            (2560, 1440),
            (2300, 900),
            (1920, 1080),
            (1920, 800),
            (1700, 900),
            (1280, 720),
            (1200, 500),
            (1024, 576),
            (1000, 400),
            (854, 480),
            (720, 470),
            (640, 360),
        ] {
            let old = [
                resolution_label(w, h),
                "1440p", // what 1080x1920 used to read as
            ];
            rows.push((
                Some(probe(vec![video(h, w, false)])),
                Some(old[1].to_string()),
                Some(resolution_label(w, h).to_string()),
            ));
            // Landscape pictures keep their label.
            rows.push((
                Some(probe(vec![video(w, h, false)])),
                Some(old[0].to_string()),
                Some(old[0].to_string()),
            ));
        }
        // Portrait cover art before a landscape picture: not the picture.
        rows.push((
            Some(probe(vec![video(600, 900, true), video(1280, 720, false)])),
            Some("720p".into()),
            Some("720p".into()),
        ));
        // Cover art ahead of the portrait picture is skipped.
        rows.push((
            Some(probe(vec![video(500, 500, true), video(1080, 1920, false)])),
            Some("1440p".into()),
            Some("1080p".into()),
        ));
        // No size, no probe, not JSON: unchanged.
        rows.push((
            Some(probe(vec![StreamInfo {
                kind: Some(StreamKind::Video),
                ..Default::default()
            }])),
            Some("SD".into()),
            Some("SD".into()),
        ));
        rows.push((None, None, None));
        rows.push((
            Some("not json".into()),
            Some("720p".into()),
            Some("720p".into()),
        ));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v6.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            migrate_to(&pool, 6).await.unwrap();
            sqlx::query(
                "INSERT INTO libraries (id, name, path, profile, created_at) \
                 VALUES ('l', 'Movies', '/m', '{}', '2026-01-01T00:00:00.000Z')",
            )
            .execute(&pool)
            .await
            .unwrap();
            for (i, (probe, before, _)) in rows.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO files (id, library_id, path, relative_path, file_name, \
                     size_bytes, modified_at, status, probe, resolution, scanned_at, \
                     updated_at) VALUES (?, 'l', ?, 'x', 'x', 1, 'x', 'done', ?, ?, 'x', 'x')",
                )
                .bind(format!("f{i:03}"))
                .bind(format!("/m/{i}.mkv"))
                .bind(probe)
                .bind(before)
                .execute(&pool)
                .await
                .unwrap();
            }
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        let got: Vec<Option<String>> =
            sqlx::query_scalar("SELECT resolution FROM files ORDER BY id")
                .fetch_all(db.pool())
                .await
                .unwrap();
        let expected: Vec<Option<String>> = rows.into_iter().map(|(_, _, after)| after).collect();
        assert_eq!(got, expected);
    }

    /// Version 9 adds the space a conversion freed (not guessed for old
    /// jobs) and the kind of problem on feed entries (taken from the failed
    /// job an old error entry refers to).
    #[tokio::test]
    async fn version_9_adds_freed_bytes_and_the_feed_problem() {
        const LIB: &str = "00000000-0000-0000-0000-00000000000a";
        const FILE: &str = "00000000-0000-0000-0000-00000000000b";
        const DONE: &str = "00000000-0000-0000-0000-0000000000d1";
        const FAILED: &str = "00000000-0000-0000-0000-0000000000f1";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v8.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            migrate_to(&pool, 8).await.unwrap();
            for sql in [
                format!(
                    "INSERT INTO libraries (id, name, path, profile, created_at) \
                     VALUES ('{LIB}', 'Movies', '/m', '{{}}', '2026-01-01T00:00:00.000Z')"
                ),
                format!(
                    "INSERT INTO files (id, library_id, path, relative_path, file_name, \
                     size_bytes, modified_at, status, scanned_at, updated_at) VALUES \
                     ('{FILE}', '{LIB}', '/m/a.mkv', 'a.mkv', 'a.mkv', 1, 'x', 'failed', 'x', 'x')"
                ),
                format!(
                    "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, \
                     stage, input_size, output_size, created_at) VALUES ('{DONE}', '{FILE}', \
                     '{LIB}', 'a.mkv', '/m/a.mkv', 'done', 'finalizing', 100, 60, \
                     '2026-01-02T00:00:00.000Z')"
                ),
                format!(
                    "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, \
                     stage, error, problem, created_at) VALUES ('{FAILED}', '{FILE}', '{LIB}', \
                     'a.mkv', '/m/a.mkv', 'failed', 'waiting', 'Not enough free space', \
                     'disk_full', '2026-01-03T00:00:00.000Z')"
                ),
                format!(
                    "INSERT INTO activity (at, level, message, job_id) VALUES \
                     ('2026-01-03T00:00:00.000Z', 'error', 'Failed a.mkv', '{FAILED}'), \
                     ('2026-01-03T00:00:00.000Z', 'info', 'Something else', '{FAILED}'), \
                     ('2026-01-03T00:00:00.000Z', 'error', 'Failed with no job', NULL), \
                     ('2026-01-03T00:00:00.000Z', 'error', 'The job is done', '{DONE}')"
                ),
            ] {
                sqlx::query(&sql).execute(&pool).await.unwrap();
            }
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        // Conversions finished before keep no number.
        let (jobs, _) = crate::db::jobs::list(db.pool(), crate::db::jobs::JobFilter::All, 10, 0)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 2);
        assert!(jobs.iter().all(|j| j.freed_bytes.is_none()), "{jobs:?}");
        let feed = crate::db::activity::list(db.pool(), 10, None)
            .await
            .unwrap();
        let problems: Vec<(&str, Option<szalinski_core::ProblemKind>)> = feed
            .iter()
            .rev()
            .map(|e| (e.message.as_str(), e.problem))
            .collect();
        assert_eq!(
            problems,
            [
                ("Failed a.mkv", Some(szalinski_core::ProblemKind::DiskFull)),
                ("Something else", None),
                ("Failed with no job", None),
                ("The job is done", None),
            ]
        );
    }

    /// Version 10 records the goal each pending or skipped verdict was
    /// decided with: the library's current goal, except a size-rule skip in
    /// a library whose goal has no size rule (an earlier goal's verdict,
    /// left unknown so the next scan decides it again).
    #[tokio::test]
    async fn version_10_records_the_goal_of_each_verdict() {
        const NO_RULE: &str = "00000000-0000-0000-0000-0000000000a1";
        const RULE: &str = "00000000-0000-0000-0000-0000000000a2";
        let no_rule_profile = r#"{"goal":"compatible","min_savings_pct":null}"#;
        let rule_profile = r#"{"goal":"save_space","min_savings_pct":10}"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v9.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            migrate_to(&pool, 9).await.unwrap();
            for (id, name, profile) in [
                (NO_RULE, "Kids", no_rule_profile),
                (RULE, "Movies", rule_profile),
            ] {
                sqlx::query(
                    "INSERT INTO libraries (id, name, path, profile, created_at) \
                     VALUES (?, ?, ?, ?, '2026-01-01T00:00:00.000Z')",
                )
                .bind(id)
                .bind(name)
                .bind(format!("/{name}"))
                .bind(profile)
                .execute(&pool)
                .await
                .unwrap();
            }
            // (file, library, status, skip reason, the job that skipped it
            // by the size rule)
            let files: [(&str, &str, &str, Option<&str>, bool); 6] = [
                (
                    "size",
                    NO_RULE,
                    "skipped",
                    Some("Only 4% smaller — kept the original"),
                    true,
                ),
                ("plain", NO_RULE, "skipped", Some("Already H.264"), false),
                ("wait", NO_RULE, "pending", None, false),
                ("done", NO_RULE, "done", None, false),
                (
                    "kept",
                    RULE,
                    "skipped",
                    Some("Only 4% smaller — kept the original"),
                    true,
                ),
                ("user", RULE, "skipped", Some("Skipped by you"), false),
            ];
            for (n, (name, lib, status, reason, size_rule)) in files.into_iter().enumerate() {
                let file_id = format!("00000000-0000-0000-0000-00000000010{n}");
                let job_id = format!("00000000-0000-0000-0000-00000000020{n}");
                sqlx::query(
                    "INSERT INTO files (id, library_id, path, relative_path, file_name, \
                     size_bytes, modified_at, status, skip_reason, job_id, scanned_at, \
                     updated_at) VALUES (?, ?, ?, ?, ?, 1, 'x', ?, ?, ?, 'x', 'x')",
                )
                .bind(&file_id)
                .bind(lib)
                .bind(format!("/{lib}/{name}.mkv"))
                .bind(format!("{name}.mkv"))
                .bind(format!("{name}.mkv"))
                .bind(status)
                .bind(reason)
                .bind(size_rule.then_some(&job_id))
                .execute(&pool)
                .await
                .unwrap();
                if size_rule {
                    sqlx::query(
                        "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, \
                         state, stage, output_size, skip_reason, created_at) VALUES \
                         (?, ?, ?, 'f.mkv', '/f.mkv', 'skipped', 'waiting', 96, ?, \
                         '2026-01-02T00:00:00.000Z')",
                    )
                    .bind(&job_id)
                    .bind(&file_id)
                    .bind(lib)
                    .bind(reason)
                    .execute(&pool)
                    .await
                    .unwrap();
                }
            }
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        let rows: Vec<(String, Option<String>)> =
            sqlx::query_as("SELECT file_name, verdict_profile FROM files ORDER BY rowid")
                .fetch_all(db.pool())
                .await
                .unwrap();
        let verdicts: Vec<(&str, Option<&str>)> = rows
            .iter()
            .map(|(name, v)| (name.as_str(), v.as_deref()))
            .collect();
        assert_eq!(
            verdicts,
            [
                ("size.mkv", None),
                ("plain.mkv", Some(no_rule_profile)),
                ("wait.mkv", Some(no_rule_profile)),
                ("done.mkv", None),
                ("kept.mkv", Some(rule_profile)),
                ("user.mkv", Some(rule_profile)),
            ]
        );
        // Jobs from before don't know their goal.
        let goals: Vec<Option<String>> = sqlx::query_scalar("SELECT profile FROM jobs")
            .fetch_all(db.pool())
            .await
            .unwrap();
        assert_eq!(goals, [None, None]);
    }

    /// Version 12 adds the mount points of the folders in use, none known
    /// yet; what was there is kept.
    #[tokio::test]
    async fn version_12_adds_the_mount_points_of_folders() {
        const LIB: &str = "00000000-0000-0000-0000-0000000000a1";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v11.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            migrate_to(&pool, 11).await.unwrap();
            sqlx::query(
                "INSERT INTO libraries (id, name, path, profile, created_at) \
                 VALUES (?, 'Movies', '/mnt/nas/Movies', '{}', '2026-01-01T00:00:00.000Z')",
            )
            .bind(LIB)
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO library_mounts (library_id, path) VALUES (?, ?)")
                .bind(LIB)
                .bind("/mnt/nas/Movies/USB")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        let known: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM folder_mounts")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(known, 0);
        let inner = crate::db::libraries::mounts(db.pool(), uuid::Uuid::parse_str(LIB).unwrap())
            .await
            .unwrap();
        assert_eq!(inner, ["/mnt/nas/Movies/USB"]);
        let mut tx = db.write_tx().await.unwrap();
        let nas = crate::db::folder_mounts::FolderMount {
            mount: "/mnt/nas".to_string(),
            identity: None,
        };
        crate::db::folder_mounts::add(&mut tx, "/mnt/nas/Movies", std::slice::from_ref(&nas))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            crate::db::folder_mounts::get(db.pool(), "/mnt/nas/Movies")
                .await
                .unwrap(),
            [nas]
        );
    }

    /// Version 13 keeps the mount points remembered before, with nothing
    /// noted about what was mounted there; the first look at one notes it,
    /// and later looks don't change it.
    #[tokio::test]
    async fn version_13_notes_what_is_mounted_at_remembered_mount_points() {
        use crate::db::folder_mounts::{self, FolderMount};
        use szalinski_worker::slow_fs::MountIdentity;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v12.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            migrate_to(&pool, 12).await.unwrap();
            sqlx::query("INSERT INTO folder_mounts (folder, mount) VALUES (?, ?)")
                .bind("/out")
                .bind("/out")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        let unknown = FolderMount {
            mount: "/out".to_string(),
            identity: None,
        };
        assert_eq!(
            folder_mounts::get(db.pool(), "/out").await.unwrap(),
            [unknown]
        );
        let share = MountIdentity {
            fstype: "nfs4".to_string(),
            source: "nas:/export".to_string(),
            root: "/".to_string(),
        };
        let other = MountIdentity {
            fstype: "tmpfs".to_string(),
            source: "tmpfs".to_string(),
            root: "/".to_string(),
        };
        for seen in [&share, &other] {
            let mut tx = db.write_tx().await.unwrap();
            let m = FolderMount {
                mount: "/out".to_string(),
                identity: Some(seen.clone()),
            };
            folder_mounts::add(&mut tx, "/out", &[m]).await.unwrap();
            tx.commit().await.unwrap();
        }
        let noted = folder_mounts::get(db.pool(), "/out").await.unwrap();
        assert_eq!(noted[0].identity.as_ref(), Some(&share));
        // Told to use the other one.
        let mut tx = db.write_tx().await.unwrap();
        folder_mounts::set_identity(&mut tx, "/out", "/out", &other)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let noted = folder_mounts::get(db.pool(), "/out").await.unwrap();
        assert_eq!(noted[0].identity.as_ref(), Some(&other));
        let final_mount: Option<String> =
            sqlx::query_scalar("SELECT final_mount FROM jobs LIMIT 1")
                .fetch_optional(db.pool())
                .await
                .unwrap()
                .flatten();
        assert_eq!(final_mount, None);
    }

    /// Version 14 keeps the jobs recorded before, with no attempts (they
    /// weren't recorded) and no way of working out progress noted; a job
    /// finished afterwards keeps its attempts, and a running one says how
    /// its progress was worked out.
    #[tokio::test]
    async fn version_14_adds_attempt_history_and_progress_basis() {
        use szalinski_core::{
            AttemptResult, HwApi, JobAttempt, JobProgress, JobStage, JobState, ProblemKind,
            ProgressBasis,
        };
        const LIB: &str = "00000000-0000-0000-0000-00000000000a";
        const FILE: &str = "00000000-0000-0000-0000-00000000000b";
        const OLD: &str = "00000000-0000-0000-0000-0000000000f1";
        const RUNNING: &str = "00000000-0000-0000-0000-0000000000f2";
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v13.db");
        {
            let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
                .await
                .unwrap();
            migrate_to(&pool, 13).await.unwrap();
            for sql in [
                format!(
                    "INSERT INTO libraries (id, name, path, profile, created_at) \
                     VALUES ('{LIB}', 'Movies', '/m', '{{}}', '2026-01-01T00:00:00.000Z')"
                ),
                format!(
                    "INSERT INTO files (id, library_id, path, relative_path, file_name, \
                     size_bytes, modified_at, status, scanned_at, updated_at) VALUES \
                     ('{FILE}', '{LIB}', '/m/a.mkv', 'a.mkv', 'a.mkv', 1, 'x', 'failed', 'x', 'x')"
                ),
                format!(
                    "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, \
                     stage, error, problem, attempt, created_at) VALUES ('{OLD}', '{FILE}', \
                     '{LIB}', 'a.mkv', '/m/a.mkv', 'failed', 'verifying', 'None of the 3 ways', \
                     'verification', 3, '2026-01-03T00:00:00.000Z')"
                ),
                format!(
                    "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, \
                     stage, attempt, created_at, started_at) VALUES ('{RUNNING}', '{FILE}', \
                     '{LIB}', 'a.mkv', '/m/a.mkv', 'running', 'transcoding', 3, \
                     '2026-01-04T00:00:00.000Z', '2026-01-04T00:00:00.000Z')"
                ),
            ] {
                sqlx::query(&sql).execute(&pool).await.unwrap();
            }
            pool.close().await;
        }
        let db = Db::open(&path).await.unwrap();
        let v: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(v, 14);
        let old = crate::db::jobs::get(db.pool(), OLD.parse().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(old.attempts.is_empty());
        assert_eq!(
            (old.progress_basis, old.frames, old.elapsed_secs),
            (None, None, None)
        );
        assert_eq!(old.attempt, 3);

        // A running job notes how its progress was worked out, and the
        // attempts that ended so far.
        let running: uuid::Uuid = RUNNING.parse().unwrap();
        let failed = JobAttempt {
            attempt: 1,
            encoder: "hevc_vaapi".into(),
            hw_api: HwApi::Vaapi,
            device: Some("/dev/dri/renderD128".into()),
            hw_decode: true,
            elapsed_secs: 121.5,
            result: AttemptResult::Failed,
            error: Some("The new file doesn't play start to finish".into()),
            problem: Some(ProblemKind::Verification),
            failed_check: None,
            command: Some("ffmpeg -hwaccel vaapi …".into()),
            log_tail: None,
        };
        let progress = JobProgress {
            job_id: running,
            file_id: FILE.parse().unwrap(),
            stage: JobStage::Transcoding,
            progress: 0.0,
            fps: Some(3.7),
            speed: None,
            eta_secs: None,
            progress_basis: Some(ProgressBasis::Unknown),
            frames: Some(366),
            elapsed_secs: Some(99),
            encoder: Some("libx265".into()),
            hw_api: Some(HwApi::Software),
            attempt: 3,
            attempts: Some(vec![failed.clone()]),
        };
        crate::db::jobs::update_progress(db.pool(), &progress)
            .await
            .unwrap();
        // A later update without them keeps them.
        let later = JobProgress {
            attempts: None,
            frames: Some(400),
            ..progress
        };
        crate::db::jobs::update_progress(db.pool(), &later)
            .await
            .unwrap();
        let job = crate::db::jobs::get(db.pool(), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.progress_basis, Some(ProgressBasis::Unknown));
        assert_eq!((job.frames, job.elapsed_secs), (Some(400), Some(99)));
        assert_eq!(job.attempts, std::slice::from_ref(&failed));

        // Finished: the outcome's attempts are kept, the live fields go.
        let worked = JobAttempt {
            attempt: 3,
            encoder: "libx265".into(),
            hw_api: HwApi::Software,
            device: None,
            hw_decode: false,
            elapsed_secs: 400.0,
            result: AttemptResult::Succeeded,
            error: None,
            problem: None,
            failed_check: None,
            command: Some("ffmpeg …".into()),
            log_tail: None,
        };
        let mut tx = db.write_tx().await.unwrap();
        crate::db::jobs::finish(
            &mut tx,
            running,
            JobState::Done,
            &crate::db::jobs::JobFinish {
                attempts: Some(vec![failed.clone(), worked.clone()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let job = crate::db::jobs::get(db.pool(), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(job.attempts, [failed, worked]);
        assert_eq!(
            (job.progress_basis, job.frames, job.elapsed_secs),
            (None, None, None)
        );
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
