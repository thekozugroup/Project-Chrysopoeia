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

/// Several files by id (without probes), in no particular order.
pub async fn get_many(pool: &SqlitePool, ids: &[Uuid]) -> sqlx::Result<Vec<MediaFile>> {
    let mut out = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(500) {
        let mut qb =
            QueryBuilder::<Sqlite>::new(format!("SELECT {COLUMNS} {FROM} WHERE f.id IN ("));
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id.to_string());
        }
        qb.push(")");
        for row in qb.build().fetch_all(pool).await? {
            out.push(from_row(&row, false)?);
        }
    }
    Ok(out)
}

/// What a scan needs to know about a stored file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub id: Uuid,
    pub path: String,
    pub size_bytes: u64,
    /// Stored timestamp string (compare with [`ts`] of the disk mtime).
    pub modified_at: String,
    pub status: FileStatus,
}

fn index_from_row(row: &SqliteRow) -> sqlx::Result<IndexEntry> {
    let status: String = row.try_get("status")?;
    Ok(IndexEntry {
        id: uuid_col(row, "id")?,
        path: row.try_get("path")?,
        size_bytes: u64_col(row, "size_bytes")?,
        modified_at: row.try_get("modified_at")?,
        status: FileStatus::parse(&status)
            .ok_or_else(|| super::decode_error(format!("bad file status {status:?}")))?,
    })
}

/// Index of every file in a library.
pub async fn index(pool: &SqlitePool, library_id: Uuid) -> sqlx::Result<Vec<IndexEntry>> {
    let rows = sqlx::query(
        "SELECT id, path, size_bytes, modified_at, status FROM files WHERE library_id = ?",
    )
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
    let row =
        sqlx::query("SELECT id, path, size_bytes, modified_at, status FROM files WHERE path = ?")
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
/// apply to the new content, so they are cleared. Queued and processing rows
/// are left alone; returns whether the row was updated.
pub async fn update_scanned(
    conn: &mut SqliteConnection,
    id: Uuid,
    f: &FileUpsert,
) -> sqlx::Result<bool> {
    let pc = ProbeColumns::from_probe(f.probe.as_ref())?;
    let now = now_ts();
    let done = sqlx::query(
        "UPDATE files SET relative_path = ?, file_name = ?, size_bytes = ?, modified_at = ?, \
         status = ?, probe = ?, container = ?, video_codec = ?, audio_codec = ?, \
         resolution = ?, hdr = ?, duration_secs = ?, bit_rate = ?, skip_reason = ?, error = ?, \
         original_size_bytes = NULL, saved_bytes = NULL, scanned_at = ?, updated_at = ? \
         WHERE id = ? AND status NOT IN ('queued', 'processing')",
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
    .bind(id.to_string())
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

/// Delete files by id. Their jobs cascade.
pub async fn delete_ids(conn: &mut SqliteConnection, ids: &[Uuid]) -> sqlx::Result<u64> {
    let mut deleted = 0;
    for chunk in ids.chunks(500) {
        let mut qb = QueryBuilder::<Sqlite>::new("DELETE FROM files WHERE id IN (");
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id.to_string());
        }
        qb.push(")");
        deleted += qb.build().execute(&mut *conn).await?.rows_affected();
    }
    Ok(deleted)
}

/// Delete the file at `path`, or every file under the folder `path`, except
/// files being processed right now. Returns the affected library ids.
pub async fn delete_path_or_prefix(pool: &SqlitePool, path: &str) -> sqlx::Result<Vec<Uuid>> {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    let rows = sqlx::query(
        "DELETE FROM files WHERE status != 'processing' \
         AND (path = ? OR substr(path, 1, length(?)) = ?) RETURNING library_id",
    )
    .bind(path)
    .bind(&prefix)
    .bind(&prefix)
    .fetch_all(pool)
    .await?;
    let mut libs = rows
        .iter()
        .map(|r| uuid_col(r, "library_id"))
        .collect::<sqlx::Result<Vec<_>>>()?;
    libs.sort_unstable();
    libs.dedup();
    Ok(libs)
}

/// A file whose decision can be recomputed after a profile change.
#[derive(Debug, Clone)]
pub struct RedecideCandidate {
    pub id: Uuid,
    pub status: FileStatus,
    pub probe: ProbeInfo,
}

/// `pending` and `skipped` files of a library that have a probe, except files
/// the user skipped by hand.
pub async fn redecide_candidates(
    pool: &SqlitePool,
    library_id: Uuid,
    user_skip_reason: &str,
) -> sqlx::Result<Vec<RedecideCandidate>> {
    let rows = sqlx::query(
        "SELECT id, status, probe FROM files WHERE library_id = ? \
         AND status IN ('pending', 'skipped') AND probe IS NOT NULL \
         AND (skip_reason IS NULL OR skip_reason != ?)",
    )
    .bind(library_id.to_string())
    .bind(user_skip_reason)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let status: String = row.try_get("status")?;
        let probe: String = row.try_get("probe")?;
        let Some(status) = FileStatus::parse(&status) else {
            continue;
        };
        match parse_json::<ProbeInfo>(&probe) {
            Ok(probe) => out.push(RedecideCandidate {
                id: uuid_col(row, "id")?,
                status,
                probe,
            }),
            Err(e) => tracing::warn!("skipping a stored probe that no longer parses: {e}"),
        }
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
