//! Aggregates: per-library stats, overview buckets and daily savings.

use std::collections::HashMap;

use chrono::{Duration, NaiveDate, Utc};
use chrysopoeia_core::{CodecCount, LibraryStats, SavingsPoint};
use sqlx::sqlite::SqliteRow;
use sqlx::{Row, SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::uuid_col;

/// Buckets shown before the rest is folded into "Other".
pub const TOP_BUCKETS: usize = 8;

/// Done files needed before a projection is shown.
pub const MIN_DONE_FOR_PROJECTION: i64 = 3;

const STATS_COLUMNS: &str = "COUNT(*) AS file_count, \
    COALESCE(SUM(size_bytes), 0) AS total_bytes, \
    COALESCE(SUM(status = 'pending'), 0) AS pending, \
    COALESCE(SUM(status = 'queued'), 0) AS queued, \
    COALESCE(SUM(status = 'processing'), 0) AS processing, \
    COALESCE(SUM(status = 'done'), 0) AS done, \
    COALESCE(SUM(status = 'skipped'), 0) AS skipped, \
    COALESCE(SUM(status = 'failed'), 0) AS failed, \
    COALESCE(SUM(saved_bytes), 0) AS saved_bytes";

fn count(row: &SqliteRow, column: &str) -> sqlx::Result<u64> {
    let v: i64 = row.try_get(column)?;
    Ok(u64::try_from(v).unwrap_or(0))
}

fn stats_from_row(row: &SqliteRow) -> sqlx::Result<LibraryStats> {
    Ok(LibraryStats {
        file_count: count(row, "file_count")?,
        total_bytes: count(row, "total_bytes")?,
        pending: count(row, "pending")?,
        queued: count(row, "queued")?,
        processing: count(row, "processing")?,
        done: count(row, "done")?,
        skipped: count(row, "skipped")?,
        failed: count(row, "failed")?,
        saved_bytes: row.try_get("saved_bytes")?,
        settling: 0,
    })
}

/// Stats for every library that has files.
pub async fn by_library(pool: &SqlitePool) -> sqlx::Result<HashMap<Uuid, LibraryStats>> {
    let rows = sqlx::query(&format!(
        "SELECT library_id, {STATS_COLUMNS} FROM files GROUP BY library_id"
    ))
    .fetch_all(pool)
    .await?;
    let mut out = HashMap::with_capacity(rows.len());
    for row in &rows {
        out.insert(uuid_col(row, "library_id")?, stats_from_row(row)?);
    }
    Ok(out)
}

/// Stats for one library.
pub async fn for_library(pool: &SqlitePool, id: Uuid) -> sqlx::Result<LibraryStats> {
    let row = sqlx::query(&format!(
        "SELECT {STATS_COLUMNS} FROM files WHERE library_id = ?"
    ))
    .bind(id.to_string())
    .fetch_one(pool)
    .await?;
    stats_from_row(&row)
}

/// Stats across all libraries.
pub async fn totals(pool: &SqlitePool) -> sqlx::Result<LibraryStats> {
    let row = sqlx::query(&format!("SELECT {STATS_COLUMNS} FROM files"))
        .fetch_one(pool)
        .await?;
    stats_from_row(&row)
}

/// Which column [`buckets`] groups by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    VideoCodec,
    AudioCodec,
    Resolution,
}

impl Bucket {
    fn column(self) -> &'static str {
        match self {
            Self::VideoCodec => "video_codec",
            Self::AudioCodec => "audio_codec",
            Self::Resolution => "resolution",
        }
    }

    fn missing_label(self) -> &'static str {
        match self {
            Self::VideoCodec => "No video",
            Self::AudioCodec => "No audio",
            Self::Resolution => "Unknown",
        }
    }
}

/// File counts and bytes per bucket: the top [`TOP_BUCKETS`] by file count,
/// then everything else as "Other". Files that were never probed are left out.
pub async fn buckets(pool: &SqlitePool, bucket: Bucket) -> sqlx::Result<Vec<CodecCount>> {
    let col = bucket.column();
    let rows = sqlx::query(&format!(
        "SELECT {col} AS name, COUNT(*) AS files, COALESCE(SUM(size_bytes), 0) AS bytes \
         FROM files WHERE probe IS NOT NULL GROUP BY {col} ORDER BY files DESC, bytes DESC"
    ))
    .fetch_all(pool)
    .await?;
    let mut out: Vec<CodecCount> = Vec::new();
    let mut other = CodecCount {
        name: "Other".into(),
        files: 0,
        bytes: 0,
    };
    for (i, row) in rows.iter().enumerate() {
        let name: Option<String> = row.try_get("name")?;
        let item = CodecCount {
            name: name.unwrap_or_else(|| bucket.missing_label().to_string()),
            files: count(row, "files")?,
            bytes: count(row, "bytes")?,
        };
        if i < TOP_BUCKETS {
            out.push(item);
        } else {
            other.files += item.files;
            other.bytes += item.bytes;
        }
    }
    if other.files > 0 {
        out.push(other);
    }
    Ok(out)
}

/// Add a converted file to today's savings (UTC).
pub async fn add_savings(conn: &mut SqliteConnection, saved_bytes: i64) -> sqlx::Result<()> {
    let date = Utc::now().date_naive().format("%Y-%m-%d").to_string();
    sqlx::query(
        "INSERT INTO savings (date, saved_bytes, files) VALUES (?, ?, 1) \
         ON CONFLICT(date) DO UPDATE SET saved_bytes = saved_bytes + excluded.saved_bytes, \
         files = files + 1",
    )
    .bind(date)
    .bind(saved_bytes)
    .execute(conn)
    .await?;
    Ok(())
}

/// The last `days` days ending today (UTC), oldest first, zero-filled.
pub async fn savings_history(pool: &SqlitePool, days: u32) -> sqlx::Result<Vec<SavingsPoint>> {
    let today = Utc::now().date_naive();
    let first = today - Duration::days(i64::from(days.saturating_sub(1)));
    let first_s = first.format("%Y-%m-%d").to_string();
    let rows = sqlx::query("SELECT date, saved_bytes, files FROM savings WHERE date >= ?")
        .bind(&first_s)
        .fetch_all(pool)
        .await?;
    let mut by_date: HashMap<String, (i64, u64)> = HashMap::with_capacity(rows.len());
    for row in &rows {
        let date: String = row.try_get("date")?;
        by_date.insert(date, (row.try_get("saved_bytes")?, count(row, "files")?));
    }
    let mut out = Vec::with_capacity(days as usize);
    let mut day: NaiveDate = first;
    while day <= today {
        let key = day.format("%Y-%m-%d").to_string();
        let (saved_bytes, files) = by_date.get(&key).copied().unwrap_or((0, 0));
        out.push(SavingsPoint {
            date: key,
            saved_bytes,
            files,
        });
        day += Duration::days(1);
    }
    Ok(out)
}

/// Projected extra savings for pending and queued files, from the average
/// ratio achieved on done files. `None` until enough files are done.
pub async fn projected_savings(pool: &SqlitePool) -> sqlx::Result<Option<i64>> {
    let row = sqlx::query(
        "SELECT COUNT(*) AS done, COALESCE(SUM(saved_bytes), 0) AS saved, \
         COALESCE(SUM(original_size_bytes), 0) AS original FROM files \
         WHERE status = 'done' AND original_size_bytes > 0 AND saved_bytes IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;
    let done: i64 = row.try_get("done")?;
    let saved: i64 = row.try_get("saved")?;
    let original: i64 = row.try_get("original")?;
    if done < MIN_DONE_FOR_PROJECTION || original <= 0 {
        return Ok(None);
    }
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COALESCE(SUM(size_bytes), 0) FROM files WHERE status IN ('pending', 'queued')",
    )
    .fetch_one(pool)
    .await?;
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let projected = (remaining as f64 * (saved as f64 / original as f64)).round() as i64;
    Ok(Some(projected.max(0)))
}
