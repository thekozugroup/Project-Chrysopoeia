//! The `activity` table: the feed of plain-language events.

use chrono::Utc;
use chrysopoeia_core::{ActivityEntry, ActivityLevel};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

use super::{enum_str, opt_uuid_col, parse_enum, ts, ts_col};

/// Rows kept; older ones are trimmed on insert.
pub const KEEP_ROWS: i64 = 5_000;

/// Entity ids an activity entry refers to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ActivityRefs {
    pub file_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
}

fn from_row(row: &SqliteRow) -> sqlx::Result<ActivityEntry> {
    let level: String = row.try_get("level")?;
    Ok(ActivityEntry {
        id: row.try_get("id")?,
        at: ts_col(row, "at")?,
        level: parse_enum(&level)?,
        message: row.try_get("message")?,
        file_id: opt_uuid_col(row, "file_id")?,
        job_id: opt_uuid_col(row, "job_id")?,
        library_id: opt_uuid_col(row, "library_id")?,
    })
}

/// Append an entry and trim the oldest beyond [`KEEP_ROWS`].
pub async fn insert(
    pool: &SqlitePool,
    level: ActivityLevel,
    message: &str,
    refs: ActivityRefs,
) -> sqlx::Result<ActivityEntry> {
    let at = Utc::now();
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO activity (at, level, message, file_id, job_id, library_id) \
         VALUES (?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(ts(at))
    .bind(enum_str(&level))
    .bind(message)
    .bind(refs.file_id.map(|u| u.to_string()))
    .bind(refs.job_id.map(|u| u.to_string()))
    .bind(refs.library_id.map(|u| u.to_string()))
    .fetch_one(pool)
    .await?;
    // Ids are monotonic (AUTOINCREMENT), so this is a primary-key range delete.
    if id > KEEP_ROWS {
        sqlx::query("DELETE FROM activity WHERE id <= ?")
            .bind(id - KEEP_ROWS)
            .execute(pool)
            .await?;
    }
    Ok(ActivityEntry {
        id,
        at,
        level,
        message: message.to_string(),
        file_id: refs.file_id,
        job_id: refs.job_id,
        library_id: refs.library_id,
    })
}

/// Newest entries first, optionally older than `before` (an id).
pub async fn list(
    pool: &SqlitePool,
    limit: u32,
    before: Option<i64>,
) -> sqlx::Result<Vec<ActivityEntry>> {
    let rows = sqlx::query(
        "SELECT id, at, level, message, file_id, job_id, library_id FROM activity \
         WHERE id < ? ORDER BY id DESC LIMIT ?",
    )
    .bind(before.unwrap_or(i64::MAX))
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;
    rows.iter().map(from_row).collect()
}
