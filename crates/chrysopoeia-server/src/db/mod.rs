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

use std::path::Path;
use std::time::Duration;

use anyhow::Context;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};
use uuid::Uuid;

/// Name of the database file inside the data directory.
pub const DB_FILE_NAME: &str = "chrysopoeia.db";

/// A write transaction.
pub type Tx = Transaction<'static, Sqlite>;

/// Connection pool plus helpers.
#[derive(Debug, Clone)]
pub struct Db {
    pool: SqlitePool,
}

impl Db {
    /// Open (creating if needed) the database at `path` and run migrations.
    pub async fn open(path: &Path) -> anyhow::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .max_connections(8)
            .min_connections(1)
            .acquire_timeout(Duration::from_secs(30))
            .connect_with(options)
            .await
            .with_context(|| {
                format!(
                    "Could not open the database at {}. Check that the data folder is writable \
                     (in Docker: the /config volume and PUID/PGID)",
                    path.display()
                )
            })?;
        migrate::migrate(&pool).await?;
        Ok(Self { pool })
    }

    /// The underlying pool, for reads and single-statement writes.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Begin a write transaction (`BEGIN IMMEDIATE`).
    pub async fn write_tx(&self) -> sqlx::Result<Tx> {
        self.pool.begin_with("BEGIN IMMEDIATE").await
    }

    /// Close every connection (checkpointing the WAL).
    pub async fn close(&self) {
        self.pool.close().await;
    }
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
}
