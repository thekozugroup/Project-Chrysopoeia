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
    skip_reason, validation, command, log_tail, created_at, started_at, finished_at";

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
    Ok(Job {
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

    fn clause(self) -> String {
        match self {
            Self::All => " ORDER BY created_at DESC, id".to_string(),
            Self::Active => " WHERE state IN ('running', 'queued') \
                ORDER BY CASE state WHEN 'running' THEN 0 ELSE 1 END, \
                priority DESC, created_at ASC, id"
                .to_string(),
            Self::Running => " WHERE state = 'running' ORDER BY started_at ASC, id".to_string(),
            Self::Queued => {
                " WHERE state = 'queued' ORDER BY priority DESC, created_at ASC, id".to_string()
            }
            Self::History => format!(
                " WHERE state IN {FINISHED_STATES} \
                 ORDER BY COALESCE(finished_at, created_at) DESC, id"
            ),
        }
    }
}

/// A page of jobs and the total count for the filter.
pub async fn list(
    pool: &SqlitePool,
    filter: JobFilter,
    limit: u32,
    offset: u32,
) -> sqlx::Result<(Vec<Job>, u64)> {
    let clause = filter.clause();
    let where_part = clause.split(" ORDER BY").next().unwrap_or_default();
    let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM jobs{where_part}"))
        .fetch_one(pool)
        .await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs{clause} LIMIT ? OFFSET ?"
    ))
    .bind(i64::from(limit))
    .bind(i64::from(offset))
    .fetch_all(pool)
    .await?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<sqlx::Result<Vec<_>>>()?;
    Ok((items, u64::try_from(total).unwrap_or(0)))
}

/// The newest jobs of a file.
pub async fn for_file(pool: &SqlitePool, file_id: Uuid, limit: u32) -> sqlx::Result<Vec<Job>> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE file_id = ? ORDER BY created_at DESC, id LIMIT ?"
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
         priority, progress, attempt, input_size, created_at) \
         VALUES (?, ?, ?, ?, ?, 'queued', 'waiting', ?, 0, 0, ?, ?)",
    )
    .bind(id.to_string())
    .bind(new.file_id.to_string())
    .bind(new.library_id.to_string())
    .bind(new.file_name)
    .bind(new.file_path)
    .bind(new.priority)
    .bind(i64_of(new.input_size))
    .bind(&now)
    .execute(conn)
    .await?;
    Ok(Some(id))
}

/// Claim the next queued job of an enabled library: mark it running and its
/// file processing, atomically. Returns the claimed job.
pub async fn claim_next(db: &Db) -> sqlx::Result<Option<Job>> {
    let mut tx = db.write_tx().await?;
    let now = now_ts();
    let claimed: Option<String> = sqlx::query_scalar(
        "UPDATE jobs SET state = 'running', stage = 'preparing', progress = 0, fps = NULL, \
         speed = NULL, eta_secs = NULL, attempt = 1, error = NULL, skip_reason = NULL, \
         started_at = ?, finished_at = NULL \
         WHERE id = (SELECT j.id FROM jobs j JOIN libraries l ON l.id = j.library_id \
                     WHERE j.state = 'queued' AND l.enabled = 1 \
                     ORDER BY j.priority DESC, j.created_at ASC, j.id LIMIT 1) \
         RETURNING id",
    )
    .bind(&now)
    .fetch_optional(&mut *tx)
    .await?;
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
}

/// Mark a job finished. Returns false when the job no longer exists.
pub async fn finish(
    conn: &mut SqliteConnection,
    id: Uuid,
    state: JobState,
    f: &JobFinish,
) -> sqlx::Result<bool> {
    let validation = f.validation.as_ref().map(to_json).transpose()?;
    let done = sqlx::query(
        "UPDATE jobs SET state = ?, error = ?, skip_reason = ?, \
         encoder = COALESCE(?, encoder), hw_api = COALESCE(?, hw_api), \
         attempt = COALESCE(?, attempt), output_size = ?, validation = ?, command = ?, \
         log_tail = ?, eta_secs = NULL, \
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

/// Cancel a job that has not started. Its file goes back to `pending` (or to
/// `file_status` when given). Returns false when the job was not queued.
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
    sqlx::query(
        "UPDATE files SET status = ?, skip_reason = ?, updated_at = ? \
         WHERE id = ? AND job_id = ? AND status = 'queued'",
    )
    .bind(file_status.as_str())
    .bind(skip_reason)
    .bind(&now)
    .bind(file_id)
    .bind(id.to_string())
    .execute(conn)
    .await?;
    Ok(true)
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

/// Ids of queued jobs whose file is in the selection.
pub async fn queued_for_files(pool: &SqlitePool, file_ids: &[Uuid]) -> sqlx::Result<Vec<Uuid>> {
    let mut out = Vec::new();
    for chunk in file_ids.chunks(500) {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT id FROM jobs WHERE state = 'queued' AND file_id IN (",
        );
        let mut sep = qb.separated(", ");
        for id in chunk {
            sep.push_bind(id.to_string());
        }
        qb.push(")");
        for row in qb.build().fetch_all(pool).await? {
            out.push(uuid_col(&row, "id")?);
        }
    }
    Ok(out)
}
