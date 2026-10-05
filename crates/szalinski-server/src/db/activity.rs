//! The `activity` table: the feed of plain-language events.

use chrono::Utc;
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqlitePool};
use szalinski_core::{ActivityEntry, ActivityLevel, ProblemKind};
use uuid::Uuid;

use super::{enum_str, opt_uuid_col, parse_enum, problem_of, ts, ts_col};

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
    let problem: Option<String> = row.try_get("problem")?;
    Ok(ActivityEntry {
        // A kind this version doesn't know reads as none.
        problem: problem
            .as_deref()
            .and_then(|p| parse_enum::<ProblemKind>(p).ok()),
        id: row.try_get("id")?,
        at: ts_col(row, "at")?,
        level: parse_enum(&level)?,
        message: row.try_get("message")?,
        file_id: opt_uuid_col(row, "file_id")?,
        job_id: opt_uuid_col(row, "job_id")?,
        library_id: opt_uuid_col(row, "library_id")?,
    })
}

/// The kind of problem an entry is about: the problem of the failed job it
/// refers to or, for an entry about a file without a job, of the file when
/// that is failed. Entries about anything else (a conversion that worked, a
/// file waiting in the queue, a whole library) have none, nor do the
/// success entries. Read when the entry is written, so the entry keeps the
/// kind the failure had then.
async fn problem_for(
    pool: &SqlitePool,
    level: ActivityLevel,
    refs: &ActivityRefs,
) -> sqlx::Result<Option<ProblemKind>> {
    if level == ActivityLevel::Success {
        return Ok(None);
    }
    let (sql, id) = match (refs.job_id, refs.file_id) {
        (Some(job), _) => (
            "SELECT error, problem FROM jobs WHERE id = ? AND state IN ('failed', 'skipped')",
            job,
        ),
        (None, Some(file)) => (
            "SELECT error, problem FROM files WHERE id = ? AND status IN ('failed', 'skipped')",
            file,
        ),
        (None, None) => return Ok(None),
    };
    let row = sqlx::query(sql)
        .bind(id.to_string())
        .fetch_optional(pool)
        .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let error: Option<String> = row.try_get("error")?;
    let problem: Option<String> = row.try_get("problem")?;
    Ok(problem_of(error.as_deref(), problem.as_deref()))
}

/// Append an entry and trim the oldest beyond [`KEEP_ROWS`].
pub async fn insert(
    pool: &SqlitePool,
    level: ActivityLevel,
    message: &str,
    refs: ActivityRefs,
) -> sqlx::Result<ActivityEntry> {
    let at = Utc::now();
    let problem = problem_for(pool, level, &refs).await?;
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO activity (at, level, message, file_id, job_id, library_id, problem) \
         VALUES (?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(ts(at))
    .bind(enum_str(&level))
    .bind(message)
    .bind(refs.file_id.map(|u| u.to_string()))
    .bind(refs.job_id.map(|u| u.to_string()))
    .bind(refs.library_id.map(|u| u.to_string()))
    .bind(problem.map(|p| enum_str(&p)))
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
        problem,
    })
}

/// Newest entries first, optionally older than `before` (an id).
pub async fn list(
    pool: &SqlitePool,
    limit: u32,
    before: Option<i64>,
) -> sqlx::Result<Vec<ActivityEntry>> {
    let rows = sqlx::query(
        "SELECT id, at, level, message, file_id, job_id, library_id, problem FROM activity \
         WHERE id < ? ORDER BY id DESC LIMIT ?",
    )
    .bind(before.unwrap_or(i64::MAX))
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;
    rows.iter().map(from_row).collect()
}
