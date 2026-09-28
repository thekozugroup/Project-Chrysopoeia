//! Dispatcher: runs queued jobs, up to the effective job limit.
//!
//! A single loop, woken by [`DispatcherHandle::wake`] (new job, settings
//! change, job finished) and a 5 s tick, claims the next queued job (highest
//! priority, then oldest) in a transaction and starts a task for it. Each task
//! loads the file, library profile and settings, asks the toolkit for encoder
//! candidates, runs the job, forwards progress, and applies the outcome to the
//! job and file rows. A result is never dropped: when the database is busy,
//! recording it is retried until it succeeds.
//!
//! Three kinds of jobs are passed over for a while instead of failing: jobs
//! of a library whose folder is offline (an unmounted share or disk), until
//! the folder is back; jobs whose file is still being copied, until it has
//! settled; and, with CPU fallback off, the jobs of a library whose files
//! need the GPU chosen in Settings while every one of its encoding sessions
//! is taken (by Plex, for instance), until the next hardware detection.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Timelike, Utc};
use chrysopoeia_core::encoder::VIDEO_ENCODERS;
use chrysopoeia_core::{
    ActivityLevel, EncoderCandidate, Event, FileStatus, HwApi, Job, JobProgress, JobStage,
    JobState, MaxJobsSource, MediaFile, OutputMode, QueueState, Settings,
};
use chrysopoeia_scanner::WatchEvent;
use chrysopoeia_worker::finalize::{Interrupted, final_output_path};
use chrysopoeia_worker::{Decision, JobOutcome, JobSpec, RunConfig};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::activity::ActivityRefs;
use crate::db::files::{MISSING_INPUT_ERROR, ReplacedFile};
use crate::db::jobs::{InterruptedJob, JobFinish};
use crate::db::{self};
use crate::format;
use crate::services::library::{
    self, PROBE_TIMEOUT, USER_SKIP_REASON, probe_error_message, still_settling,
};
use crate::state::{AppState, lock};

/// Idle re-check interval.
const TICK: Duration = Duration::from_secs(5);
/// How often an offline library folder is checked again.
const OFFLINE_RECHECK: Duration = Duration::from_secs(15);
/// First wait before retrying to record a job result; doubles up to
/// [`OUTCOME_RETRY_MAX`].
const OUTCOME_RETRY_FIRST: Duration = Duration::from_millis(250);
/// Longest wait between retries to record a job result.
const OUTCOME_RETRY_MAX: Duration = Duration::from_secs(5);
/// Attempts for errors that aren't about a busy database (bugs, corrupt
/// rows) before the job is closed with a plain error instead.
const OUTCOME_HARD_ATTEMPTS: u32 = 5;
/// How long recording a result keeps retrying once shutdown has begun.
const OUTCOME_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
/// Minimum time between progress writes to the database, per job.
const PROGRESS_WRITE_INTERVAL: Duration = Duration::from_secs(2);
/// Hard upper bound on concurrent jobs.
pub const MAX_JOBS_LIMIT: u32 = 32;

/// Why a running job was cancelled; decides where its file goes next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelIntent {
    /// The user cancelled it: the file becomes `pending` (or stays `done`
    /// when Chrysopoeia converted it before).
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
    /// Its first-choice encoder runs on the CPU.
    software: bool,
    cancel: CancellationToken,
    intent: Option<CancelIntent>,
}

/// A library whose folder can't be reached; its jobs wait.
struct Offline {
    reason: String,
    next_check: Instant,
}

/// Shared queue state and the running jobs.
pub struct DispatcherHandle {
    notify: Notify,
    paused: AtomicBool,
    waiting_for_schedule: AtomicBool,
    /// Set once start-up recovery has finished; no job starts before.
    ready: AtomicBool,
    running: std::sync::Mutex<HashMap<Uuid, RunningJob>>,
    /// Libraries whose folder is offline, by id.
    offline: std::sync::Mutex<HashMap<Uuid, Offline>>,
    /// Jobs whose file is still being written, with when to try again.
    deferred: std::sync::Mutex<HashMap<Uuid, Instant>>,
    /// Libraries whose jobs wait for the busy GPU chosen in Settings; they
    /// are tried again after the next hardware detection or settings
    /// change. (By library, not by job: a big queue would otherwise be
    /// claimed and put back one job at a time.)
    hardware_wait: std::sync::Mutex<HashSet<Uuid>>,
    /// Whether the feed already says that conversions wait for a busy GPU.
    hardware_wait_announced: AtomicBool,
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
            offline: std::sync::Mutex::new(HashMap::new()),
            deferred: std::sync::Mutex::new(HashMap::new()),
            hardware_wait: std::sync::Mutex::new(HashSet::new()),
            hardware_wait_announced: AtomicBool::new(false),
        }
    }

    /// Why a library's jobs are waiting for its folder, if they are.
    pub fn offline_reason(&self, library_id: Uuid) -> Option<String> {
        lock(&self.offline)
            .get(&library_id)
            .map(|o| o.reason.clone())
    }

    /// Mark a library's folder offline. Returns true when it wasn't already.
    fn mark_offline(&self, library_id: Uuid, reason: String) -> bool {
        lock(&self.offline)
            .insert(
                library_id,
                Offline {
                    reason,
                    next_check: Instant::now() + OFFLINE_RECHECK,
                },
            )
            .is_none()
    }

    /// A library's folder answered again: its jobs may run. Returns true when
    /// it was offline.
    pub fn library_back(&self, library_id: Uuid) -> bool {
        let was_offline = lock(&self.offline).remove(&library_id).is_some();
        if was_offline {
            self.wake();
        }
        was_offline
    }

    /// Keep a job in the queue without starting it until `until`.
    fn defer(&self, job_id: Uuid, until: Instant) {
        lock(&self.deferred).insert(job_id, until);
    }

    /// Keep a library's jobs in the queue until the hardware is checked
    /// again (or the hardware settings change).
    fn wait_for_hardware(&self, library_id: Uuid) {
        lock(&self.hardware_wait).insert(library_id);
    }

    /// Whether the feed should now say that conversions wait for a busy
    /// GPU: true once per time the GPU is busy.
    fn announce_hardware_wait(&self) -> bool {
        !self.hardware_wait_announced.swap(true, Ordering::SeqCst)
    }

    /// The hardware was checked again, or the hardware settings changed:
    /// jobs waiting for a busy GPU may try again. `still_busy` says whether
    /// a GPU is still busy (the feed then doesn't repeat that they wait).
    pub fn hardware_changed(&self, still_busy: bool) {
        let released = {
            let mut waiting = lock(&self.hardware_wait);
            let n = waiting.len();
            waiting.clear();
            n
        };
        if !still_busy {
            self.hardware_wait_announced.store(false, Ordering::SeqCst);
        }
        if released > 0 {
            self.wake();
        }
    }

    /// Libraries whose jobs wait for a busy GPU.
    pub fn hardware_waiting_count(&self) -> usize {
        lock(&self.hardware_wait).len()
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

    /// Running jobs whose first-choice encoder runs on the CPU.
    pub fn software_running_count(&self) -> usize {
        lock(&self.running).values().filter(|j| j.software).count()
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
/// `--max-jobs` (`MAX_JOBS`), else the hardware recommendation. `MAX_JOBS`
/// takes the place of the automatic count, so it applies whenever "Jobs at
/// once" is Automatic in Settings, and a number chosen there wins.
pub fn effective_max_jobs(state: &AppState) -> (u32, bool) {
    let settings = state.settings();
    if let Some(n) = settings.max_jobs.or(state.config.max_jobs) {
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
    let settings = state.settings();
    let max_jobs_source = if settings.max_jobs.is_some() {
        MaxJobsSource::Settings
    } else if state.config.max_jobs.is_some() {
        MaxJobsSource::Env
    } else {
        MaxJobsSource::Auto
    };
    let paused = state.dispatcher.is_paused();
    let waiting = !paused && queued > 0 && outside_active_hours(&settings);
    Ok(QueueState {
        paused,
        running,
        queued,
        max_jobs,
        max_jobs_auto,
        max_jobs_source,
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
    let (max_jobs, auto) = effective_max_jobs(state);
    if d.running_count() >= max_jobs as usize {
        return Ok(());
    }
    recheck_offline(state).await;
    let offline: Vec<Uuid> = lock(&d.offline).keys().copied().collect();
    let deferred: Vec<Uuid> = {
        let now = Instant::now();
        let mut deferred = lock(&d.deferred);
        deferred.retain(|_, until| *until > now);
        deferred.keys().copied().collect()
    };
    let software_libraries = software_libraries(state, &settings).await?;
    let software_cap = auto.then(|| software_job_cap(state, max_jobs)).flatten();
    while d.running_count() < max_jobs as usize && !state.shutdown.is_cancelled() {
        // CPU encodes beyond the CPU's own job count would only slow each
        // other down, even when the limit follows the GPU: pass over the
        // libraries whose jobs encode on the CPU until one finishes.
        let cpu_full = software_cap.is_some_and(|cap| d.software_running_count() >= cap);
        let mut skip_libraries = offline.clone();
        skip_libraries.extend(lock(&d.hardware_wait).iter().copied());
        if cpu_full {
            skip_libraries.extend(software_libraries.iter().copied());
        }
        // A job whose task is still recording that it goes back to the
        // queue is already `queued` in the database: never start it twice.
        let mut skip_jobs = deferred.clone();
        skip_jobs.extend(lock(&d.running).keys().copied());
        let Some(job) = db::jobs::claim_next(&state.db, &skip_libraries, &skip_jobs).await? else {
            break;
        };
        let software = software_libraries.contains(&job.library_id);
        start_job(state, job, software);
    }
    Ok(())
}

/// How many CPU encodes may run at once when the automatic job limit is
/// higher than the CPU's own count (it follows the GPU): the hardware's
/// recommended CPU jobs. `None` when that is no limit at all.
fn software_job_cap(state: &AppState, max_jobs: u32) -> Option<usize> {
    let hw = state.hardware.current()?;
    let cap = hw.recommended_jobs.cpu_jobs.max(1);
    (cap < max_jobs).then_some(cap as usize)
}

/// Libraries whose jobs would encode on the CPU first: no verified hardware
/// encoder for their codec under the current preference.
async fn software_libraries(state: &AppState, settings: &Settings) -> sqlx::Result<Vec<Uuid>> {
    let Some(hw) = state.hardware.current() else {
        return Ok(Vec::new());
    };
    let libs = db::libraries::list(state.db.pool()).await?;
    let mut by_codec: HashMap<chrysopoeia_core::VideoCodec, bool> = HashMap::new();
    let mut out = Vec::new();
    for lib in libs {
        let codec = lib.profile.video_codec;
        let software = *by_codec.entry(codec).or_insert_with(|| {
            state
                .toolkit
                .encoder_candidates(&hw, codec, settings.hardware, settings.cpu_fallback)
                .ok()
                .and_then(|c| c.first().map(|c| c.api == HwApi::Software))
                .unwrap_or(true)
        });
        if software {
            out.push(lib.id);
        }
    }
    Ok(out)
}

/// Check offline library folders that are due, and let the jobs of those
/// that are back run again.
async fn recheck_offline(state: &AppState) {
    let due: Vec<Uuid> = {
        let now = Instant::now();
        lock(&state.dispatcher.offline)
            .iter()
            .filter(|(_, o)| o.next_check <= now)
            .map(|(id, _)| *id)
            .collect()
    };
    for id in due {
        let lib = match db::libraries::get(state.db.pool(), id).await {
            Ok(Some(lib)) => lib,
            Ok(None) => {
                lock(&state.dispatcher.offline).remove(&id);
                continue;
            }
            Err(e) => {
                tracing::warn!("could not load library {id}: {e}");
                continue;
            }
        };
        match library::root_unavailable(&lib.path).await {
            Some(reason) => {
                if let Some(o) = lock(&state.dispatcher.offline).get_mut(&id) {
                    o.reason = reason;
                    o.next_check = Instant::now() + OFFLINE_RECHECK;
                }
            }
            None => {
                if state.dispatcher.library_back(id) {
                    state
                        .library_activity(
                            ActivityLevel::Info,
                            format!("{} is reachable again; its conversions continue.", lib.name),
                            id,
                        )
                        .await;
                    state.broadcast_library(id).await;
                }
            }
        }
    }
}

fn start_job(state: &AppState, job: Job, software: bool) {
    let cancel = CancellationToken::new();
    lock(&state.dispatcher.running).insert(
        job.id,
        RunningJob {
            file_id: job.file_id,
            library_id: job.library_id,
            software,
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
        tracing::debug!(job = %job.id, file = %job.file_path, "job started");

        let (disposition, ctx) = execute(&state, &job, cancel).await;
        let intent = guard.intent();
        // The slot stays taken until the result is recorded.
        record(&state, &job, disposition, intent, &ctx).await;
        // Free the slot before announcing, so the queue state is current.
        drop(guard);
        state.broadcast_job(job.id).await;
        state.broadcast_file(job.file_id).await;
        state.broadcast_library(job.library_id).await;
        state.broadcast_stats().await;
        state.broadcast_queue_state().await;
        // Watch events for the file were ignored while it was being
        // converted (an upgrade that replaced it, for instance); look at it
        // again now. Nothing happens when it is as the database says.
        if let Some(file) = &ctx.file
            && !state.shutdown.is_cancelled()
            && let Err(e) =
                library::handle_watch_event(&state, WatchEvent::Upserted(PathBuf::from(&file.path)))
                    .await
        {
            tracing::debug!(job = %job.id, "could not look at the file again: {e:#}");
        }
    });
}

/// What `apply_outcome` needs to know about how the job ran.
#[derive(Debug, Clone)]
struct ExecContext {
    file: Option<MediaFile>,
    library_root: Option<PathBuf>,
    library_name: Option<String>,
    output_mode: OutputMode,
}

impl ExecContext {
    /// Whether the file may still be one Chrysopoeia converted before: its
    /// content didn't change since (as far as the job saw).
    fn converted_before(&self) -> bool {
        self.file
            .as_ref()
            .is_none_or(|f| f.original_size_bytes.is_some())
    }
}

/// Why a job goes back to the queue without running.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Requeue {
    /// The library folder can't be reached (the reason says why).
    LibraryOffline(String),
    /// The file changed moments ago and may still be being copied.
    Settling,
    /// The file needs the GPU chosen in Settings, CPU fallback is off, and
    /// every encoding session of that GPU was taken when it was checked
    /// (the problem says so).
    HardwareBusy(String),
}

/// How a job's run ended, as far as the queue is concerned.
#[derive(Debug, Clone, PartialEq)]
enum Disposition {
    /// It ran (or could not run) and has a result.
    Finished(JobOutcome),
    /// It goes back to the queue for later.
    Requeue(Requeue),
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
) -> (Disposition, ExecContext) {
    let settings = state.settings();
    let mut ctx = ExecContext {
        file: None,
        library_root: None,
        library_name: None,
        output_mode: settings.output_mode,
    };
    let done = |outcome: JobOutcome, ctx: ExecContext| (Disposition::Finished(outcome), ctx);
    let file = match db::files::get(state.db.pool(), job.file_id, true).await {
        Ok(Some(f)) => f,
        Ok(None) => return done(failed("This file is no longer in the library."), ctx),
        Err(e) => {
            tracing::error!(job = %job.id, "could not load the file: {e}");
            return done(failed(crate::error::INTERNAL_MESSAGE), ctx);
        }
    };
    ctx.file = Some(file.clone());
    let lib = match db::libraries::get(state.db.pool(), file.library_id).await {
        Ok(Some(l)) => l,
        Ok(None) => return done(JobOutcome::Cancelled, ctx),
        Err(e) => {
            tracing::error!(job = %job.id, "could not load the library: {e}");
            return done(failed(crate::error::INTERNAL_MESSAGE), ctx);
        }
    };
    ctx.library_root = Some(PathBuf::from(&lib.path));
    ctx.library_name = Some(lib.name.clone());

    let input = PathBuf::from(&file.path);
    let meta = match tokio::fs::metadata(&input).await {
        Ok(m) if m.is_file() => m,
        _ => {
            // A whole library missing is a disconnected drive or share, not
            // a deleted file: wait for it instead of failing every job.
            if let Some(reason) = library::root_unavailable(&lib.path).await {
                return (Disposition::Requeue(Requeue::LibraryOffline(reason)), ctx);
            }
            return done(
                failed(format!(
                    "{MISSING_INPUT_ERROR}{}. It may have been moved or deleted.",
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
    if !unchanged && still_settling(modified, state.config.settle) {
        return (Disposition::Requeue(Requeue::Settling), ctx);
    }
    if !unchanged && let Some(f) = ctx.file.as_mut() {
        // New content: an earlier conversion's savings no longer apply
        // (the stored row is updated with the new probe below).
        f.original_size_bytes = None;
        f.saved_bytes = None;
    }
    let probe = match file.probe.clone() {
        Some(p) if unchanged && !lacks_hdr10_metadata(&p) => p,
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
            Err(e) => return done(failed(probe_error_message(&e)), ctx),
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
    // The hardware chosen in Settings can't make this codec here: say so
    // on the job instead of quietly converting on the CPU, and don't
    // convert at all when converting on the CPU is turned off. Only files
    // that would be converted need an encoder: the others are skipped (or
    // fail for their own reason) as usual.
    let preference_problem = hw
        .as_deref()
        .filter(|_| would_convert(state, &probe, &lib.profile, job.force))
        .and_then(|hw| {
            let codec = Some(lib.profile.video_codec);
            chrysopoeia_hwdetect::preference_problem(hw, settings.hardware, codec).map(|problem| {
                let busy = chrysopoeia_hwdetect::preference_busy(hw, settings.hardware, codec);
                (problem, busy)
            })
        });
    if let Some((problem, busy)) = &preference_problem
        && !settings.cpu_fallback
    {
        // A GPU that was only busy (every session taken, by Plex for
        // instance) is waited for; one that doesn't work fails the job.
        if *busy {
            return (
                Disposition::Requeue(Requeue::HardwareBusy(problem.clone())),
                ctx,
            );
        }
        return done(failed(preference_failure(problem)), ctx);
    }
    let preference_problem = preference_problem.map(|(problem, _)| problem);

    let cfg = run_config(state, &settings);
    // Where the result goes, so start-up recovery can finish the
    // replacement if the server stops while it is being put in place.
    let final_path = final_output_path(
        &input,
        lib.profile.container,
        cfg.output_mode,
        cfg.output_folder.as_deref(),
        Path::new(&lib.path),
    );
    if let Some(p) = final_path.to_str()
        && let Err(e) = db::jobs::set_final_path(state.db.pool(), job.id, p).await
    {
        tracing::warn!(job = %job.id, "could not store where the result goes: {e}");
    }
    let spec = JobSpec {
        job_id: job.id,
        file_id: file.id,
        input,
        library_root: PathBuf::from(&lib.path),
        probe,
        profile: lib.profile.clone(),
        candidates,
        force: job.force,
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
    let outcome = match (outcome, &preference_problem) {
        (
            JobOutcome::Done {
                output_path,
                output_size,
                original_size,
                encoder,
                hw_api,
                attempt,
                validation,
                command,
                mut notes,
            },
            Some(problem),
        ) => {
            notes.push(format!(
                "{problem}, so this file was converted on the {}",
                hw_api.label()
            ));
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
            }
        }
        (outcome, _) => outcome,
    };
    // The sender is gone once `run_job` returns, so the forwarder drains and
    // ends; don't wait on a worker that leaked a clone of it.
    if tokio::time::timeout(Duration::from_secs(2), forwarder)
        .await
        .is_err()
    {
        tracing::debug!(job = %job.id, "progress forwarder did not finish in time");
    }
    done(outcome, ctx)
}

/// Whether the worker would convert this file (so it needs an encoder): the
/// decision the worker makes before its first attempt, including "Convert
/// anyway". A decision that can't be made counts as converting; the worker
/// reports the problem.
fn would_convert(
    state: &AppState,
    probe: &chrysopoeia_core::ProbeInfo,
    profile: &chrysopoeia_core::TranscodeProfile,
    force: bool,
) -> bool {
    match state.toolkit.decide(probe, profile) {
        Ok(Decision::Skip { .. }) if force => matches!(
            chrysopoeia_worker::decide_forced(probe, profile),
            Decision::Transcode
        ),
        Ok(Decision::Skip { .. }) => false,
        Ok(Decision::Transcode) | Err(_) => true,
    }
}

/// The error of a job that wasn't converted because the hardware chosen in
/// Settings can't be used and converting on the CPU instead is turned off.
fn preference_failure(problem: &str) -> String {
    format!(
        "{problem}, and converting on the CPU instead is turned off, so this file wasn't \
         converted. Choose Automatic under Hardware in Settings, or allow CPU fallback."
    )
}

/// Whether a stored probe of PQ (HDR10-style) video has no mastering
/// display or light levels: probes made by older versions never read them,
/// so the file is probed again and the conversion keeps them.
fn lacks_hdr10_metadata(probe: &chrysopoeia_core::ProbeInfo) -> bool {
    probe.primary_video().is_some_and(|v| {
        v.color_transfer
            .as_deref()
            .is_some_and(|t| t.eq_ignore_ascii_case("smpte2084"))
            && v.mastering_display.is_none()
            && v.content_light.is_none()
    })
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

/// Record how a job ended, retrying while the database is busy. The result
/// must not be lost: after a replace the file on disk has already changed,
/// and a job left `running` would hold its file forever. Gives up only at
/// shutdown (start-up recovery re-queues the job) or, for errors that
/// retrying can't fix, after closing the job with a plain error.
async fn record(
    state: &AppState,
    job: &Job,
    disposition: Disposition,
    intent: Option<CancelIntent>,
    ctx: &ExecContext,
) {
    let mut delay = OUTCOME_RETRY_FIRST;
    let mut hard_failures = 0u32;
    let mut attempts = 0u32;
    let mut give_up_at: Option<Instant> = None;
    loop {
        let result = match &disposition {
            Disposition::Finished(outcome) => {
                apply_outcome(state, job, outcome.clone(), intent, ctx).await
            }
            Disposition::Requeue(why) => apply_requeue(state, job, why, ctx).await,
        };
        let Err(e) = result else {
            if attempts > 0 {
                tracing::info!(job = %job.id, attempts, "job result recorded after retrying");
            }
            return;
        };
        attempts += 1;
        let transient = e
            .chain()
            .find_map(|c| c.downcast_ref::<sqlx::Error>())
            .is_some_and(db::is_transient);
        if !transient {
            hard_failures += 1;
            if hard_failures >= OUTCOME_HARD_ATTEMPTS {
                tracing::error!(job = %job.id, "could not record the job result: {e:#}");
                close_unrecorded(state, job).await;
                return;
            }
        }
        if attempts == 1 || attempts.is_multiple_of(10) {
            tracing::warn!(
                job = %job.id,
                attempts,
                "could not record the job result yet, retrying: {e:#}"
            );
        }
        if state.shutdown.is_cancelled() {
            let deadline =
                *give_up_at.get_or_insert_with(|| Instant::now() + OUTCOME_SHUTDOWN_GRACE);
            if Instant::now() >= deadline {
                tracing::error!(
                    job = %job.id,
                    "shutting down before the job result could be recorded; the job will be \
                     checked again on the next start"
                );
                return;
            }
        }
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(OUTCOME_RETRY_MAX);
    }
}

/// Last resort when a result can't be recorded: close the job and flag the
/// file so nothing stays "running" and the user sees what happened.
async fn close_unrecorded(state: &AppState, job: &Job) {
    let error = "Chrysopoeia couldn't record how this conversion ended. Scan the library to \
                 bring the file list up to date. The details are in the server log.";
    let result = async {
        let mut tx = state.db.write_tx().await?;
        db::jobs::finish(
            &mut tx,
            job.id,
            JobState::Failed,
            &JobFinish {
                error: Some(error.to_string()),
                ..JobFinish::default()
            },
        )
        .await?;
        db::files::set_status(&mut tx, job.file_id, FileStatus::Failed, None, Some(error)).await?;
        tx.commit().await
    }
    .await;
    if let Err(e) = result {
        tracing::error!(job = %job.id, "could not close the job either: {e}");
    }
}

/// Put a job back in the queue without running it, and hold it (or its
/// library) back for a while so it isn't picked again right away.
async fn apply_requeue(
    state: &AppState,
    job: &Job,
    why: &Requeue,
    ctx: &ExecContext,
) -> anyhow::Result<()> {
    // Hold it back first, so the dispatcher can't pick it up again between
    // the commit and here.
    let newly_offline = match why {
        Requeue::LibraryOffline(reason) => state
            .dispatcher
            .mark_offline(job.library_id, reason.clone()),
        Requeue::Settling => {
            state
                .dispatcher
                .defer(job.id, Instant::now() + state.config.settle);
            false
        }
        Requeue::HardwareBusy(_) => {
            state.dispatcher.wait_for_hardware(job.library_id);
            // Check again in a few minutes for as long as jobs wait.
            crate::services::hardware::recheck_while_waiting(state);
            false
        }
    };
    let mut tx = state.db.write_tx().await?;
    db::jobs::requeue(&mut tx, job.id).await?;
    db::files::set_status(&mut tx, job.file_id, FileStatus::Queued, None, None).await?;
    tx.commit().await?;
    match why {
        Requeue::LibraryOffline(reason) if newly_offline => {
            let name = ctx.library_name.as_deref().unwrap_or("A library");
            state
                .library_activity(
                    ActivityLevel::Warning,
                    format!(
                        "{name} can't be reached right now, so its conversions are waiting. \
                         {reason}"
                    ),
                    job.library_id,
                )
                .await;
            state.broadcast_library(job.library_id).await;
        }
        Requeue::LibraryOffline(_) => {}
        Requeue::Settling => {
            tracing::debug!(job = %job.id, file = %job.file_path, "waiting for the file to finish copying");
        }
        Requeue::HardwareBusy(problem) => {
            tracing::debug!(job = %job.id, file = %job.file_path, "waiting for the chosen GPU");
            if state.dispatcher.announce_hardware_wait() {
                state
                    .activity(
                        ActivityLevel::Warning,
                        hardware_wait_message(problem),
                        ActivityRefs::default(),
                    )
                    .await;
            }
        }
    }
    Ok(())
}

/// The feed entry when conversions start waiting for a busy GPU.
fn hardware_wait_message(problem: &str) -> String {
    format!(
        "{problem}, and converting on the CPU instead is turned off, so conversions that need it \
         wait until it is free. Chrysopoeia checks again by itself every few minutes."
    )
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
    let outcome = match outcome {
        JobOutcome::Failed { error, .. }
            if !error.starts_with(MISSING_INPUT_ERROR) && input_vanished(ctx).await =>
        {
            return remove_vanished(state, job, ctx, &name).await;
        }
        other => other,
    };
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
                    command: (!command.is_empty()).then_some(command),
                    notes: notes.clone(),
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
            db::stats::add_savings(&mut tx, job.library_id, saved_now).await?;
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
            for note in &notes {
                message.push_str(". ");
                message.push_str(note.trim_end_matches('.'));
            }
            tracing::debug!(job = %job.id, encoder, "job done");
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
            // A file Chrysopoeia already converted (queued again, e.g. to
            // try another goal) stays done with its savings when the new
            // attempt is skipped: the file on disk is still the converted
            // one. The job row records the skip.
            let kept_done = exists && db::files::keep_converted(&mut tx, job.file_id).await?;
            if exists && !kept_done {
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
                let message = if kept_done {
                    format!("Kept {name} as it is — {reason}")
                } else {
                    format!("Skipped {name} — {reason}")
                };
                state.activity(ActivityLevel::Info, message, refs).await;
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
            // A file Chrysopoeia converted before is still the converted
            // one (nothing was changed), unless it is gone: it stays done,
            // and the failed job is the record of this attempt.
            let kept_done = exists
                && !error.starts_with(MISSING_INPUT_ERROR)
                && ctx.converted_before()
                && db::files::keep_converted(&mut tx, job.file_id).await?;
            if exists && !kept_done {
                db::files::set_status(&mut tx, job.file_id, FileStatus::Failed, None, Some(&error))
                    .await?;
            }
            tx.commit().await?;
            if exists {
                // The activity entry below is the one warning in the log.
                tracing::debug!(job = %job.id, "job failed: {error}");
                let message = if kept_done {
                    format!(
                        "Failed {name}: {}. The file stays as its earlier conversion left it.",
                        error.trim_end_matches('.')
                    )
                } else {
                    format!("Failed {name}: {error}")
                };
                state.activity(ActivityLevel::Error, message, refs).await;
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
                        if intent == CancelIntent::Skip {
                            db::files::set_status(
                                &mut tx,
                                job.file_id,
                                FileStatus::Skipped,
                                Some(USER_SKIP_REASON),
                                None,
                            )
                            .await?;
                        } else if !(ctx.converted_before()
                            && db::files::keep_converted(&mut tx, job.file_id).await?)
                        {
                            // A converted file stays done (it is still the
                            // converted one); others wait to be converted.
                            db::files::set_status(
                                &mut tx,
                                job.file_id,
                                FileStatus::Pending,
                                None,
                                None,
                            )
                            .await?;
                        }
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

/// A file moved or deleted while it was being converted. The watcher left
/// its row alone then; with watching on, take the row off the list now, as
/// the watcher would have (its job history goes with it). Otherwise the
/// file is marked as missing, and the next scan removes it (or finds it
/// again).
async fn remove_vanished(
    state: &AppState,
    job: &Job,
    ctx: &ExecContext,
    name: &str,
) -> anyhow::Result<()> {
    let path = ctx
        .file
        .as_ref()
        .map_or(job.file_path.as_str(), |f| f.path.as_str());
    let mut tx = state.db.write_tx().await?;
    let message = if state.settings().watch_folders {
        let removed = sqlx::query("DELETE FROM files WHERE id = ?")
            .bind(job.file_id.to_string())
            .execute(&mut *tx)
            .await?
            .rows_affected();
        tx.commit().await?;
        if removed == 0 {
            return Ok(());
        }
        state.emit(Event::FilesChanged {
            library_id: Some(job.library_id),
        });
        format!(
            "{name} was moved or deleted while it was being converted, so it was taken off the list."
        )
    } else {
        let error = format!("{MISSING_INPUT_ERROR}{path}. It may have been moved or deleted.");
        let exists = db::jobs::finish(
            &mut tx,
            job.id,
            JobState::Failed,
            &JobFinish {
                error: Some(error.clone()),
                ..JobFinish::default()
            },
        )
        .await?;
        if exists {
            db::files::set_status(&mut tx, job.file_id, FileStatus::Failed, None, Some(&error))
                .await?;
        }
        tx.commit().await?;
        if !exists {
            return Ok(());
        }
        format!("{name} was moved or deleted while it was being converted.")
    };
    state
        .activity(
            ActivityLevel::Info,
            message,
            ActivityRefs {
                library_id: Some(job.library_id),
                ..ActivityRefs::default()
            },
        )
        .await;
    Ok(())
}

/// Whether the file a job worked on is gone while its library folder is
/// there (a disconnected share is not a deleted file).
async fn input_vanished(ctx: &ExecContext) -> bool {
    let (Some(file), Some(root)) = (&ctx.file, &ctx.library_root) else {
        return false;
    };
    let missing = matches!(
        tokio::fs::symlink_metadata(&file.path).await,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    );
    missing
        && library::root_unavailable(&root.to_string_lossy())
            .await
            .is_none()
}

/// Note on a conversion finished by start-up recovery.
const RESUMED_NOTE: &str = "Chrysopoeia stopped just as the new file was being put in place. \
    The new file was already complete, so it was kept";

/// Start-up: finish the conversions a crash stopped while their result was
/// being put in place, so their new file is recorded instead of being made
/// again (or reported as a conflict with Chrysopoeia's own file). Runs
/// before interrupted jobs are re-queued and before leftover backups are
/// put back. Returns how many were finished.
pub async fn complete_interrupted(state: &AppState) -> u64 {
    let jobs = match db::jobs::interrupted(state.db.pool()).await {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!("could not look for interrupted conversions: {e}");
            return 0;
        }
    };
    let mut targets: HashMap<String, usize> = HashMap::new();
    for j in &jobs {
        if let Some(p) = &j.final_path {
            *targets.entry(p.clone()).or_default() += 1;
        }
    }
    let mut completed = 0;
    for InterruptedJob { job, final_path } in jobs {
        let Some(final_path) = final_path else {
            continue;
        };
        // Two interrupted jobs aiming at one name can't be told apart.
        if targets.get(&final_path).copied().unwrap_or(0) > 1 {
            continue;
        }
        let file = db::files::get(state.db.pool(), job.file_id, true)
            .await
            .ok()
            .flatten();
        let input = PathBuf::from(
            file.as_ref()
                .map_or(job.file_path.as_str(), |f| f.path.as_str()),
        );
        let target = PathBuf::from(&final_path);
        let replace = target.parent() == input.parent();
        let placed = if replace {
            match state
                .toolkit
                .resume_replace(input.clone(), target.clone(), job.id)
                .await
            {
                Ok(Interrupted::Placed {
                    size,
                    original_size,
                }) => Some((size, original_size)),
                Ok(Interrupted::NotPlaced) => None,
                Err(e) => {
                    tracing::warn!(job = %job.id, "could not check an interrupted conversion: {e:#}");
                    None
                }
            }
        } else {
            // Folder mode never touches the original, so there is no backup
            // to go by: the job had reached its last step, and the file is
            // in the output folder (the name was free when the job started).
            let there = tokio::fs::metadata(&target)
                .await
                .ok()
                .filter(std::fs::Metadata::is_file);
            let claimed = db::jobs::other_done_at(state.db.pool(), job.id, &final_path)
                .await
                .unwrap_or(true);
            match there {
                Some(meta) if job.stage == JobStage::Finalizing && !claimed => {
                    Some((meta.len(), job.input_size))
                }
                _ => None,
            }
        };
        let Some((size, original_size)) = placed else {
            continue;
        };
        let lib = db::libraries::get(state.db.pool(), job.library_id)
            .await
            .ok()
            .flatten();
        let ctx = ExecContext {
            file,
            library_root: lib.as_ref().map(|l| PathBuf::from(&l.path)),
            library_name: lib.map(|l| l.name),
            output_mode: if replace {
                OutputMode::Replace
            } else {
                OutputMode::Folder
            },
        };
        let outcome = JobOutcome::Done {
            output_path: target,
            output_size: size,
            original_size,
            encoder: job.encoder.clone().unwrap_or_default(),
            hw_api: job.hw_api.unwrap_or(HwApi::Software),
            attempt: job.attempt.max(1),
            validation: None,
            command: String::new(),
            notes: vec![RESUMED_NOTE.to_string()],
        };
        tracing::debug!(job = %job.id, file = %job.file_path, "finished a conversion the last stop interrupted");
        record(state, &job, Disposition::Finished(outcome), None, &ctx).await;
        completed += 1;
    }
    completed
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
