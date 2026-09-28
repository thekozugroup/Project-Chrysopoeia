//! The `files` table: every media file in every library.

use chrono::{DateTime, Utc};
use chrysopoeia_core::{FileStatus, MediaFile, ProbeInfo};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{
    enum_str, i64_of, like_escape, now_ts, opt_u64_col, opt_uuid_col, parse_enum, parse_json,
    to_json, ts, ts_col, u64_col, uuid_col,
};

/// Columns for [`MediaFile`] without the probe. `progress` comes from the
/// file's current job while it is processing.
const COLUMNS: &str = "f.id, f.library_id, f.path, f.relative_path, f.file_name, f.size_bytes, \
    f.modified_at, f.status, f.container, f.video_codec, f.audio_codec, f.resolution, f.hdr, \
    f.duration_secs, f.bit_rate, f.original_size_bytes, f.saved_bytes, f.skip_reason, f.error, \
    f.job_id, f.scanned_at, f.updated_at, \
    CASE WHEN f.status = 'processing' THEN j.progress END AS progress";

const FROM: &str = "FROM files f LEFT JOIN jobs j ON j.id = f.job_id";

/// Map a row selected with [`COLUMNS`] (plus `f.probe` when `with_probe`).
fn from_row(row: &SqliteRow, with_probe: bool) -> sqlx::Result<MediaFile> {
    let status: String = row.try_get("status")?;
    let hdr: Option<String> = row.try_get("hdr")?;
    let probe = if with_probe {
        let raw: Option<String> = row.try_get("probe")?;
        raw.as_deref().map(parse_json).transpose()?
    } else {
        None
    };
    let progress: Option<f64> = row.try_get("progress")?;
    Ok(MediaFile {
        id: uuid_col(row, "id")?,
        library_id: uuid_col(row, "library_id")?,
        path: row.try_get("path")?,
        relative_path: row.try_get("relative_path")?,
        file_name: row.try_get("file_name")?,
        size_bytes: u64_col(row, "size_bytes")?,
        modified_at: ts_col(row, "modified_at")?,
        status: FileStatus::parse(&status)
            .ok_or_else(|| super::decode_error(format!("bad file status {status:?}")))?,
        container: row.try_get("container")?,
        video_codec: row.try_get("video_codec")?,
        audio_codec: row.try_get("audio_codec")?,
        resolution: row.try_get("resolution")?,
        hdr: hdr.as_deref().map(parse_enum).transpose()?,
        duration_secs: row.try_get("duration_secs")?,
        bit_rate: opt_u64_col(row, "bit_rate")?,
        original_size_bytes: opt_u64_col(row, "original_size_bytes")?,
        saved_bytes: row.try_get("saved_bytes")?,
        skip_reason: row.try_get("skip_reason")?,
        error: row.try_get("error")?,
        job_id: opt_uuid_col(row, "job_id")?,
        #[allow(clippy::cast_possible_truncation)]
        progress: progress.map(|p| p as f32),
        probe,
        scanned_at: ts_col(row, "scanned_at")?,
        updated_at: ts_col(row, "updated_at")?,
    })
}

/// Sort order for [`list`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Size,
    Updated,
    Status,
}

impl SortKey {
    /// Parse `name`, `size`, `updated` or `status`, optionally prefixed with
    /// `-` for descending. Returns the key and whether it is descending.
    pub fn parse(value: &str) -> Option<(Self, bool)> {
        let (desc, key) = match value.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, value),
        };
        let key = match key {
            "name" => Self::Name,
            "size" => Self::Size,
            "updated" => Self::Updated,
            "status" => Self::Status,
            _ => return None,
        };
        Some((key, desc))
    }

    fn sql(self) -> &'static str {
        match self {
            Self::Name => "f.file_name COLLATE NOCASE",
            Self::Size => "f.size_bytes",
            Self::Updated => "f.updated_at",
            // Lifecycle order: what is happening now first.
            Self::Status => {
                "CASE f.status WHEN 'processing' THEN 0 WHEN 'queued' THEN 1 \
                 WHEN 'failed' THEN 2 WHEN 'pending' THEN 3 WHEN 'done' THEN 4 ELSE 5 END"
            }
        }
    }
}

/// Filters for [`list`].
#[derive(Debug, Clone)]
pub struct FileQuery {
    /// Empty = any status.
    pub statuses: Vec<FileStatus>,
    pub library: Option<Uuid>,
    /// Case-insensitive substring of the name or path.
    pub search: Option<String>,
    pub sort: SortKey,
    pub descending: bool,
    pub limit: u32,
    pub offset: u32,
}

fn push_filters(qb: &mut QueryBuilder<'_, Sqlite>, q: &FileQuery) {
    qb.push(" WHERE 1 = 1");
    if !q.statuses.is_empty() {
        qb.push(" AND f.status IN (");
        let mut sep = qb.separated(", ");
        for s in &q.statuses {
            sep.push_bind(s.as_str());
        }
        qb.push(")");
    }
    if let Some(lib) = q.library {
        qb.push(" AND f.library_id = ").push_bind(lib.to_string());
    }
    if let Some(search) = q.search.as_deref().filter(|s| !s.is_empty()) {
        let pattern = format!("%{}%", like_escape(search));
        qb.push(" AND (f.file_name LIKE ")
            .push_bind(pattern.clone())
            .push(" ESCAPE '\\' OR f.relative_path LIKE ")
            .push_bind(pattern)
            .push(" ESCAPE '\\')");
    }
}

/// A page of files (without probes) and the total matching count.
pub async fn list(pool: &SqlitePool, q: &FileQuery) -> sqlx::Result<(Vec<MediaFile>, u64)> {
    let mut count = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM files f");
    push_filters(&mut count, q);
    let total: i64 = count.build_query_scalar().fetch_one(pool).await?;

    let mut qb = QueryBuilder::<Sqlite>::new(format!("SELECT {COLUMNS} {FROM}"));
    push_filters(&mut qb, q);
    qb.push(" ORDER BY ")
        .push(q.sort.sql())
        .push(if q.descending { " DESC" } else { " ASC" })
        .push(", f.path ASC LIMIT ")
        .push_bind(i64::from(q.limit))
        .push(" OFFSET ")
        .push_bind(i64::from(q.offset));
    let rows = qb.build().fetch_all(pool).await?;
    let items = rows
        .iter()
        .map(|r| from_row(r, false))
        .collect::<sqlx::Result<Vec<_>>>()?;
    Ok((items, u64::try_from(total).unwrap_or(0)))
}

/// One file, optionally with its probe.
pub async fn get(pool: &SqlitePool, id: Uuid, with_probe: bool) -> sqlx::Result<Option<MediaFile>> {
    get_conn(&mut *pool.acquire().await?, id, with_probe).await
}

/// [`get`] on a specific connection (e.g. inside a transaction).
pub async fn get_conn(
    conn: &mut SqliteConnection,
    id: Uuid,
    with_probe: bool,
) -> sqlx::Result<Option<MediaFile>> {
    let probe_col = if with_probe { ", f.probe" } else { "" };
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS}{probe_col} {FROM} WHERE f.id = ?"
    ))
    .bind(id.to_string())
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(|r| from_row(r, with_probe)).transpose()
}

/// Start of the error a job records when its input file is gone. Scans look
/// at such files again when they reappear unchanged (a share that was briefly
/// disconnected).
pub const MISSING_INPUT_ERROR: &str = "The file is no longer at ";

/// What a scan needs to know about a stored file. Also the guard for writing
/// the scan's result: a row that changed since it was read (a job finished,
/// a watch event, the user) is left alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub id: Uuid,
    pub path: String,
    pub size_bytes: u64,
    /// Stored timestamp string (compare with [`ts`] of the disk mtime).
    pub modified_at: String,
    pub status: FileStatus,
    /// Stored `updated_at`, which every write changes.
    pub updated_at: String,
    /// The last job failed because the file was missing.
    pub missing_input: bool,
}

const INDEX_COLUMNS: &str = "id, path, size_bytes, modified_at, status, updated_at, \
    (status = 'failed' AND substr(COALESCE(error, ''), 1, length(?1)) = ?1) AS missing_input";

fn index_from_row(row: &SqliteRow) -> sqlx::Result<IndexEntry> {
    let status: String = row.try_get("status")?;
    Ok(IndexEntry {
        id: uuid_col(row, "id")?,
        path: row.try_get("path")?,
        size_bytes: u64_col(row, "size_bytes")?,
        modified_at: row.try_get("modified_at")?,
        status: FileStatus::parse(&status)
            .ok_or_else(|| super::decode_error(format!("bad file status {status:?}")))?,
        updated_at: row.try_get("updated_at")?,
        missing_input: row.try_get("missing_input")?,
    })
}

/// Index of every file in a library.
pub async fn index(pool: &SqlitePool, library_id: Uuid) -> sqlx::Result<Vec<IndexEntry>> {
    let rows = sqlx::query(&format!(
        "SELECT {INDEX_COLUMNS} FROM files WHERE library_id = ?2"
    ))
    .bind(MISSING_INPUT_ERROR)
    .bind(library_id.to_string())
    .fetch_all(pool)
    .await?;
    rows.iter().map(index_from_row).collect()
}

/// Index entry for one path.
pub async fn find_by_path(pool: &SqlitePool, path: &str) -> sqlx::Result<Option<IndexEntry>> {
    find_by_path_conn(&mut *pool.acquire().await?, path).await
}

/// [`find_by_path`] on a specific connection.
pub async fn find_by_path_conn(
    conn: &mut SqliteConnection,
    path: &str,
) -> sqlx::Result<Option<IndexEntry>> {
    let row = sqlx::query(&format!(
        "SELECT {INDEX_COLUMNS} FROM files WHERE path = ?2"
    ))
    .bind(MISSING_INPUT_ERROR)
    .bind(path)
    .fetch_optional(conn)
    .await?;
    row.as_ref().map(index_from_row).transpose()
}

/// A file as found by a scan, after probing and deciding.
#[derive(Debug, Clone)]
pub struct FileUpsert {
    pub library_id: Uuid,
    pub path: String,
    pub relative_path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub modified_at: DateTime<Utc>,
    pub status: FileStatus,
    pub probe: Option<ProbeInfo>,
    pub skip_reason: Option<String>,
    pub error: Option<String>,
}

/// Columns derived from a probe.
struct ProbeColumns {
    json: Option<String>,
    container: Option<String>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    resolution: Option<String>,
    hdr: Option<String>,
    duration_secs: Option<f64>,
    bit_rate: Option<i64>,
}

impl ProbeColumns {
    fn from_probe(probe: Option<&ProbeInfo>) -> sqlx::Result<Self> {
        let Some(p) = probe else {
            return Ok(Self {
                json: None,
                container: None,
                video_codec: None,
                audio_codec: None,
                resolution: None,
                hdr: None,
                duration_secs: None,
                bit_rate: None,
            });
        };
        Ok(Self {
            json: Some(to_json(p)?),
            container: Some(p.container.clone()).filter(|c| !c.is_empty()),
            video_codec: p.video_codec().map(str::to_string),
            audio_codec: p.audio_codec().map(str::to_string),
            resolution: p.resolution_label().map(str::to_string),
            hdr: p.hdr().map(|h| enum_str(&h)),
            duration_secs: p.duration_secs,
            bit_rate: p.bit_rate.map(i64_of),
        })
    }
}

/// Insert a new file row. Returns its id, or `None` when a row with the same
/// path already exists.
pub async fn insert(conn: &mut SqliteConnection, f: &FileUpsert) -> sqlx::Result<Option<Uuid>> {
    let id = Uuid::new_v4();
    let pc = ProbeColumns::from_probe(f.probe.as_ref())?;
    let now = now_ts();
    let done = sqlx::query(
        "INSERT INTO files (id, library_id, path, relative_path, file_name, size_bytes, \
         modified_at, status, probe, container, video_codec, audio_codec, resolution, hdr, \
         duration_secs, bit_rate, skip_reason, error, scanned_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(path) DO NOTHING",
    )
    .bind(id.to_string())
    .bind(f.library_id.to_string())
    .bind(&f.path)
    .bind(&f.relative_path)
    .bind(&f.file_name)
    .bind(i64_of(f.size_bytes))
    .bind(ts(f.modified_at))
    .bind(f.status.as_str())
    .bind(pc.json)
    .bind(pc.container)
    .bind(pc.video_codec)
    .bind(pc.audio_codec)
    .bind(pc.resolution)
    .bind(pc.hdr)
    .bind(pc.duration_secs)
    .bind(pc.bit_rate)
    .bind(&f.skip_reason)
    .bind(&f.error)
    .bind(&now)
    .bind(&now)
    .execute(conn)
    .await?;
    Ok((done.rows_affected() > 0).then_some(id))
}

/// Replace a changed file's row. Savings from an earlier conversion no longer
/// apply to the new content, so they are cleared. The row is only updated
/// when it still matches `seen` (nothing wrote it since the scan read it) and
/// is not queued or processing; returns whether it was updated.
pub async fn update_scanned(
    conn: &mut SqliteConnection,
    seen: &IndexEntry,
    f: &FileUpsert,
) -> sqlx::Result<bool> {
    let pc = ProbeColumns::from_probe(f.probe.as_ref())?;
    let now = now_ts();
    let done = sqlx::query(
        "UPDATE files SET relative_path = ?, file_name = ?, size_bytes = ?, modified_at = ?, \
         status = ?, probe = ?, container = ?, video_codec = ?, audio_codec = ?, \
         resolution = ?, hdr = ?, duration_secs = ?, bit_rate = ?, skip_reason = ?, error = ?, \
         original_size_bytes = NULL, saved_bytes = NULL, scanned_at = ?, updated_at = ? \
         WHERE id = ? AND status NOT IN ('queued', 'processing') AND updated_at = ? \
         AND size_bytes = ? AND modified_at = ?",
    )
    .bind(&f.relative_path)
    .bind(&f.file_name)
    .bind(i64_of(f.size_bytes))
    .bind(ts(f.modified_at))
    .bind(f.status.as_str())
    .bind(pc.json)
    .bind(pc.container)
    .bind(pc.video_codec)
    .bind(pc.audio_codec)
    .bind(pc.resolution)
    .bind(pc.hdr)
    .bind(pc.duration_secs)
    .bind(pc.bit_rate)
    .bind(&f.skip_reason)
    .bind(&f.error)
    .bind(&now)
    .bind(&now)
    .bind(seen.id.to_string())
    .bind(&seen.updated_at)
    .bind(i64_of(seen.size_bytes))
    .bind(&seen.modified_at)
    .execute(conn)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Store a fresh probe for a file whose content changed before a job ran.
pub async fn update_probe(
    pool: &SqlitePool,
    id: Uuid,
    size_bytes: u64,
    modified_at: DateTime<Utc>,
    probe: &ProbeInfo,
) -> sqlx::Result<()> {
    let pc = ProbeColumns::from_probe(Some(probe))?;
    sqlx::query(
        "UPDATE files SET size_bytes = ?, modified_at = ?, probe = ?, container = ?, \
         video_codec = ?, audio_codec = ?, resolution = ?, hdr = ?, duration_secs = ?, \
         bit_rate = ?, scanned_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(i64_of(size_bytes))
    .bind(ts(modified_at))
    .bind(pc.json)
    .bind(pc.container)
    .bind(pc.video_codec)
    .bind(pc.audio_codec)
    .bind(pc.resolution)
    .bind(pc.hdr)
    .bind(pc.duration_secs)
    .bind(pc.bit_rate)
    .bind(now_ts())
    .bind(now_ts())
    .bind(id.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// The file after a successful replace: new location, size, codecs.
#[derive(Debug, Clone)]
pub struct ReplacedFile {
    pub path: String,
    pub relative_path: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub modified_at: DateTime<Utc>,
    /// Fresh probe of the output; `None` keeps the stored codec columns.
    pub probe: Option<ProbeInfo>,
}

/// Point a file row at its replacement.
pub async fn apply_replacement(
    conn: &mut SqliteConnection,
    id: Uuid,
    r: &ReplacedFile,
) -> sqlx::Result<()> {
    let now = now_ts();
    sqlx::query(
        "UPDATE files SET path = ?, relative_path = ?, file_name = ?, size_bytes = ?, \
         modified_at = ?, updated_at = ? WHERE id = ?",
    )
    .bind(&r.path)
    .bind(&r.relative_path)
    .bind(&r.file_name)
    .bind(i64_of(r.size_bytes))
    .bind(ts(r.modified_at))
    .bind(&now)
    .bind(id.to_string())
    .execute(&mut *conn)
    .await?;
    if let Some(probe) = &r.probe {
        let pc = ProbeColumns::from_probe(Some(probe))?;
        sqlx::query(
            "UPDATE files SET probe = ?, container = ?, video_codec = ?, audio_codec = ?, \
             resolution = ?, hdr = ?, duration_secs = ?, bit_rate = ?, scanned_at = ? \
             WHERE id = ?",
        )
        .bind(pc.json)
        .bind(pc.container)
        .bind(pc.video_codec)
        .bind(pc.audio_codec)
        .bind(pc.resolution)
        .bind(pc.hdr)
        .bind(pc.duration_secs)
        .bind(pc.bit_rate)
        .bind(&now)
        .bind(id.to_string())
        .execute(conn)
        .await?;
    }
    Ok(())
}

/// Set a file's status, reason and error, but only while its status is one
/// of `only_if`. Returns whether the row changed.
pub async fn set_status_where(
    conn: &mut SqliteConnection,
    id: Uuid,
    status: FileStatus,
    skip_reason: Option<&str>,
    only_if: &[FileStatus],
) -> sqlx::Result<bool> {
    let mut qb = QueryBuilder::<Sqlite>::new("UPDATE files SET status = ");
    qb.push_bind(status.as_str())
        .push(", skip_reason = ")
        .push_bind(skip_reason)
        .push(", error = NULL, updated_at = ")
        .push_bind(now_ts())
        .push(" WHERE id = ")
        .push_bind(id.to_string())
        .push(" AND status IN (");
    let mut sep = qb.separated(", ");
    for s in only_if {
        sep.push_bind(s.as_str());
    }
    qb.push(")");
    Ok(qb.build().execute(conn).await?.rows_affected() > 0)
}

/// After a job for a file Chrysopoeia converted before ended without a new
/// result (skipped): the file on disk is still the converted one, so it
/// stays `done` with its savings. Only files with `original_size_bytes`
/// qualify (set when a conversion replaced the file, cleared when its
/// content changes). Returns whether the file was kept done.
pub async fn keep_converted(conn: &mut SqliteConnection, id: Uuid) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE files SET status = 'done', skip_reason = NULL, error = NULL, updated_at = ? \
         WHERE id = ? AND original_size_bytes IS NOT NULL",
    )
    .bind(now_ts())
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Set a file's status, reason and error (and optionally its job).
pub async fn set_status(
    conn: &mut SqliteConnection,
    id: Uuid,
    status: FileStatus,
    skip_reason: Option<&str>,
    error: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE files SET status = ?, skip_reason = ?, error = ?, updated_at = ? WHERE id = ?",
    )
    .bind(status.as_str())
    .bind(skip_reason)
    .bind(error)
    .bind(now_ts())
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// Mark files "skipped by the user" in one statement: those among `ids`
/// whose status is still one of `statuses` (except `processing`, which needs
/// its job cancelled first). Their queued jobs are cancelled. Returns how
/// many files were skipped and the ids of those that are processing.
pub async fn skip_many(
    conn: &mut SqliteConnection,
    ids: &[Uuid],
    statuses: &[FileStatus],
    reason: &str,
) -> sqlx::Result<(u64, Vec<Uuid>)> {
    let now = now_ts();
    let mut qb =
        QueryBuilder::<Sqlite>::new("SELECT id FROM files WHERE status = 'processing' AND id IN (");
    let mut sep = qb.separated(", ");
    for id in ids {
        sep.push_bind(id.to_string());
    }
    qb.push(")");
    let processing = qb
        .build()
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(|r| uuid_col(r, "id"))
        .collect::<sqlx::Result<Vec<_>>>()?;

    let skippable: Vec<&str> = statuses
        .iter()
        .filter(|s| **s != FileStatus::Processing)
        .map(|s| s.as_str())
        .collect();
    if skippable.is_empty() {
        return Ok((0, processing));
    }
    // The files to skip, looked up by id. (`+status` keeps SQLite from
    // scanning the status index instead of the id lookups.)
    let mut qb = QueryBuilder::<Sqlite>::new("SELECT id FROM files WHERE id IN (");
    let mut sep = qb.separated(", ");
    for id in ids {
        sep.push_bind(id.to_string());
    }
    qb.push(") AND +status IN (");
    let mut sep = qb.separated(", ");
    for s in &skippable {
        sep.push_bind(*s);
    }
    qb.push(")");
    let targets: Vec<String> = qb.build_query_scalar().fetch_all(&mut *conn).await?;
    if targets.is_empty() {
        return Ok((0, processing));
    }

    let mut qb = QueryBuilder::<Sqlite>::new("UPDATE jobs SET state = 'cancelled', finished_at = ");
    qb.push_bind(&now).push(" WHERE file_id IN (");
    let mut sep = qb.separated(", ");
    for id in &targets {
        sep.push_bind(id.as_str());
    }
    qb.push(") AND +state = 'queued'");
    qb.build().execute(&mut *conn).await?;

    let mut qb = QueryBuilder::<Sqlite>::new("UPDATE files SET status = 'skipped', skip_reason = ");
    qb.push_bind(reason)
        .push(", error = NULL, updated_at = ")
        .push_bind(&now)
        .push(" WHERE id IN (");
    let mut sep = qb.separated(", ");
    for id in &targets {
        sep.push_bind(id.as_str());
    }
    qb.push(")");
    let skipped = qb.build().execute(&mut *conn).await?.rows_affected();
    Ok((skipped, processing))
}

/// What a deleted row knew about its file, kept for a while in case the
/// same file turns up under another name (a renamed file or folder), so it
/// keeps its state instead of being probed and converted again.
#[derive(Debug, Clone)]
pub struct RemovedRow {
    pub library_id: Uuid,
    pub size_bytes: u64,
    /// Stored timestamp string (compare with [`ts`] of the disk mtime).
    pub modified_at: String,
    pub status: FileStatus,
    pub probe: Option<ProbeInfo>,
    pub skip_reason: Option<String>,
    pub error: Option<String>,
    pub original_size_bytes: Option<u64>,
    pub saved_bytes: Option<i64>,
}

const REMOVED_COLUMNS: &str = "library_id, size_bytes, modified_at, status, probe, \
    skip_reason, error, original_size_bytes, saved_bytes";

fn removed_from_row(row: &SqliteRow) -> sqlx::Result<RemovedRow> {
    let status: String = row.try_get("status")?;
    let probe: Option<String> = row.try_get("probe")?;
    Ok(RemovedRow {
        library_id: uuid_col(row, "library_id")?,
        size_bytes: u64_col(row, "size_bytes")?,
        modified_at: row.try_get("modified_at")?,
        status: FileStatus::parse(&status)
            .ok_or_else(|| super::decode_error(format!("bad file status {status:?}")))?,
        // A probe that doesn't parse is just not carried over.
        probe: probe.as_deref().and_then(|p| parse_json(p).ok()),
        skip_reason: row.try_get("skip_reason")?,
        error: row.try_get("error")?,
        original_size_bytes: opt_u64_col(row, "original_size_bytes")?,
        saved_bytes: row.try_get("saved_bytes")?,
    })
}

/// Delete the rows of files a scan no longer found, unless a row changed
/// since the scan read it (a job moved it to a new name, a watch event) or
/// is being processed. Their jobs cascade. Returns what the deleted rows
/// knew.
pub async fn delete_unchanged(
    conn: &mut SqliteConnection,
    seen: &[IndexEntry],
) -> sqlx::Result<Vec<RemovedRow>> {
    let mut deleted = Vec::new();
    for e in seen {
        let row = sqlx::query(&format!(
            "DELETE FROM files WHERE id = ? AND updated_at = ? AND status != 'processing' \
             RETURNING {REMOVED_COLUMNS}"
        ))
        .bind(e.id.to_string())
        .bind(&e.updated_at)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = row {
            deleted.push(removed_from_row(&row)?);
        }
    }
    Ok(deleted)
}

/// Delete the file at `path`, or every file under the folder `path`, except
/// files being processed right now. Returns what the deleted rows knew.
pub async fn delete_path_or_prefix(pool: &SqlitePool, path: &str) -> sqlx::Result<Vec<RemovedRow>> {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    let rows = sqlx::query(&format!(
        "DELETE FROM files WHERE status != 'processing' \
         AND (path = ? OR substr(path, 1, length(?)) = ?) RETURNING {REMOVED_COLUMNS}"
    ))
    .bind(path)
    .bind(&prefix)
    .bind(&prefix)
    .fetch_all(pool)
    .await?;
    rows.iter().map(removed_from_row).collect()
}

/// Insert the row of a file that was removed under another name moments ago
/// (see [`RemovedRow`]): its status, probe, reasons and savings carry over,
/// its job history does not. Returns the new id, or `None` when the path
/// already has a row.
pub async fn insert_moved(
    conn: &mut SqliteConnection,
    f: &FileUpsert,
    from: &RemovedRow,
) -> sqlx::Result<Option<Uuid>> {
    let row = FileUpsert {
        status: from.status,
        probe: from.probe.clone(),
        skip_reason: from.skip_reason.clone(),
        error: from.error.clone(),
        ..f.clone()
    };
    let Some(id) = insert(&mut *conn, &row).await? else {
        return Ok(None);
    };
    if from.original_size_bytes.is_some() || from.saved_bytes.is_some() {
        sqlx::query("UPDATE files SET original_size_bytes = ?, saved_bytes = ? WHERE id = ?")
            .bind(from.original_size_bytes.map(i64_of))
            .bind(from.saved_bytes)
            .bind(id.to_string())
            .execute(conn)
            .await?;
    }
    Ok(Some(id))
}

/// A file whose decision can be recomputed after a profile change.
#[derive(Debug, Clone)]
pub struct RedecideCandidate {
    pub id: Uuid,
    pub status: FileStatus,
    pub probe: ProbeInfo,
    pub skip_reason: Option<String>,
    /// A finished job skipped it because the result was not small enough
    /// (as opposed to a decision that needed no job at all).
    pub size_rule_skip: bool,
}

/// Condition for files a profile change re-decides: `pending` and `skipped`
/// files with a probe, except files the user skipped by hand (`?` is the
/// user's skip reason).
const REDECIDE_FILTER: &str = "f.status IN ('pending', 'skipped') AND f.probe IS NOT NULL \
    AND (f.skip_reason IS NULL OR f.skip_reason != ?)";

/// Ids of the files of a library a profile change re-decides.
pub async fn redecide_candidate_ids(
    pool: &SqlitePool,
    library_id: Uuid,
    user_skip_reason: &str,
) -> sqlx::Result<Vec<Uuid>> {
    let rows = sqlx::query(&format!(
        "SELECT f.id FROM files f WHERE f.library_id = ? AND {REDECIDE_FILTER}"
    ))
    .bind(library_id.to_string())
    .bind(user_skip_reason)
    .fetch_all(pool)
    .await?;
    rows.iter().map(|r| uuid_col(r, "id")).collect()
}

/// The files among `ids` that still qualify for re-deciding, with their
/// probes. Read inside the caller's write transaction so the rows can't
/// change before they are written.
pub async fn redecide_candidates(
    conn: &mut SqliteConnection,
    ids: &[Uuid],
    user_skip_reason: &str,
) -> sqlx::Result<Vec<RedecideCandidate>> {
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT f.id, f.status, f.probe, f.skip_reason, \
         (f.status = 'skipped' AND j.state = 'skipped' AND j.output_size IS NOT NULL \
          AND j.skip_reason IS f.skip_reason) AS size_rule_skip \
         FROM files f LEFT JOIN jobs j ON j.id = f.job_id WHERE f.id IN (",
    );
    let mut sep = qb.separated(", ");
    for id in ids {
        sep.push_bind(id.to_string());
    }
    // `REDECIDE_FILTER` with its one parameter bound in place.
    let (before, after) = REDECIDE_FILTER
        .split_once('?')
        .unwrap_or((REDECIDE_FILTER, ""));
    qb.push(") AND ")
        .push(before)
        .push_bind(user_skip_reason)
        .push(after);
    let rows = qb.build().fetch_all(conn).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let id = uuid_col(row, "id")?;
        let status: String = row.try_get("status")?;
        let probe: String = row.try_get("probe")?;
        let size_rule_skip: Option<bool> = row.try_get("size_rule_skip")?;
        let skip_reason: Option<String> = row.try_get("skip_reason")?;
        let Some(status) = FileStatus::parse(&status) else {
            continue;
        };
        match parse_json::<ProbeInfo>(&probe) {
            Ok(probe) => out.push(RedecideCandidate {
                id,
                status,
                probe,
                skip_reason,
                size_rule_skip: size_rule_skip.unwrap_or(false),
            }),
            Err(e) => tracing::warn!("skipping a stored probe that no longer parses: {e}"),
        }
    }
    Ok(out)
}

/// What deciding whether a selected file is worth queueing needs.
#[derive(Debug, Clone)]
pub struct QueueCandidate {
    pub id: Uuid,
    pub library_id: Uuid,
    pub status: FileStatus,
    /// `None` when the file was never probed (or its probe no longer parses).
    pub probe: Option<ProbeInfo>,
}

/// The files among `ids` (at most a few hundred), with their probes.
pub async fn queue_candidates(
    pool: &SqlitePool,
    ids: &[Uuid],
) -> sqlx::Result<Vec<QueueCandidate>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT id, library_id, status, probe FROM files WHERE id IN (",
    );
    let mut sep = qb.separated(", ");
    for id in ids {
        sep.push_bind(id.to_string());
    }
    qb.push(")");
    let rows = qb.build().fetch_all(pool).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let status: String = row.try_get("status")?;
        let Some(status) = FileStatus::parse(&status) else {
            continue;
        };
        let probe: Option<String> = row.try_get("probe")?;
        out.push(QueueCandidate {
            id: uuid_col(row, "id")?,
            library_id: uuid_col(row, "library_id")?,
            status,
            probe: probe.and_then(|p| parse_json::<ProbeInfo>(&p).ok()),
        });
    }
    Ok(out)
}

/// Ids of files matching a bulk selection.
pub async fn select_ids(
    pool: &SqlitePool,
    ids: Option<&[Uuid]>,
    library: Option<Uuid>,
    statuses: &[FileStatus],
) -> sqlx::Result<Vec<Uuid>> {
    let mut out = Vec::new();
    let id_chunks: Vec<Option<&[Uuid]>> = match ids {
        Some(ids) => ids.chunks(500).map(Some).collect(),
        None => vec![None],
    };
    for chunk in id_chunks {
        let mut qb = QueryBuilder::<Sqlite>::new("SELECT id FROM files WHERE 1 = 1");
        if let Some(chunk) = chunk {
            qb.push(" AND id IN (");
            let mut sep = qb.separated(", ");
            for id in chunk {
                sep.push_bind(id.to_string());
            }
            qb.push(")");
        }
        if let Some(lib) = library {
            qb.push(" AND library_id = ").push_bind(lib.to_string());
        }
        if !statuses.is_empty() {
            qb.push(" AND status IN (");
            let mut sep = qb.separated(", ");
            for s in statuses {
                sep.push_bind(s.as_str());
            }
            qb.push(")");
        }
        for row in qb.build().fetch_all(pool).await? {
            out.push(uuid_col(&row, "id")?);
        }
    }
    Ok(out)
}
