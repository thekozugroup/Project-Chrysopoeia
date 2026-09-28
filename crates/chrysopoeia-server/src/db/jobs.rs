//! The `jobs` table: one row per conversion attempt of a file.

use chrysopoeia_core::{FileStatus, HwApi, Job, JobProgress, JobStage, JobState, ValidationReport};
use sqlx::sqlite::SqliteRow;
use sqlx::{QueryBuilder, Row, Sqlite, SqliteConnection, SqlitePool};
use uuid::Uuid;

use super::{
    Db, enum_str, i64_of, now_ts, opt_ts_col, opt_u64_col, parse_enum, parse_json, to_json, ts_col,
    u64_col, uuid_col,
};

const COLUMNS: &str = "id, file_id, library_id, file_name, file_path, state, stage, priority, \
    progress, fps, speed, eta_secs, encoder, hw_api, attempt, input_size, output_size, error, \
    skip_reason, validation, command, log_tail, notes, force, created_at, started_at, finished_at";

/// States that count as finished (history).
pub const FINISHED_STATES: &str = "('done', 'skipped', 'failed', 'cancelled')";

#[allow(clippy::cast_possible_truncation)]
fn from_row(row: &SqliteRow) -> sqlx::Result<Job> {
    let state: String = row.try_get("state")?;
    let stage: String = row.try_get("stage")?;
    let hw_api: Option<String> = row.try_get("hw_api")?;
    let validation: Option<String> = row.try_get("validation")?;
    let progress: f64 = row.try_get("progress")?;
    let fps: Option<f64> = row.try_get("fps")?;
    let speed: Option<f64> = row.try_get("speed")?;
    let priority: i64 = row.try_get("priority")?;
    let attempt: i64 = row.try_get("attempt")?;
    let notes: Option<String> = row.try_get("notes")?;
    let force: i64 = row.try_get("force")?;
    Ok(Job {
        // Filled from the `problem` column once the backend records causes.
        problem: None,
        force: force != 0,
        notes: notes
            .as_deref()
            .map(parse_json::<Vec<String>>)
            .transpose()?
            .unwrap_or_default(),
        id: uuid_col(row, "id")?,
        file_id: uuid_col(row, "file_id")?,
        library_id: uuid_col(row, "library_id")?,
        file_name: row.try_get("file_name")?,
        file_path: row.try_get("file_path")?,
        state: parse_enum(&state)?,
        stage: parse_enum(&stage)?,
        priority: i32::try_from(priority).unwrap_or(0),
        progress: progress as f32,
        fps: fps.map(|v| v as f32),
        speed: speed.map(|v| v as f32),
        eta_secs: opt_u64_col(row, "eta_secs")?,
        encoder: row.try_get("encoder")?,
        hw_api: hw_api.as_deref().map(parse_enum::<HwApi>).transpose()?,
        attempt: u32::try_from(attempt).unwrap_or(0),
        input_size: u64_col(row, "input_size")?,
        output_size: opt_u64_col(row, "output_size")?,
        error: row.try_get("error")?,
        skip_reason: row.try_get("skip_reason")?,
        validation: validation
            .as_deref()
            .map(parse_json::<ValidationReport>)
            .transpose()?,
        command: row.try_get("command")?,
        log_tail: row.try_get("log_tail")?,
        created_at: ts_col(row, "created_at")?,
        started_at: opt_ts_col(row, "started_at")?,
        finished_at: opt_ts_col(row, "finished_at")?,
    })
}

/// One job.
pub async fn get(pool: &SqlitePool, id: Uuid) -> sqlx::Result<Option<Job>> {
    get_conn(&mut *pool.acquire().await?, id).await
}

/// [`get`] on a specific connection.
pub async fn get_conn(conn: &mut SqliteConnection, id: Uuid) -> sqlx::Result<Option<Job>> {
    let row = sqlx::query(&format!("SELECT {COLUMNS} FROM jobs WHERE id = ?"))
        .bind(id.to_string())
        .fetch_optional(conn)
        .await?;
    row.as_ref().map(from_row).transpose()
}

/// Which jobs `GET /api/jobs` lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobFilter {
    /// Everything, newest first.
    All,
    /// Running then queued, in queue order.
    Active,
    Running,
    /// Queue order: priority, then oldest first.
    Queued,
    /// Finished jobs, newest first.
    History,
}

impl JobFilter {
    /// Parse the `state` query value.
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "all" => Self::All,
            "active" => Self::Active,
            "running" => Self::Running,
            "queued" => Self::Queued,
            "history" => Self::History,
            _ => return None,
        })
    }

    /// `WHERE` and `ORDER BY` for filters that are a single ordered range.
    /// Ties are broken by `rowid`, which grows with every insert, so jobs
    /// created in the same millisecond keep the order they were queued in.
    fn clause(self) -> String {
        match self {
            Self::All => " ORDER BY created_at DESC, rowid DESC".to_string(),
            Self::Active => " WHERE state IN ('running', 'queued') \
                ORDER BY CASE state WHEN 'running' THEN 0 ELSE 1 END, \
                priority DESC, created_at ASC, rowid ASC"
                .to_string(),
            Self::Running => {
                " WHERE state = 'running' ORDER BY started_at ASC, rowid ASC".to_string()
            }
            Self::Queued => format!(" WHERE state = 'queued' ORDER BY {QUEUE_ORDER}"),
            // Every finished job has `finished_at`, and the partial index
            // `idx_jobs_finished` holds exactly these rows in this order.
            // Without statistics SQLite would pick the state index and sort
            // the whole history, so the index is named.
            Self::History => format!(
                " INDEXED BY idx_jobs_finished WHERE state IN {FINISHED_STATES} \
                 ORDER BY finished_at DESC, rowid DESC"
            ),
        }
    }
}

/// Queue order: highest priority, then oldest, then the order of creation.
const QUEUE_ORDER: &str = "priority DESC, created_at ASC, rowid ASC";

async fn list_page(
    pool: &SqlitePool,
    filter: JobFilter,
    limit: u32,
    offset: u32,
) -> sqlx::Result<Vec<Job>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs{} LIMIT ? OFFSET ?",
        filter.clause()
    ))
    .bind(i64::from(limit))
    .bind(i64::from(offset))
    .fetch_all(pool)
    .await?;
    rows.iter().map(from_row).collect()
}

/// The query plan of a job page (for tests).
#[cfg(test)]
pub async fn explain_list(pool: &SqlitePool, filter: JobFilter) -> Vec<String> {
    let rows = sqlx::query(&format!(
        "EXPLAIN QUERY PLAN SELECT {COLUMNS} FROM jobs{} LIMIT 50 OFFSET 0",
        filter.clause()
    ))
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    rows.iter()
        .filter_map(|r| r.try_get::<String, _>("detail").ok())
        .collect()
}

async fn count(pool: &SqlitePool, filter: JobFilter) -> sqlx::Result<u64> {
    let clause = filter.clause();
    let where_part = clause.split(" ORDER BY").next().unwrap_or_default();
    let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM jobs{where_part}"))
        .fetch_one(pool)
        .await?;
    Ok(u64::try_from(total).unwrap_or(0))
}

/// A page of jobs and the total count for the filter.
pub async fn list(
    pool: &SqlitePool,
    filter: JobFilter,
    limit: u32,
    offset: u32,
) -> sqlx::Result<(Vec<Job>, u64)> {
    if filter != JobFilter::Active {
        let total = count(pool, filter).await?;
        return Ok((list_page(pool, filter, limit, offset).await?, total));
    }
    // Running jobs first, then the queue. Paged as two index-ordered ranges
    // so a queue of tens of thousands is never sorted as a whole.
    let running = count(pool, JobFilter::Running).await?;
    let queued = count(pool, JobFilter::Queued).await?;
    let offset64 = u64::from(offset);
    let mut items = if offset64 < running {
        list_page(pool, JobFilter::Running, limit, offset).await?
    } else {
        Vec::new()
    };
    let remaining = limit.saturating_sub(u32::try_from(items.len()).unwrap_or(u32::MAX));
    if remaining > 0 {
        let queue_offset = u32::try_from(offset64.saturating_sub(running)).unwrap_or(u32::MAX);
        items.extend(list_page(pool, JobFilter::Queued, remaining, queue_offset).await?);
    }
    Ok((items, running + queued))
}

/// The newest jobs of a file.
pub async fn for_file(pool: &SqlitePool, file_id: Uuid, limit: u32) -> sqlx::Result<Vec<Job>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE file_id = ? ORDER BY created_at DESC, rowid DESC \
         LIMIT ?"
    ))
    .bind(file_id.to_string())
    .bind(i64::from(limit))
    .fetch_all(pool)
    .await?;
    rows.iter().map(from_row).collect()
}

/// What a new job needs from its file.
#[derive(Debug, Clone)]
pub struct NewJob<'a> {
    pub file_id: Uuid,
    pub library_id: Uuid,
    pub file_name: &'a str,
    pub file_path: &'a str,
    pub input_size: u64,
    pub priority: i32,
    /// "Convert anyway" (see `Job::force`).
    pub force: bool,
}

/// Create a queued job and mark its file queued. Returns the job id, or
/// `None` when the file is already queued or processing (checked inside the
/// caller's transaction, so concurrent requests can't queue a file twice).
pub async fn create(conn: &mut SqliteConnection, new: &NewJob<'_>) -> sqlx::Result<Option<Uuid>> {
    let id = Uuid::new_v4();
    let now = now_ts();
    let claimed = sqlx::query(
        "UPDATE files SET status = 'queued', job_id = ?, skip_reason = NULL, error = NULL, \
         updated_at = ? WHERE id = ? AND status NOT IN ('queued', 'processing')",
    )
    .bind(id.to_string())
    .bind(&now)
    .bind(new.file_id.to_string())
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if claimed == 0 {
        return Ok(None);
    }
    sqlx::query(
        "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, stage, \
         priority, progress, attempt, input_size, force, created_at) \
         VALUES (?, ?, ?, ?, ?, 'queued', 'waiting', ?, 0, 0, ?, ?, ?)",
    )
    .bind(id.to_string())
    .bind(new.file_id.to_string())
    .bind(new.library_id.to_string())
    .bind(new.file_name)
    .bind(new.file_path)
    .bind(new.priority)
    .bind(i64_of(new.input_size))
    .bind(new.force)
    .bind(&now)
    .execute(conn)
    .await?;
    Ok(Some(id))
}

/// Queue many files at once (one job each, in the given order), but only
/// those whose status is one of `only_if` (which must not include `queued`
/// or `processing`). Two statements for the whole list, so large selections
/// stay fast; callers pass a few hundred files at a time. Returns how many
/// were queued.
pub async fn create_many(
    conn: &mut SqliteConnection,
    file_ids: &[Uuid],
    only_if: &[FileStatus],
) -> sqlx::Result<u64> {
    let allowed: Vec<&str> = only_if
        .iter()
        .filter(|s| !matches!(s, FileStatus::Queued | FileStatus::Processing))
        .map(|s| s.as_str())
        .collect();
    if file_ids.is_empty() || allowed.is_empty() {
        return Ok(0);
    }
    let now = now_ts();
    let new_rows = |qb: &mut QueryBuilder<'_, Sqlite>| {
        qb.push("WITH new(job_id, file_id, pos) AS (VALUES ");
        let mut sep = qb.separated(", ");
        for (pos, id) in file_ids.iter().enumerate() {
            sep.push("(")
                .push_bind_unseparated(Uuid::new_v4().to_string())
                .push_unseparated(", ")
                .push_bind_unseparated(id.to_string())
                .push_unseparated(", ")
                .push_bind_unseparated(i64::try_from(pos).unwrap_or(i64::MAX))
                .push_unseparated(")");
        }
        qb.push(") ");
    };
    let status_filter = |qb: &mut QueryBuilder<'_, Sqlite>| {
        qb.push("+f.status IN (");
        let mut sep = qb.separated(", ");
        for s in &allowed {
            sep.push_bind(*s);
        }
        qb.push(")");
    };

    let mut qb = QueryBuilder::<Sqlite>::new("");
    new_rows(&mut qb);
    qb.push(
        "INSERT INTO jobs (id, file_id, library_id, file_name, file_path, state, stage, \
         priority, progress, attempt, input_size, created_at) \
         SELECT new.job_id, f.id, f.library_id, f.file_name, f.path, 'queued', 'waiting', \
         0, 0, 0, f.size_bytes, ",
    );
    qb.push_bind(&now)
        .push(" FROM new JOIN files f ON f.id = new.file_id WHERE ");
    status_filter(&mut qb);
    qb.push(" ORDER BY new.pos");
    qb.build().execute(&mut *conn).await?;

    // Point each file at the job just created for it.
    let mut qb = QueryBuilder::<Sqlite>::new(
        "UPDATE files AS f SET status = 'queued', skip_reason = NULL, error = NULL, updated_at = ",
    );
    qb.push_bind(&now).push(
        ", job_id = (SELECT j.id FROM jobs j WHERE j.file_id = f.id AND j.state = 'queued' \
         ORDER BY j.rowid DESC LIMIT 1) WHERE f.id IN (",
    );
    let mut sep = qb.separated(", ");
    for id in file_ids {
        sep.push_bind(id.to_string());
    }
    qb.push(") AND ");
    status_filter(&mut qb);
    Ok(qb.build().execute(conn).await?.rows_affected())
}

/// Claim the next queued job of an enabled library: mark it running and its
/// file processing, atomically. Jobs of `skip_libraries` (folders that are
/// offline) and `skip_jobs` (waiting for a file to settle) are passed over.
/// Returns the claimed job.
pub async fn claim_next(
    db: &Db,
    skip_libraries: &[Uuid],
    skip_jobs: &[Uuid],
) -> sqlx::Result<Option<Job>> {
    let mut tx = db.write_tx().await?;
    let now = now_ts();
    let mut qb = QueryBuilder::<Sqlite>::new(
        "UPDATE jobs SET state = 'running', stage = 'preparing', progress = 0, fps = NULL, \
         speed = NULL, eta_secs = NULL, attempt = 1, error = NULL, skip_reason = NULL, \
         notes = NULL, finished_at = NULL, started_at = ",
    );
    qb.push_bind(now.clone()).push(
        " WHERE id = (SELECT j.id FROM jobs j JOIN libraries l ON l.id = j.library_id \
         WHERE j.state = 'queued' AND l.enabled = 1",
    );
    if !skip_libraries.is_empty() {
        qb.push(" AND j.library_id NOT IN (");
        let mut sep = qb.separated(", ");
        for id in skip_libraries {
            sep.push_bind(id.to_string());
        }
        qb.push(")");
    }
    if !skip_jobs.is_empty() {
        qb.push(" AND j.id NOT IN (");
        let mut sep = qb.separated(", ");
        for id in skip_jobs {
            sep.push_bind(id.to_string());
        }
        qb.push(")");
    }
    qb.push(" ORDER BY j.priority DESC, j.created_at ASC, j.rowid ASC LIMIT 1) RETURNING id");
    let claimed: Option<String> = qb.build_query_scalar().fetch_optional(&mut *tx).await?;
    let Some(id) = claimed else {
        tx.rollback().await?;
        return Ok(None);
    };
    let Ok(id) = Uuid::parse_str(&id) else {
        tx.rollback().await?;
        return Ok(None);
    };
    let job = get_conn(&mut tx, id).await?;
    if let Some(job) = &job {
        sqlx::query(
            "UPDATE files SET status = 'processing', job_id = ?, error = NULL, updated_at = ? \
             WHERE id = ?",
        )
        .bind(id.to_string())
        .bind(&now)
        .bind(job.file_id.to_string())
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(job)
}

/// Store live progress.
pub async fn update_progress(pool: &SqlitePool, p: &JobProgress) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE jobs SET stage = ?, progress = ?, fps = ?, speed = ?, eta_secs = ?, \
         encoder = COALESCE(?, encoder), hw_api = COALESCE(?, hw_api), attempt = ? \
         WHERE id = ? AND state = 'running'",
    )
    .bind(enum_str(&p.stage))
    .bind(f64::from(p.progress))
    .bind(p.fps.map(f64::from))
    .bind(p.speed.map(f64::from))
    .bind(p.eta_secs.map(i64_of))
    .bind(&p.encoder)
    .bind(p.hw_api.map(|a| enum_str(&a)))
    .bind(i64::from(p.attempt))
    .bind(p.job_id.to_string())
    .execute(pool)
    .await?;
    Ok(())
}

/// Final fields of a finished job.
#[derive(Debug, Clone, Default)]
pub struct JobFinish {
    pub error: Option<String>,
    pub skip_reason: Option<String>,
    pub encoder: Option<String>,
    pub hw_api: Option<HwApi>,
    pub attempt: Option<u32>,
    pub output_size: Option<u64>,
    pub validation: Option<ValidationReport>,
    pub command: Option<String>,
    pub log_tail: Option<String>,
    /// Plain-language notes about compromises the conversion made.
    pub notes: Vec<String>,
}

/// Mark a job finished. Returns false when the job no longer exists.
pub async fn finish(
    conn: &mut SqliteConnection,
    id: Uuid,
    state: JobState,
    f: &JobFinish,
) -> sqlx::Result<bool> {
    let validation = f.validation.as_ref().map(to_json).transpose()?;
    let notes = if f.notes.is_empty() {
        None
    } else {
        Some(to_json(&f.notes)?)
    };
    let done = sqlx::query(
        "UPDATE jobs SET state = ?, error = ?, skip_reason = ?, \
         encoder = COALESCE(?, encoder), hw_api = COALESCE(?, hw_api), \
         attempt = COALESCE(?, attempt), output_size = ?, validation = ?, command = ?, \
         log_tail = ?, notes = ?, eta_secs = NULL, \
         progress = CASE WHEN ? = 'done' THEN 100 ELSE progress END, finished_at = ? \
         WHERE id = ?",
    )
    .bind(enum_str(&state))
    .bind(&f.error)
    .bind(&f.skip_reason)
    .bind(&f.encoder)
    .bind(f.hw_api.map(|a| enum_str(&a)))
    .bind(f.attempt.map(i64::from))
    .bind(f.output_size.map(i64_of))
    .bind(validation)
    .bind(&f.command)
    .bind(&f.log_tail)
    .bind(notes)
    .bind(enum_str(&state))
    .bind(now_ts())
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Put a job back in the queue as if it had never started.
pub async fn requeue(conn: &mut SqliteConnection, id: Uuid) -> sqlx::Result<()> {
    sqlx::query(
        "UPDATE jobs SET state = 'queued', stage = ?, progress = 0, fps = NULL, speed = NULL, \
         eta_secs = NULL, attempt = 0, started_at = NULL, finished_at = NULL WHERE id = ?",
    )
    .bind(enum_str(&JobStage::Waiting))
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(())
}

/// Cancel a job that has not started. Its file goes to `file_status`,
/// except that a file Chrysopoeia converted before goes back to `done` when
/// `file_status` is `pending` (see [`super::files::keep_converted`]).
/// Returns false when the job was not queued.
pub async fn cancel_queued(
    conn: &mut SqliteConnection,
    id: Uuid,
    file_status: FileStatus,
    skip_reason: Option<&str>,
) -> sqlx::Result<bool> {
    let now = now_ts();
    let file_id: Option<String> = sqlx::query_scalar(
        "UPDATE jobs SET state = 'cancelled', finished_at = ? WHERE id = ? AND state = 'queued' \
         RETURNING file_id",
    )
    .bind(&now)
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    let Some(file_id) = file_id else {
        return Ok(false);
    };
    close_file(conn, &file_id, id, "queued", file_status, skip_reason).await?;
    Ok(true)
}

/// Close a job the database says is running but that has no task (the
/// queue marks a job running a moment before its task starts, and a crash
/// can leave one): only while it is still running, so a result recorded in
/// the meantime is never overwritten. Its file goes back to `pending`, or
/// to `done` when Chrysopoeia converted it before. Returns false when the
/// job was no longer running.
pub async fn cancel_running_row(conn: &mut SqliteConnection, id: Uuid) -> sqlx::Result<bool> {
    let file_id: Option<String> = sqlx::query_scalar(
        "UPDATE jobs SET state = 'cancelled', eta_secs = NULL, finished_at = ? \
         WHERE id = ? AND state = 'running' RETURNING file_id",
    )
    .bind(now_ts())
    .bind(id.to_string())
    .fetch_optional(&mut *conn)
    .await?;
    let Some(file_id) = file_id else {
        return Ok(false);
    };
    close_file(conn, &file_id, id, "processing", FileStatus::Pending, None).await?;
    Ok(true)
}

/// After cancelling job `job_id`, move its file from `from` to `status`
/// (a converted file to `done` instead of `pending`), if the file still
/// belongs to that job.
async fn close_file(
    conn: &mut SqliteConnection,
    file_id: &str,
    job_id: Uuid,
    from: &str,
    status: FileStatus,
    skip_reason: Option<&str>,
) -> sqlx::Result<()> {
    let now = now_ts();
    if status == FileStatus::Pending {
        let kept = sqlx::query(&format!(
            "UPDATE files SET status = 'done', skip_reason = NULL, error = NULL, updated_at = ?, \
             job_id = {} WHERE id = ? AND job_id = ? AND status = ? \
             AND original_size_bytes IS NOT NULL",
            super::files::LAST_CONVERSION_JOB
        ))
        .bind(&now)
        .bind(file_id)
        .bind(job_id.to_string())
        .bind(from)
        .execute(&mut *conn)
        .await?
        .rows_affected();
        if kept > 0 {
            return Ok(());
        }
    }
    sqlx::query(
        "UPDATE files SET status = ?, skip_reason = ?, updated_at = ? \
         WHERE id = ? AND job_id = ? AND status = ?",
    )
    .bind(status.as_str())
    .bind(skip_reason)
    .bind(&now)
    .bind(file_id)
    .bind(job_id.to_string())
    .bind(from)
    .execute(conn)
    .await?;
    Ok(())
}

/// Set a job's priority.
pub async fn set_priority(pool: &SqlitePool, id: Uuid, priority: i32) -> sqlx::Result<()> {
    sqlx::query("UPDATE jobs SET priority = ? WHERE id = ?")
        .bind(priority)
        .bind(id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

/// Highest priority among queued jobs.
pub async fn max_queued_priority(pool: &SqlitePool) -> sqlx::Result<Option<i32>> {
    let v: Option<i64> =
        sqlx::query_scalar("SELECT MAX(priority) FROM jobs WHERE state = 'queued'")
            .fetch_one(pool)
            .await?;
    Ok(v.map(|v| i32::try_from(v).unwrap_or(i32::MAX)))
}

/// Finished jobs kept by [`trim_history`], newest first.
pub const HISTORY_KEEP_ROWS: i64 = 5_000;
/// Finished jobs older than this many days are trimmed by [`trim_history`].
pub const HISTORY_KEEP_DAYS: i64 = 90;
/// Rows deleted per transaction while trimming.
const TRIM_CHUNK: u64 = 500;

/// Delete old finished jobs: those older than [`HISTORY_KEEP_DAYS`] and those
/// beyond the newest [`HISTORY_KEEP_ROWS`]. The job each file points at is
/// always kept (the file's detail page shows it), and so is the conversion
/// of a converted file that is queued again (the file points at it again
/// when the new job ends without a new result). Works in small chunks so it
/// never holds the write lock for long. Returns how many were deleted.
pub async fn trim_history(db: &Db) -> sqlx::Result<u64> {
    let cutoff = super::ts(chrono::Utc::now() - chrono::Duration::days(HISTORY_KEEP_DAYS));
    let oldest_kept: Option<String> = sqlx::query_scalar(&format!(
        "SELECT finished_at FROM jobs{} LIMIT 1 OFFSET ?",
        JobFilter::History.clause()
    ))
    .bind(HISTORY_KEEP_ROWS - 1)
    .fetch_optional(db.pool())
    .await?;
    // Whichever limit reaches further back in time wins.
    let boundary = match oldest_kept {
        Some(t) if t > cutoff => t,
        _ => cutoff,
    };
    let mut deleted = 0;
    loop {
        let mut tx = db.write_tx().await?;
        let n = sqlx::query(&format!(
            "DELETE FROM jobs WHERE rowid IN (SELECT rowid FROM jobs \
             WHERE state IN {FINISHED_STATES} AND finished_at < ? \
             AND id NOT IN (SELECT job_id FROM files WHERE job_id IS NOT NULL) \
             AND id NOT IN (SELECT kept FROM (SELECT (SELECT d.id FROM jobs d \
                 WHERE d.file_id = f.id AND d.state = 'done' \
                 ORDER BY d.finished_at DESC, d.rowid DESC LIMIT 1) AS kept \
                 FROM files f WHERE f.original_size_bytes IS NOT NULL \
                 AND f.status IN ('queued', 'processing')) WHERE kept IS NOT NULL) \
             LIMIT ?)"
        ))
        .bind(&boundary)
        .bind(i64_of(TRIM_CHUNK))
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        deleted += n;
        if n < TRIM_CHUNK {
            return Ok(deleted);
        }
        tokio::task::yield_now().await;
    }
}

/// Delete finished jobs. Files keep their status. Returns the count.
pub async fn clear_history(db: &Db) -> sqlx::Result<u64> {
    let mut tx = db.write_tx().await?;
    sqlx::query(&format!(
        "UPDATE files SET job_id = NULL WHERE job_id IN \
         (SELECT id FROM jobs WHERE state IN {FINISHED_STATES})"
    ))
    .execute(&mut *tx)
    .await?;
    let done = sqlx::query(&format!(
        "DELETE FROM jobs WHERE state IN {FINISHED_STATES}"
    ))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(done.rows_affected())
}

/// Remember where a running job will put its result (see
/// [`interrupted`]).
pub async fn set_final_path(pool: &SqlitePool, id: Uuid, path: &str) -> sqlx::Result<()> {
    sqlx::query("UPDATE jobs SET final_path = ? WHERE id = ? AND state = 'running'")
        .bind(path)
        .bind(id.to_string())
        .execute(pool)
        .await?;
    Ok(())
}

/// A job that was running when the server stopped, with where it was going
/// to put its result.
#[derive(Debug, Clone)]
pub struct InterruptedJob {
    pub job: Job,
    pub final_path: Option<String>,
}

/// Jobs left `running` by the previous run (before [`recover_interrupted`]).
pub async fn interrupted(pool: &SqlitePool) -> sqlx::Result<Vec<InterruptedJob>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS}, final_path FROM jobs WHERE state = 'running'"
    ))
    .fetch_all(pool)
    .await?;
    rows.iter()
        .map(|row| {
            Ok(InterruptedJob {
                job: from_row(row)?,
                final_path: row.try_get("final_path")?,
            })
        })
        .collect()
}

/// Whether a finished conversion other than `job` put its result at `path`.
pub async fn other_done_at(pool: &SqlitePool, job: Uuid, path: &str) -> sqlx::Result<bool> {
    let row = sqlx::query(
        "SELECT 1 FROM jobs WHERE final_path = ? AND id != ? AND state = 'done' LIMIT 1",
    )
    .bind(path)
    .bind(job.to_string())
    .fetch_optional(pool)
    .await?;
    Ok(row.is_some())
}

/// Startup recovery: jobs left `running` go back to the queue, and files left
/// `processing` follow their job (or become `pending` without one).
/// Returns the number of jobs re-queued.
pub async fn recover_interrupted(db: &Db) -> sqlx::Result<u64> {
    let mut tx = db.write_tx().await?;
    let jobs = sqlx::query(
        "UPDATE jobs SET state = 'queued', stage = 'waiting', progress = 0, fps = NULL, \
         speed = NULL, eta_secs = NULL, attempt = 0, started_at = NULL WHERE state = 'running'",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let now = now_ts();
    sqlx::query(
        "UPDATE files SET status = 'queued', updated_at = ? WHERE status = 'processing' \
         AND job_id IN (SELECT id FROM jobs WHERE state = 'queued')",
    )
    .bind(&now)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE files SET status = 'pending', updated_at = ? WHERE status = 'processing'")
        .bind(&now)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(jobs)
}

/// Number of running and queued jobs.
pub async fn counts(pool: &SqlitePool) -> sqlx::Result<(u32, u32)> {
    let row = sqlx::query(
        "SELECT COALESCE(SUM(state = 'running'), 0) AS running, \
                COALESCE(SUM(state = 'queued'), 0) AS queued \
         FROM jobs WHERE state IN ('running', 'queued')",
    )
    .fetch_one(pool)
    .await?;
    let running: i64 = row.try_get("running")?;
    let queued: i64 = row.try_get("queued")?;
    Ok((
        u32::try_from(running).unwrap_or(0),
        u32::try_from(queued).unwrap_or(0),
    ))
}
