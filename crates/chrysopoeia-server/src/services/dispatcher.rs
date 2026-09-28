//! Dispatcher: runs queued jobs, up to the effective job limit.
//!
//! A single loop, woken by [`DispatcherHandle::wake`] (new job, settings
//! change, job finished) and a 5 s tick, claims the next queued job (highest
//! priority, then oldest) in a transaction and starts a task for it. Each task
//! loads the file, library profile and settings, asks the toolkit for encoder
//! candidates, runs the job, forwards progress, and applies the outcome to the
//! job and file rows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Timelike, Utc};
use chrysopoeia_core::encoder::VIDEO_ENCODERS;
use chrysopoeia_core::{
    ActivityLevel, EncoderCandidate, Event, FileStatus, HwApi, Job, JobProgress, JobStage,
    JobState, MediaFile, OutputMode, QueueState, Settings,
};
use chrysopoeia_worker::{JobOutcome, JobSpec, RunConfig};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::activity::ActivityRefs;
use crate::db::files::ReplacedFile;
use crate::db::jobs::JobFinish;
use crate::db::{self};
use crate::format;
use crate::services::library::{PROBE_TIMEOUT, USER_SKIP_REASON, probe_error_message};
use crate::state::{AppState, lock};

/// Idle re-check interval.
const TICK: Duration = Duration::from_secs(5);
/// Minimum time between progress writes to the database, per job.
const PROGRESS_WRITE_INTERVAL: Duration = Duration::from_secs(2);
/// Hard upper bound on concurrent jobs.
pub const MAX_JOBS_LIMIT: u32 = 32;

/// Why a running job was cancelled; decides where its file goes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelIntent {
    /// The user cancelled it: the file becomes `pending`.
    User,
    /// "Stop now": the job goes back to the queue.
    Requeue,
    /// The user skipped the file: it becomes `skipped`.
    Skip,
    /// The server is shutting down: the job goes back to the queue.
    Shutdown,
    /// The library was removed; its rows are gone.
    Removed,
}

struct RunningJob {
    file_id: Uuid,
    library_id: Uuid,
    cancel: CancellationToken,
    intent: Option<CancelIntent>,
}

/// Shared queue state and the running jobs.
pub struct DispatcherHandle {
    notify: Notify,
    paused: AtomicBool,
    waiting_for_schedule: AtomicBool,
    /// Set once start-up recovery has finished; no job starts before.
    ready: AtomicBool,
    running: std::sync::Mutex<HashMap<Uuid, RunningJob>>,
}

impl DispatcherHandle {
    /// A handle with the persisted pause state.
    pub fn new(paused: bool) -> Self {
        Self {
            notify: Notify::new(),
            paused: AtomicBool::new(paused),
            waiting_for_schedule: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            running: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Ask the loop to look for work now.
    pub fn wake(&self) {
        self.notify.notify_one();
    }

    /// Allow jobs to start (after start-up recovery).
    pub fn set_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
        self.wake();
    }

    /// Whether start-up recovery has finished.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Whether the user paused the queue.
    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Jobs running right now.
    pub fn running_count(&self) -> usize {
        lock(&self.running).len()
    }

    /// Whether a job is running.
    pub fn is_running(&self, job_id: Uuid) -> bool {
        lock(&self.running).contains_key(&job_id)
    }

    /// Cancel a running job. Returns false when it isn't running.
    pub fn cancel_job(&self, job_id: Uuid, intent: CancelIntent) -> bool {
        let mut running = lock(&self.running);
        match running.get_mut(&job_id) {
            Some(job) => {
                job.intent.get_or_insert(intent);
                job.cancel.cancel();
                true
            }
            None => false,
        }
    }

    fn cancel_where(&self, intent: CancelIntent, pred: impl Fn(&RunningJob) -> bool) -> Vec<Uuid> {
        let mut running = lock(&self.running);
        let mut ids = Vec::new();
        for (id, job) in running.iter_mut() {
            if pred(job) {
                job.intent.get_or_insert(intent);
                job.cancel.cancel();
                ids.push(*id);
            }
        }
        ids
    }

    /// Cancel every running job.
    pub fn cancel_all(&self, intent: CancelIntent) -> Vec<Uuid> {
        self.cancel_where(intent, |_| true)
    }

    /// Cancel the running jobs of a library.
    pub fn cancel_library(&self, library_id: Uuid, intent: CancelIntent) -> Vec<Uuid> {
        self.cancel_where(intent, |j| j.library_id == library_id)
    }

    /// Cancel the running job of a file.
    pub fn cancel_file(&self, file_id: Uuid, intent: CancelIntent) -> Vec<Uuid> {
        self.cancel_where(intent, |j| j.file_id == file_id)
    }

    /// Wait until none of `ids` is running, up to `timeout`. Returns whether
    /// they all finished.
    pub async fn wait_finished(&self, ids: &[Uuid], timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            let any = {
                let running = lock(&self.running);
                ids.iter().any(|id| running.contains_key(id))
            };
            if !any {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

/// Removes a job from the running set when its task ends, even on panic.
struct RunningGuard {
    state: AppState,
    job_id: Uuid,
}

impl RunningGuard {
    fn intent(&self) -> Option<CancelIntent> {
        lock(&self.state.dispatcher.running)
            .get(&self.job_id)
            .and_then(|j| j.intent)
    }
}

impl Drop for RunningGuard {
    fn drop(&mut self) {
        lock(&self.state.dispatcher.running).remove(&self.job_id);
        self.state.dispatcher.wake();
    }
}

/// Effective job limit and whether it is automatic: the saved setting, else
/// the hardware recommendation. (`--max-jobs` seeds the setting on first
/// run; after that the setting alone decides, so choosing "Automatic" in the
/// UI really means automatic.)
pub fn effective_max_jobs(state: &AppState) -> (u32, bool) {
    let settings = state.settings();
    if let Some(n) = settings.max_jobs {
        return (n.clamp(1, MAX_JOBS_LIMIT), false);
    }
    let total = match state.hardware.current() {
        Some(hw) => hw.recommended_jobs.total,
        None => crate::services::hardware::jobs_before_detection(),
    };
    (total.clamp(1, MAX_JOBS_LIMIT), true)
}

/// Whether the current local hour is outside the configured active hours.
pub fn outside_active_hours(settings: &Settings) -> bool {
    let hour = u8::try_from(Local::now().hour()).unwrap_or(0);
    settings.active_hours.is_some_and(|h| !h.contains(hour))
}

/// The queue state as the UI shows it.
pub async fn queue_state(state: &AppState) -> sqlx::Result<QueueState> {
    let (running, queued) = db::jobs::counts(state.db.pool()).await?;
    let (max_jobs, max_jobs_auto) = effective_max_jobs(state);
    let paused = state.dispatcher.is_paused();
    let waiting = !paused && queued > 0 && outside_active_hours(&state.settings());
    Ok(QueueState {
        paused,
        running,
        queued,
        max_jobs,
        max_jobs_auto,
        waiting_for_schedule: waiting,
    })
}

/// Pause or resume the queue (persisted). Running jobs are not touched.
pub async fn set_paused(state: &AppState, paused: bool) -> sqlx::Result<()> {
    db::settings::set_flag(state.db.pool(), db::settings::QUEUE_PAUSED_KEY, paused).await?;
    state.dispatcher.paused.store(paused, Ordering::SeqCst);
    if !paused {
        state.dispatcher.wake();
    }
    state.broadcast_queue_state().await;
    Ok(())
}

/// The dispatcher loop. Returns when the server shuts down.
pub async fn run(state: AppState) {
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = state.shutdown.cancelled() => break,
            () = state.dispatcher.notify.notified() => {}
            _ = tick.tick() => {}
        }
        if let Err(e) = fill_slots(&state).await {
            tracing::error!("the job queue could not start a job: {e:#}");
        }
    }
    tracing::debug!("dispatcher stopped");
}

async fn fill_slots(state: &AppState) -> anyhow::Result<()> {
    let d = &state.dispatcher;
    if state.shutdown.is_cancelled()
        || !d.ready.load(Ordering::SeqCst)
        || !state.hardware.is_ready()
    {
        return Ok(());
    }
    let settings = state.settings();
    let outside = outside_active_hours(&settings);
    if d.waiting_for_schedule.swap(outside, Ordering::SeqCst) != outside {
        state.broadcast_queue_state().await;
    }
    if d.is_paused() || outside {
        return Ok(());
    }
    let (max_jobs, _) = effective_max_jobs(state);
    while d.running_count() < max_jobs as usize && !state.shutdown.is_cancelled() {
        let Some(job) = db::jobs::claim_next(&state.db).await? else {
            break;
        };
        start_job(state, job);
    }
    Ok(())
}

fn start_job(state: &AppState, job: Job) {
    let cancel = CancellationToken::new();
    lock(&state.dispatcher.running).insert(
        job.id,
        RunningJob {
            file_id: job.file_id,
            library_id: job.library_id,
            cancel: cancel.clone(),
            intent: None,
        },
    );
    let guard = RunningGuard {
        state: state.clone(),
        job_id: job.id,
    };
    let state = state.clone();
    tokio::spawn(async move {
        state.emit(Event::JobUpdated { job: job.clone() });
        state.broadcast_file(job.file_id).await;
        state.broadcast_queue_state().await;
        tracing::info!(job = %job.id, file = %job.file_path, "job started");

        let (outcome, ctx) = execute(&state, &job, cancel).await;
        let intent = guard.intent();
        if let Err(e) = apply_outcome(&state, &job, outcome, intent, &ctx).await {
            tracing::error!(job = %job.id, "could not record the job result: {e:#}");
        }
        // Free the slot before announcing, so the queue state is current.
        drop(guard);
        state.broadcast_job(job.id).await;
        state.broadcast_file(job.file_id).await;
        state.broadcast_library(job.library_id).await;
        state.broadcast_stats().await;
        state.broadcast_queue_state().await;
    });
}

/// What `apply_outcome` needs to know about how the job ran.
#[derive(Debug, Clone)]
struct ExecContext {
    file: Option<MediaFile>,
    library_root: Option<PathBuf>,
    output_mode: OutputMode,
}

fn failed(error: impl Into<String>) -> JobOutcome {
    JobOutcome::Failed {
        error: error.into(),
        log_tail: None,
        command: None,
        encoder: None,
        attempt: 0,
        validation: None,
    }
}

/// The software encoder for a codec, used when detection offers nothing.
fn software_candidate(codec: chrysopoeia_core::VideoCodec) -> Option<EncoderCandidate> {
    VIDEO_ENCODERS
        .iter()
        .find(|e| e.codec == codec && e.api == HwApi::Software)
        .map(|e| EncoderCandidate {
            name: e.name.to_string(),
            codec,
            api: HwApi::Software,
            device: None,
            hw_decode: false,
        })
}

/// Run config for the current settings.
pub fn run_config(state: &AppState, settings: &Settings) -> RunConfig {
    RunConfig {
        ffmpeg: state.config.ffmpeg.clone(),
        ffprobe: state.config.ffprobe.clone(),
        temp_dir: settings
            .temp_dir
            .as_deref()
            .map(PathBuf::from)
            .or_else(|| state.config.temp_dir.clone()),
        validation: settings.validation,
        output_mode: settings.output_mode,
        output_folder: settings.output_folder.as_deref().map(PathBuf::from),
        keep_file_dates: settings.keep_file_dates,
        low_priority: settings.low_priority,
    }
}

async fn execute(
    state: &AppState,
    job: &Job,
    cancel: CancellationToken,
) -> (JobOutcome, ExecContext) {
    let settings = state.settings();
    let mut ctx = ExecContext {
        file: None,
        library_root: None,
        output_mode: settings.output_mode,
    };
    let file = match db::files::get(state.db.pool(), job.file_id, true).await {
        Ok(Some(f)) => f,
        Ok(None) => return (failed("This file is no longer in the library."), ctx),
        Err(e) => {
            tracing::error!(job = %job.id, "could not load the file: {e}");
            return (failed(crate::error::INTERNAL_MESSAGE), ctx);
        }
    };
    ctx.file = Some(file.clone());
    let lib = match db::libraries::get(state.db.pool(), file.library_id).await {
        Ok(Some(l)) => l,
        Ok(None) => return (JobOutcome::Cancelled, ctx),
        Err(e) => {
            tracing::error!(job = %job.id, "could not load the library: {e}");
            return (failed(crate::error::INTERNAL_MESSAGE), ctx);
        }
    };
    ctx.library_root = Some(PathBuf::from(&lib.path));

    let input = PathBuf::from(&file.path);
    let meta = match tokio::fs::metadata(&input).await {
        Ok(m) if m.is_file() => m,
        _ => {
            return (
                failed(format!(
                    "The file is no longer at {}. It may have been moved or deleted.",
                    file.path
                )),
                ctx,
            );
        }
    };
    let modified: DateTime<Utc> = meta
        .modified()
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(|_| Utc::now());
    let unchanged = meta.len() == file.size_bytes && db::ts(modified) == db::ts(file.modified_at);
    let probe = match file.probe.clone() {
        Some(p) if unchanged => p,
        _ => match state.toolkit.probe_file(input.clone(), PROBE_TIMEOUT).await {
            Ok(p) => {
                if let Err(e) =
                    db::files::update_probe(state.db.pool(), file.id, meta.len(), modified, &p)
                        .await
                {
                    tracing::warn!(job = %job.id, "could not store the new probe: {e}");
                }
                p
            }
            Err(e) => return (failed(probe_error_message(&e)), ctx),
        },
    };

    let hw = state.hardware.current();
    let candidates = match &hw {
        Some(hw) => state
            .toolkit
            .encoder_candidates(
                hw,
                lib.profile.video_codec,
                settings.hardware,
                settings.cpu_fallback,
            )
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let candidates = if candidates.is_empty() {
        software_candidate(lib.profile.video_codec)
            .into_iter()
            .collect()
    } else {
        candidates
    };

    let cfg = run_config(state, &settings);
    let spec = JobSpec {
        job_id: job.id,
        file_id: file.id,
        input,
        library_root: PathBuf::from(&lib.path),
        probe,
        profile: lib.profile.clone(),
        candidates,
    };

    let (tx, rx) = mpsc::channel::<JobProgress>(64);
    let forwarder = tokio::spawn(forward_progress(state.clone(), rx));
    let outcome = match state.toolkit.run_job(cfg, spec, tx, cancel).await {
        Ok(outcome) => outcome,
        Err(_) => failed(
            "The converter stopped unexpectedly. The original file is untouched. The details \
             are in the server log.",
        ),
    };
    // The sender is gone once `run_job` returns, so the forwarder drains and
    // ends; don't wait on a worker that leaked a clone of it.
    if tokio::time::timeout(Duration::from_secs(2), forwarder)
        .await
        .is_err()
    {
        tracing::debug!(job = %job.id, "progress forwarder did not finish in time");
    }
    (outcome, ctx)
}

/// Forward worker progress: every update to the WebSocket, and to the
/// database when the stage changes or every 2 s.
async fn forward_progress(state: AppState, mut rx: mpsc::Receiver<JobProgress>) {
    let mut last_write: Option<Instant> = None;
    let mut last_stage: Option<JobStage> = None;
    while let Some(p) = rx.recv().await {
        let due = last_write.is_none_or(|t| t.elapsed() >= PROGRESS_WRITE_INTERVAL);
        let stage_changed = last_stage != Some(p.stage);
        if due || stage_changed {
            if let Err(e) = db::jobs::update_progress(state.db.pool(), &p).await {
                tracing::warn!(job = %p.job_id, "could not store progress: {e}");
            }
            last_write = Some(Instant::now());
            last_stage = Some(p.stage);
        }
        state.emit(Event::JobProgress(p));
    }
}

fn relative_to(root: Option<&Path>, path: &Path) -> String {
    root.and_then(|r| path.strip_prefix(r).ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Record how a job ended on the job and file rows, and in the feed.
async fn apply_outcome(
    state: &AppState,
    job: &Job,
    outcome: JobOutcome,
    intent: Option<CancelIntent>,
    ctx: &ExecContext,
) -> anyhow::Result<()> {
    let refs = ActivityRefs {
        file_id: Some(job.file_id),
        job_id: Some(job.id),
        library_id: Some(job.library_id),
    };
    let name = ctx
        .file
        .as_ref()
        .map_or(job.file_name.as_str(), |f| f.file_name.as_str())
        .to_string();
    match outcome {
        JobOutcome::Done {
            output_path,
            output_size,
            original_size,
            encoder,
            hw_api,
            attempt,
            validation,
            command,
            notes,
        } => {
            let verified = validation.as_ref().is_some_and(|v| v.passed);
            // Filesystem work before the transaction keeps it short.
            let replaced = if ctx.output_mode == OutputMode::Replace {
                let meta = tokio::fs::metadata(&output_path).await.ok();
                let probe = state
                    .toolkit
                    .probe_file(output_path.clone(), PROBE_TIMEOUT)
                    .await
                    .ok();
                output_path.to_str().map(|p| ReplacedFile {
                    path: p.to_string(),
                    relative_path: relative_to(ctx.library_root.as_deref(), &output_path),
                    file_name: output_path
                        .file_name()
                        .map_or_else(|| p.to_string(), |n| n.to_string_lossy().into_owned()),
                    size_bytes: meta.as_ref().map_or(output_size, std::fs::Metadata::len),
                    modified_at: meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .map_or_else(Utc::now, DateTime::<Utc>::from),
                    probe,
                })
            } else {
                None
            };

            let mut tx = state.db.write_tx().await?;
            let exists = db::jobs::finish(
                &mut tx,
                job.id,
                JobState::Done,
                &JobFinish {
                    encoder: Some(encoder.clone()),
                    hw_api: Some(hw_api),
                    attempt: Some(attempt),
                    output_size: Some(output_size),
                    validation,
                    command: Some(command),
                    ..JobFinish::default()
                },
            )
            .await?;
            if !exists {
                tx.rollback().await?;
                return Ok(());
            }
            let first_original = ctx
                .file
                .as_ref()
                .and_then(|f| f.original_size_bytes)
                .unwrap_or(original_size);
            let current_size = match &replaced {
                Some(r) => {
                    if let Some(f) = &ctx.file
                        && r.path != f.path
                    {
                        // A stale row for the new name (e.g. a file removed
                        // since the last scan) would block the rename.
                        sqlx::query("DELETE FROM files WHERE path = ? AND id != ?")
                            .bind(&r.path)
                            .bind(job.file_id.to_string())
                            .execute(&mut *tx)
                            .await?;
                    }
                    db::files::apply_replacement(&mut tx, job.file_id, r).await?;
                    r.size_bytes
                }
                None => output_size,
            };
            let saved_total = db::i64_of(first_original) - db::i64_of(current_size);
            sqlx::query(
                "UPDATE files SET status = 'done', original_size_bytes = ?, saved_bytes = ?, \
                 skip_reason = NULL, error = NULL, job_id = ?, updated_at = ? WHERE id = ?",
            )
            .bind(db::i64_of(first_original))
            .bind(saved_total)
            .bind(job.id.to_string())
            .bind(db::now_ts())
            .bind(job.file_id.to_string())
            .execute(&mut *tx)
            .await?;
            let saved_now = db::i64_of(original_size) - db::i64_of(output_size);
            db::stats::add_savings(&mut tx, saved_now).await?;
            tx.commit().await?;

            let pct = format::percent(saved_now, original_size);
            let mut message = if saved_now >= 0 {
                format!(
                    "Converted {name} — saved {} ({pct}%)",
                    format::bytes(saved_now.unsigned_abs())
                )
            } else {
                format!(
                    "Converted {name} — {} larger (+{}%)",
                    format::bytes(saved_now.unsigned_abs()),
                    pct.abs()
                )
            };
            if verified {
                message.push_str(", verified");
            }
            for note in notes {
                message.push_str(". ");
                message.push_str(note.trim_end_matches('.'));
            }
            tracing::info!(job = %job.id, encoder, "job done");
            state.activity(ActivityLevel::Success, message, refs).await;
        }
        JobOutcome::Skipped {
            reason,
            encoder,
            output_size,
        } => {
            let mut tx = state.db.write_tx().await?;
            let exists = db::jobs::finish(
                &mut tx,
                job.id,
                JobState::Skipped,
                &JobFinish {
                    skip_reason: Some(reason.clone()),
                    encoder,
                    output_size,
                    ..JobFinish::default()
                },
            )
            .await?;
            if exists {
                db::files::set_status(
                    &mut tx,
                    job.file_id,
                    FileStatus::Skipped,
                    Some(&reason),
                    None,
                )
                .await?;
            }
            tx.commit().await?;
            if exists {
                state
                    .activity(
                        ActivityLevel::Info,
                        format!("Skipped {name} — {reason}"),
                        refs,
                    )
                    .await;
            }
        }
        JobOutcome::Failed {
            error,
            log_tail,
            command,
            encoder,
            attempt,
            validation,
        } => {
            let mut tx = state.db.write_tx().await?;
            let exists = db::jobs::finish(
                &mut tx,
                job.id,
                JobState::Failed,
                &JobFinish {
                    error: Some(error.clone()),
                    encoder,
                    attempt: (attempt > 0).then_some(attempt),
                    validation,
                    command,
                    log_tail,
                    ..JobFinish::default()
                },
            )
            .await?;
            if exists {
                db::files::set_status(&mut tx, job.file_id, FileStatus::Failed, None, Some(&error))
                    .await?;
            }
            tx.commit().await?;
            if exists {
                tracing::warn!(job = %job.id, "job failed: {error}");
                state
                    .activity(
                        ActivityLevel::Error,
                        format!("Failed {name}: {error}"),
                        refs,
                    )
                    .await;
            }
        }
        JobOutcome::Cancelled => {
            let intent = intent.unwrap_or(if state.shutdown.is_cancelled() {
                CancelIntent::Shutdown
            } else {
                CancelIntent::User
            });
            let mut tx = state.db.write_tx().await?;
            match intent {
                CancelIntent::Requeue | CancelIntent::Shutdown => {
                    db::jobs::requeue(&mut tx, job.id).await?;
                    db::files::set_status(&mut tx, job.file_id, FileStatus::Queued, None, None)
                        .await?;
                }
                CancelIntent::User | CancelIntent::Skip | CancelIntent::Removed => {
                    let exists = db::jobs::finish(
                        &mut tx,
                        job.id,
                        JobState::Cancelled,
                        &JobFinish::default(),
                    )
                    .await?;
                    if exists {
                        let (status, reason) = if intent == CancelIntent::Skip {
                            (FileStatus::Skipped, Some(USER_SKIP_REASON))
                        } else {
                            (FileStatus::Pending, None)
                        };
                        db::files::set_status(&mut tx, job.file_id, status, reason, None).await?;
                    }
                }
            }
            tx.commit().await?;
            if intent == CancelIntent::User {
                state
                    .activity(ActivityLevel::Info, format!("Cancelled {name}"), refs)
                    .await;
            }
        }
    }
    Ok(())
}

/// Stop starting jobs, cancel the running ones (they go back to the queue)
/// and wait for them to wind down, up to `timeout`.
pub async fn shutdown(state: &AppState, timeout: Duration) {
    let ids = state.dispatcher.cancel_all(CancelIntent::Shutdown);
    if ids.is_empty() {
        return;
    }
    tracing::info!("stopping {} running job(s)", ids.len());
    if !state.dispatcher.wait_finished(&ids, timeout).await {
        tracing::warn!("some jobs did not stop in time; they will be re-queued on the next start");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrysopoeia_core::VideoCodec;

    #[test]
    fn software_fallback_exists_for_every_codec() {
        for codec in VideoCodec::ALL {
            let c = software_candidate(codec).unwrap();
            assert_eq!(c.api, HwApi::Software);
            assert!(!c.hw_decode);
        }
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_to(Some(Path::new("/m")), Path::new("/m/a/b.mkv")),
            "a/b.mkv"
        );
        assert_eq!(relative_to(None, Path::new("/x/b.mkv")), "/x/b.mkv");
    }
}
