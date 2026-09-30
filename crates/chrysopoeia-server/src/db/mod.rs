//! SQLite storage (WAL mode) for settings, libraries, files, jobs, activity
//! and daily savings.
//!
//! All queries are runtime queries. Timestamps are stored as RFC 3339 UTC
//! strings with millisecond precision (so they sort as text) and UUIDs as
//! hyphenated lowercase strings. Write transactions start with
//! `BEGIN IMMEDIATE` so concurrent writers wait on the busy timeout instead of
//! failing on a lock upgrade.

pub mod activity;
pub mod files;
pub mod jobs;
pub mod libraries;
pub mod migrate;
pub mod settings;
pub mod stats;

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use chrysopoeia_core::ProblemKind;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
};
use sqlx::{Connection, Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

/// Name of the database file inside the data directory.
pub const DB_FILE_NAME: &str = "chrysopoeia.db";

/// A write transaction.
pub type Tx = Transaction<'static, Sqlite>;

/// How long a writer waits for another writer before giving up. Generous,
/// because Unraid appdata often lives on spinning disks; long operations
/// commit in chunks so no single write holds the lock for anywhere near this.
pub const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

/// Connection pool plus helpers.
#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Open (creating if needed) the database at `path` and run migrations.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        Self::open_with(path, DEFAULT_BUSY_TIMEOUT).await
    }

    /// [`Db::open`] with a specific busy timeout (tests shorten it).
    pub async fn open_with(path: &Path, busy_timeout: Duration) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(busy_timeout);
        let pool = match pool_options(busy_timeout).connect_with(options).await {
            Ok(pool) => pool,
            Err(e) => {
                let e = anyhow::Error::new(e);
                if is_damaged(&e) {
                    return Err(damaged(path, &e));
                }
                return Err(e.context(format!(
                    "Could not open the database at {}. Check that the data folder is writable \
                     (in Docker: the /config volume and PUID/PGID)",
                    path.display()
                )));
            }
        };
        if let Err(e) = migrate::migrate(&pool).await {
            pool.close().await;
            if is_damaged(&e) {
                return Err(damaged(path, &e));
            }
            return Err(e);
        }
        Ok(Self { pool })
    }

    /// The underlying pool, for reads and single-statement writes.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Begin a write transaction (`BEGIN IMMEDIATE`).
    ///
    /// sqlx refuses a `BEGIN` on a connection it still counts as inside a
    /// transaction. The pool never hands such a connection out (see
    /// [`pool_options`]); should one slip through anyway, it is dropped
    /// (and closed) and another one is used.
    pub async fn write_tx(&self) -> sqlx::Result<Tx> {
        let mut attempts = 0;
        loop {
            match self.pool.begin_with("BEGIN IMMEDIATE").await {
                Err(sqlx::Error::InvalidSavePointStatement | sqlx::Error::BeginFailed)
                    if attempts < 3 =>
                {
                    attempts += 1;
                    tracing::warn!("a database connection was left in a bad state; using another");
                }
                result => return result,
            }
        }
    }

    /// Close every connection (checkpointing the WAL).
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

/// Pool settings. A connection whose transaction ended badly is closed
/// instead of being reused: when a write fails because the disk is full (or
/// on an I/O error), SQLite rolls the transaction back by itself, so the
/// `COMMIT` and the `ROLLBACK` sqlx sends afterwards both fail and sqlx keeps
/// counting the connection as inside a transaction. Every later `BEGIN` on
/// it would fail, long after the disk has room again.
fn pool_options(busy_timeout: Duration) -> SqlitePoolOptions {
    SqlitePoolOptions::new()
        .max_connections(8)
        .min_connections(1)
        .acquire_timeout(busy_timeout.max(Duration::from_secs(30)))
        .after_release(|conn, _meta| {
            Box::pin(async move {
                // The ping is handled after the rollback a dropped
                // transaction queued, so the depth read next is final.
                conn.ping().await?;
                Ok(!conn.is_in_transaction())
            })
        })
        .before_acquire(|conn, _meta| Box::pin(async move { Ok(!conn.is_in_transaction()) }))
}

/// The database file is damaged: SQLite finds it malformed, or not a
/// database at all. Start-up moves it aside and begins a new one.
#[derive(Debug, thiserror::Error)]
#[error("The database at {} is damaged ({detail})", path.display())]
pub struct DamagedDatabase {
    /// The damaged file.
    pub path: PathBuf,
    /// SQLite's description, for the log.
    pub detail: String,
}

fn damaged(path: &Path, e: &anyhow::Error) -> anyhow::Error {
    let detail = e
        .chain()
        .find_map(|c| c.downcast_ref::<sqlx::Error>())
        .map_or_else(|| format!("{e:#}"), ToString::to_string);
    anyhow::Error::new(DamagedDatabase {
        path: path.to_path_buf(),
        detail,
    })
}

/// Whether an error says the database file itself is damaged
/// (`SQLITE_CORRUPT` or `SQLITE_NOTADB`), not unreadable or busy.
pub fn is_damaged(e: &anyhow::Error) -> bool {
    e.chain()
        .filter_map(|c| c.downcast_ref::<sqlx::Error>())
        .any(|e| match e {
            sqlx::Error::Database(d) => d
                .code()
                .and_then(|c| c.parse::<i32>().ok())
                .is_some_and(|c| matches!(c & 0xff, 11 | 26)),
            _ => false,
        })
}

/// Move a damaged database file (and its WAL and shared-memory files) aside
/// as `<name>.damaged-<UTC time>`, so a new database can be started. Returns
/// where the database file went.
pub fn move_damaged_aside(path: &Path) -> anyhow::Result<PathBuf> {
    let name = path
        .file_name()
        .map_or_else(|| DB_FILE_NAME.to_string(), |n| n.to_string_lossy().into_owned());
    let aside_name = format!("{name}.damaged-{}", Utc::now().format("%Y%m%d-%H%M%S"));
    let aside = path.with_file_name(&aside_name);
    std::fs::rename(path, &aside).with_context(|| {
        format!(
            "The database at {} is damaged and could not be moved aside. Stop Chrysopoeia, \
             rename or delete that file (your media files are not affected by this), and start \
             it again",
            path.display()
        )
    })?;
    for suffix in ["-wal", "-shm"] {
        let side = path.with_file_name(format!("{name}{suffix}"));
        if side.exists() {
            let to = path.with_file_name(format!("{aside_name}{suffix}"));
            if let Err(e) = std::fs::rename(&side, &to) {
                tracing::warn!("could not move {} aside: {e}", side.display());
                let _ = std::fs::remove_file(&side);
            }
        }
    }
    Ok(aside)
}

/// Format a timestamp for storage.
pub fn ts(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// The current time, formatted for storage.
pub fn now_ts() -> String {
    ts(Utc::now())
}

/// A decode error for values that don't parse.
pub fn decode_error(message: impl Into<String>) -> sqlx::Error {
    sqlx::Error::Decode(message.into().into())
}

/// Parse a stored timestamp.
pub fn parse_ts(value: &str) -> sqlx::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| decode_error(format!("bad timestamp {value:?}: {e}")))
}

/// Serialize a unit enum to its serde name (e.g. `JobState::Queued` → `queued`).
pub fn enum_str<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// Parse a unit enum from its serde name.
pub fn parse_enum<T: DeserializeOwned>(value: &str) -> sqlx::Result<T> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|e| decode_error(format!("bad enum value {value:?}: {e}")))
}

/// The kind of problem behind a stored `error`: the stored `problem` when
/// there is an error (`other` when a row has none or one this version
/// doesn't know), and none without an error, so the two always go together.
pub fn problem_of(error: Option<&str>, problem: Option<&str>) -> Option<ProblemKind> {
    error?;
    Some(
        problem
            .and_then(|p| parse_enum::<ProblemKind>(p).ok())
            .unwrap_or(ProblemKind::Other),
    )
}

/// Parse a JSON column.
pub fn parse_json<T: DeserializeOwned>(value: &str) -> sqlx::Result<T> {
    serde_json::from_str(value).map_err(|e| decode_error(format!("bad JSON column: {e}")))
}

/// Serialize a value for a JSON column.
pub fn to_json<T: Serialize>(value: &T) -> sqlx::Result<String> {
    serde_json::to_string(value).map_err(|e| sqlx::Error::Encode(Box::new(e)))
}

/// Read a UUID column.
pub fn uuid_col(row: &SqliteRow, column: &str) -> sqlx::Result<Uuid> {
    let s: String = row.try_get(column)?;
    Uuid::parse_str(&s).map_err(|e| decode_error(format!("bad id {s:?}: {e}")))
}

/// Read a nullable UUID column.
pub fn opt_uuid_col(row: &SqliteRow, column: &str) -> sqlx::Result<Option<Uuid>> {
    let s: Option<String> = row.try_get(column)?;
    s.map(|s| Uuid::parse_str(&s).map_err(|e| decode_error(format!("bad id {s:?}: {e}"))))
        .transpose()
}

/// Read a timestamp column.
pub fn ts_col(row: &SqliteRow, column: &str) -> sqlx::Result<DateTime<Utc>> {
    let s: String = row.try_get(column)?;
    parse_ts(&s)
}

/// Read a nullable timestamp column.
pub fn opt_ts_col(row: &SqliteRow, column: &str) -> sqlx::Result<Option<DateTime<Utc>>> {
    let s: Option<String> = row.try_get(column)?;
    s.as_deref().map(parse_ts).transpose()
}

/// Read a non-negative integer column as `u64`.
pub fn u64_col(row: &SqliteRow, column: &str) -> sqlx::Result<u64> {
    let v: i64 = row.try_get(column)?;
    Ok(u64::try_from(v).unwrap_or(0))
}

/// Read a nullable non-negative integer column as `u64`.
pub fn opt_u64_col(row: &SqliteRow, column: &str) -> sqlx::Result<Option<u64>> {
    let v: Option<i64> = row.try_get(column)?;
    Ok(v.map(|v| u64::try_from(v).unwrap_or(0)))
}

/// Whether a database error is temporary (another writer holds the lock, a
/// full disk or an I/O hiccup), so the operation is worth retrying.
pub fn is_transient(e: &sqlx::Error) -> bool {
    match e {
        sqlx::Error::PoolTimedOut | sqlx::Error::Io(_) => true,
        sqlx::Error::Database(d) => d
            .code()
            .and_then(|c| c.parse::<i32>().ok())
            // Primary result codes: BUSY, LOCKED, IOERR, FULL.
            .is_some_and(|c| matches!(c & 0xff, 5 | 6 | 10 | 13)),
        _ => false,
    }
}

/// Convert a byte count for storage.
pub fn i64_of(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Escape `%`, `_` and `\` for a `LIKE ... ESCAPE '\'` pattern.
pub fn like_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    for c in value.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrysopoeia_core::JobState;

    #[test]
    fn timestamps_roundtrip_and_sort() {
        let a = Utc::now();
        let s = ts(a);
        assert!(s.ends_with('Z'));
        let back = parse_ts(&s).unwrap();
        assert_eq!(back.timestamp_millis(), a.timestamp_millis());
    }

    #[test]
    fn enums_use_serde_names() {
        assert_eq!(enum_str(&JobState::Queued), "queued");
        let s: JobState = parse_enum("cancelled").unwrap();
        assert_eq!(s, JobState::Cancelled);
        assert!(parse_enum::<JobState>("nope").is_err());
    }

    #[test]
    fn like_escaping() {
        assert_eq!(like_escape("50%_off\\"), "50\\%\\_off\\\\");
    }

    /// A write that fails because the disk is full makes SQLite roll the
    /// transaction back by itself; the connection must not come back to
    /// the pool still counted as inside a transaction, or every later write
    /// fails ("begin_with at non-zero transaction depth") until a restart.
    #[tokio::test]
    async fn writes_work_again_after_the_disk_was_full() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("t.db")).await.unwrap();
        sqlx::query("CREATE TABLE big (x BLOB)")
            .execute(db.pool())
            .await
            .unwrap();
        for _ in 0..3 {
            let mut tx = db.write_tx().await.unwrap();
            // This connection may not grow the file: the disk is "full".
            let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            sqlx::query(&format!("PRAGMA max_page_count = {pages}"))
                .execute(&mut *tx)
                .await
                .unwrap();
            let full = sqlx::query("INSERT INTO big VALUES (zeroblob(200000))")
                .execute(&mut *tx)
                .await
                .unwrap_err();
            assert!(is_transient(&full), "{full}");
            // SQLite already rolled back, so the commit fails too.
            assert!(tx.commit().await.is_err());
            // Let the pool take the connection back.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // The disk has room again: every write works.
        for i in 0..20 {
            let mut tx = db
                .write_tx()
                .await
                .unwrap_or_else(|e| panic!("write {i} could not start: {e}"));
            sqlx::query("INSERT INTO big VALUES (zeroblob(10))")
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM big")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(rows, 20);
    }

    #[tokio::test]
    async fn a_damaged_database_is_recognised_and_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DB_FILE_NAME);
        {
            let db = Db::open(&path).await.unwrap();
            db.close().await;
        }
        // Garble the header: SQLite no longer sees a database there.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[..16].copy_from_slice(b"not a database!!");
        std::fs::write(&path, &bytes).unwrap();
        std::fs::write(dir.path().join(format!("{DB_FILE_NAME}-wal")), b"junk").unwrap();

        let err = Db::open(&path).await.unwrap_err();
        assert!(err.downcast_ref::<DamagedDatabase>().is_some(), "{err:#}");
        let aside = move_damaged_aside(&path).unwrap();
        assert!(!path.exists());
        assert!(aside.exists());
        assert!(
            aside
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("chrysopoeia.db.damaged-")
        );
        assert!(!dir.path().join(format!("{DB_FILE_NAME}-wal")).exists());
        // A new database starts where the damaged one was.
        Db::open(&path).await.unwrap();

        // A folder that can't be written is not "damaged".
        let missing = dir.path().join("no/such/folder/t.db");
        let err = Db::open(&missing).await.unwrap_err();
        assert!(err.downcast_ref::<DamagedDatabase>().is_none());
        assert!(format!("{err:#}").contains("Check that the data folder is writable"));
    }
}
