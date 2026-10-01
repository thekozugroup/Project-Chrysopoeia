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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use chrono::{DateTime, Local, Timelike, Utc};
use chrysopoeia_core::encoder::VIDEO_ENCODERS;
use chrysopoeia_core::{
    ActivityLevel, EncoderCandidate, Event, FileStatus, HwApi, Job, JobProgress, JobStage,
    JobState, MaxJobsSource, MediaFile, OutputMode, ProblemKind, QueueState, Settings,
    TranscodeProfile,
};
use chrysopoeia_scanner::WatchEvent;
use chrysopoeia_worker::finalize::{Interrupted, final_output_path, output_name_taken};
use chrysopoeia_worker::run::Unfinished;
use chrysopoeia_worker::slow_fs::{self, NotAnswering, WATCH_INTERVAL};
use chrysopoeia_worker::{Decision, JobOutcome, JobSpec, RunConfig};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::db::activity::ActivityRefs;
use crate::db::files::{MISSING_INPUT_ERROR, ReplacedFile, missing_input_error};
use crate::db::jobs::{DestinationOwner, InterruptedJob, JobFinish};
use crate::db::{self};
use crate::format;
use crate::services::fs_guard;
use crate::services::library::{
    self, PROBE_TIMEOUT, USER_SKIP_REASON, probe_error_message, probe_problem, still_settling,
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
/// How long the file a job starts on may take to answer before its library
/// counts as offline. Generous: an array disk that has to spin up first
/// takes several seconds.
const INPUT_CHECK_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(1)
} else {
    Duration::from_secs(30)
};
/// How long the last looks at a finished job's new file may still take
/// once the job is cancelled.
const CANCEL_GRACE: Duration = Duration::from_secs(2);
/// How long a folder may take to list a job's leftovers.
const LEFTOVER_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a file or folder that stopped answering may take to answer
/// when it is checked again (every [`OFFLINE_RECHECK`]).
const RECHECK_TIMEOUT: Duration = Duration::from_secs(3);
/// How long start-up recovery waits for a folder before it leaves the job
/// to look for itself when it runs again.
const STARTUP_CHECK_TIMEOUT: Duration = if cfg!(test) {
    Duration::from_secs(1)
} else {
    Duration::from_secs(10)
};
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
    /// What stopped answering, when it isn't the library folder itself (a
    /// file in it, the work folder, the output folder): it must answer too
    /// before the jobs go on.
    check: Option<PathBuf>,
}

/// A job that ended while its new file was still being put in place (see
/// `chrysopoeia_worker::run::take_unfinished`).
struct PlacingJob {
    file_id: Uuid,
    /// Asks it to undo what it did at its next safe point.
    stop: Arc<AtomicBool>,
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
    /// Jobs put back once after the disk filled up while other jobs were
    /// writing to it too (see [`space_was_shared`]).
    disk_retried: std::sync::Mutex<HashSet<Uuid>>,
    /// Jobs whose new file was still being put in place when they ended, by
    /// job id: their file isn't converted again until that is over.
    placing: std::sync::Mutex<HashMap<Uuid, PlacingJob>>,
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
            disk_retried: std::sync::Mutex::new(HashSet::new()),
            placing: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Why a library's jobs are waiting for its folder, if they are.
    pub fn offline_reason(&self, library_id: Uuid) -> Option<String> {
        lock(&self.offline)
            .get(&library_id)
            .map(|o| o.reason.clone())
    }

    /// Mark a library's folder offline (and `check`, when what stopped
    /// answering is something else). Returns true when it wasn't already.
    fn mark_offline(&self, library_id: Uuid, reason: String, check: Option<PathBuf>) -> bool {
        lock(&self.offline)
            .insert(
                library_id,
                Offline {
                    reason,
                    next_check: Instant::now() + OFFLINE_RECHECK,
                    check,
                },
            )
            .is_none()
    }

    /// Ask the new file of job `job_id`, still being put in place after the
    /// job ended, to be undone at its next safe point (the job was
    /// cancelled). Returns false when it isn't being put in place.
    pub fn stop_placing(&self, job_id: Uuid) -> bool {
        match lock(&self.placing).get(&job_id) {
            Some(p) => {
                p.stop.store(true, Ordering::SeqCst);
                true
            }
            None => false,
        }
    }

    /// Whether job `job_id`'s new file is still being put in place after the
    /// job ended.
    pub fn is_placing(&self, job_id: Uuid) -> bool {
        lock(&self.placing).contains_key(&job_id)
    }

    /// Files whose earlier job's new file is still being put in place.
    fn placing_files(&self) -> Vec<Uuid> {
        lock(&self.placing).values().map(|p| p.file_id).collect()
    }

    /// Check an offline library again the next time the queue looks for
    /// work (tests; normally every [`OFFLINE_RECHECK`]).
    #[cfg(test)]
    pub(crate) fn recheck_now(&self, library_id: Uuid) {
        if let Some(o) = lock(&self.offline).get_mut(&library_id) {
            o.next_check = Instant::now();
        }
    }

    /// The recorded cancel intent of a running job.
    pub(crate) fn intent_of(&self, job_id: Uuid) -> Option<CancelIntent> {
        lock(&self.running).get(&job_id).and_then(|j| j.intent)
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

    /// The jobs running right now.
    pub fn running_ids(&self) -> Vec<Uuid> {
        lock(&self.running).keys().copied().collect()
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
        // Nor a file whose earlier job is still putting its new file in
        // place (on a share that stopped answering).
        let skip_files = d.placing_files();
        let Some(job) =
            db::jobs::claim_next(&state.db, &skip_libraries, &skip_jobs, &skip_files).await?
        else {
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
    let due: Vec<(Uuid, Option<PathBuf>)> = {
        let now = Instant::now();
        lock(&state.dispatcher.offline)
            .iter()
            .filter(|(_, o)| o.next_check <= now)
            .map(|(id, o)| (*id, o.check.clone()))
            .collect()
    };
    for (id, check) in due {
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
        let problem = match library::root_unavailable(&lib.path).await {
            Some(reason) => Some(reason),
            // What stopped answering inside it (or elsewhere) answers too.
            None => match &check {
                Some(path) if fs_guard::metadata(path, RECHECK_TIMEOUT).await.is_none() => {
                    Some(stuck_reason(&lib.path, path))
                }
                _ => None,
            },
        };
        match problem {
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

        let (disposition, mut ctx) = execute(&state, &job, cancel.clone()).await;
        // Its new file may still be being put in place (a share stopped
        // answering, or Cancel came meanwhile): that goes on by itself.
        let unfinished = state.toolkit.take_unfinished(job.id);
        let disposition = space_was_shared(&state, &job, disposition);
        // Every look at the disk from here on is bounded and gives way to
        // Cancel and Stop: the job holds its slot until it is recorded.
        let disposition = after_failure(&disposition, &mut ctx, &cancel)
            .await
            .unwrap_or(disposition);
        prepare_record(&state, &disposition, &mut ctx, &cancel).await;
        // A file moved during its conversion (a folder renamed by Sonarr or
        // Radarr, say) leaves the temp file written next to it in the
        // folder's new place, where this job can't find it (the converter
        // then reports the new file missing): once the job has ended, the
        // library is searched for this job's leftovers.
        let may_have_left_files = matches!(
            &disposition,
            Disposition::Finished(JobOutcome::Failed { .. })
        ) && ctx.worker_ran
            && ctx.input_gone
            && run_config(&state, &state.settings()).temp_dir.is_none();
        let offline = matches!(
            &disposition,
            Disposition::Requeue(Requeue::LibraryOffline { .. })
        );
        // The slot stays taken until the result is recorded.
        let recorded = record(&state, &job, disposition, &ctx).await;
        if let Some(unfinished) = unfinished {
            // Before the slot is freed, so the file isn't started again
            // while its new file may still be moving in.
            hold_placing(&state, &job, unfinished, ctx.clone(), &recorded).await;
        }
        // Free the slot before announcing, so the queue state is current.
        drop(guard);
        if may_have_left_files && !state.shutdown.is_cancelled() {
            let (state, library_id, job_id) = (state.clone(), job.library_id, job.id);
            tokio::spawn(async move {
                library::sweep_job_leftovers(&state, library_id, job_id).await;
            });
        }
        state.broadcast_job(job.id).await;
        state.broadcast_file(job.file_id).await;
        state.broadcast_library(job.library_id).await;
        state.broadcast_stats().await;
        state.broadcast_queue_state().await;
        // Watch events for the file were ignored while it was being
        // converted (an upgrade that replaced it, for instance); look at it
        // again now. Nothing happens when it is as the database says (and
        // a library that stopped answering is left alone).
        if let Some(file) = &ctx.file
            && !offline
            && !state.shutdown.is_cancelled()
            && let Err(e) =
                library::handle_watch_event(&state, WatchEvent::Upserted(PathBuf::from(&file.path)))
                    .await
        {
            tracing::debug!(job = %job.id, "could not look at the file again: {e:#}");
        }
    });
}

/// A job that failed while its share stopped answering (which may well be
/// why it failed) goes back to the queue with its library offline instead;
/// one whose file is gone is noted in `ctx`. Cancel and Stop win over the
/// check (the job then ends as they say). `None` keeps `disposition`.
async fn after_failure(
    disposition: &Disposition,
    ctx: &mut ExecContext,
    cancel: &CancellationToken,
) -> Option<Disposition> {
    let Disposition::Finished(JobOutcome::Failed { error, .. }) = disposition else {
        return None;
    };
    if error.starts_with(MISSING_INPUT_ERROR) {
        return None;
    }
    let now = tokio::select! {
        now = input_now(ctx) => now,
        () = cancel.cancelled() => return Some(Disposition::Finished(JobOutcome::Cancelled)),
    };
    match now {
        InputNow::There => None,
        InputNow::Gone => {
            ctx.input_gone = true;
            None
        }
        InputNow::Unreachable { reason, check } => {
            Some(Disposition::Requeue(Requeue::LibraryOffline {
                reason,
                check,
            }))
        }
    }
}

/// Look at the disk once for what recording `disposition` needs (the new
/// file of a finished replacement), so that retries of the record, and the
/// transaction, never wait for a share.
async fn prepare_record(
    state: &AppState,
    disposition: &Disposition,
    ctx: &mut ExecContext,
    cancel: &CancellationToken,
) {
    if let Disposition::Finished(JobOutcome::Done {
        output_path,
        output_size,
        ..
    }) = disposition
    {
        ctx.replaced = replaced_file(state, ctx, output_path, *output_size, cancel).await;
    }
}

/// The new file after a replacement, as the file list should show it. Its
/// size and date come from the disk and its contents from a fresh probe,
/// when the disk answers in time (else from the job: the next scan reads
/// the rest). Cancel only cuts the looking short: the file is in place.
async fn replaced_file(
    state: &AppState,
    ctx: &ExecContext,
    output_path: &Path,
    output_size: u64,
    cancel: &CancellationToken,
) -> Option<ReplacedFile> {
    if ctx.output_mode != OutputMode::Replace {
        return None;
    }
    let p = output_path.to_str()?;
    // A Cancel that came too late (the file is in place) still gets the
    // usual look, unless the disk is slow to give it.
    let cut_short = async {
        cancel.cancelled().await;
        tokio::time::sleep(CANCEL_GRACE).await;
    };
    tokio::pin!(cut_short);
    let meta = tokio::select! {
        m = fs_guard::metadata(output_path, INPUT_CHECK_TIMEOUT) => m.and_then(Result::ok),
        () = &mut cut_short => None,
    };
    let probe = if meta.is_some() {
        let watched = [output_path.to_path_buf()];
        tokio::select! {
            p = state.toolkit.probe_file(output_path.to_path_buf(), PROBE_TIMEOUT) => p.ok(),
            () = &mut cut_short => None,
            _ = slow_fs::first_unanswered(&watched, WATCH_INTERVAL, INPUT_CHECK_TIMEOUT) => None,
        }
    } else {
        None
    };
    Some(ReplacedFile {
        path: p.to_string(),
        relative_path: relative_to(ctx.library_root.as_deref(), output_path),
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
}

/// A job ended while its new file was still being put in place: a share
/// stopped answering in the middle of it, or the job was cancelled then.
/// A rename that has started can't be called back, and abandoning it would
/// leave the files out of step with the database, so it is left to finish
/// on its own thread. Meanwhile the job's file is held back (no other job
/// starts on it), and when it ends:
///
/// - the new file took its place: the job is recorded as done (whatever it
///   was recorded as before, since that is what happened to the file);
/// - it was undone (the job was cancelled or stopped before the new file
///   took its place), or it failed with the original kept: nothing more to
///   record; a job back in the queue converts the file again.
///
/// If the server stops first, the next start finds the job's backup next to
/// the new file and finishes the record (see [`complete_interrupted`], and
/// the job's own look when it runs again in [`execute`]).
async fn hold_placing(
    state: &AppState,
    job: &Job,
    unfinished: Unfinished,
    ctx: ExecContext,
    recorded: &Disposition,
) {
    let stop = unfinished.stop_flag();
    // Cancelled or stopped: undo it if it isn't too late.
    if matches!(recorded, Disposition::Finished(JobOutcome::Cancelled)) {
        stop.store(true, Ordering::SeqCst);
    }
    lock(&state.dispatcher.placing).insert(
        job.id,
        PlacingJob {
            file_id: job.file_id,
            stop: Arc::clone(&stop),
        },
    );
    let name = ctx
        .file
        .as_ref()
        .map_or(job.file_name.as_str(), |f| f.file_name.as_str())
        .to_string();
    if let Disposition::Requeue(Requeue::LibraryOffline { .. }) = recorded {
        state
            .activity(
                ActivityLevel::Warning,
                format!(
                    "The converted {name} was being put in place when its folder stopped \
                     answering. It is finished when the folder answers again; until then the \
                     original is kept safe."
                ),
                ActivityRefs {
                    file_id: Some(job.file_id),
                    job_id: Some(job.id),
                    library_id: Some(job.library_id),
                },
            )
            .await;
    }
    let (state, job) = (state.clone(), job.clone());
    tokio::spawn(async move {
        let outcome = unfinished.outcome().await;
        if let JobOutcome::Done {
            output_path,
            output_size,
            original_size,
            encoder,
            hw_api,
            attempt,
            validation,
            command,
            mut notes,
        } = outcome
        {
            notes.push(if stop.load(Ordering::SeqCst) {
                "It was stopped while the new file was being put in place, but that had gone \
                 too far to undo, so it was finished"
                    .to_string()
            } else {
                "Its folder stopped answering while the new file was being put in place; that \
                 was finished when the folder answered again"
                    .to_string()
            });
            let mut ctx = ctx;
            let never = CancellationToken::new();
            ctx.replaced = replaced_file(&state, &ctx, &output_path, output_size, &never).await;
            let outcome = JobOutcome::Done {
                output_path,
                output_size,
                original_size,
                encoder,
                hw_api,
                attempt,
                validation,
                command,
                notes,
            };
            record(&state, &job, Disposition::Finished(outcome), &ctx).await;
        } else {
            tracing::debug!(job = %job.id, "the new file wasn't put in place: {outcome:?}");
        }
        lock(&state.dispatcher.placing).remove(&job.id);
        state.dispatcher.wake();
        state.broadcast_job(job.id).await;
        state.broadcast_file(job.file_id).await;
        state.broadcast_library(job.library_id).await;
        state.broadcast_stats().await;
        state.broadcast_queue_state().await;
    });
}

/// How long a job put back after running out of disk space waits before
/// it may start again.
const DISK_RETRY_DELAY: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(60)
};

/// A job that ran out of disk space while other jobs were writing to the
/// same disks may well fit alone: put it back once (the space reservation
/// then makes it wait its turn) instead of failing it for good.
fn space_was_shared(state: &AppState, job: &Job, disposition: Disposition) -> Disposition {
    let full = matches!(
        &disposition,
        Disposition::Finished(JobOutcome::Failed {
            problem: ProblemKind::DiskFull,
            ..
        })
    );
    // This job still counts as running.
    if full
        && state.dispatcher.running_count() > 1
        && !state.shutdown.is_cancelled()
        && lock(&state.dispatcher.disk_retried).insert(job.id)
    {
        return Disposition::Requeue(Requeue::DiskShared);
    }
    disposition
}

/// What `apply_outcome` needs to know about how the job ran.
#[derive(Debug, Clone)]
struct ExecContext {
    file: Option<MediaFile>,
    library_root: Option<PathBuf>,
    library_name: Option<String>,
    output_mode: OutputMode,
    /// The worker ran (so it may have written a temp file).
    worker_ran: bool,
    /// The original has other hard links and is being replaced: its data
    /// stays on disk through them, so the conversion saves no space.
    shared_original: bool,
    /// The job failed and its file is gone (moved or deleted meanwhile)
    /// while its library folder is there.
    input_gone: bool,
    /// After a replacement: the new file as the file list should show it
    /// (see [`prepare_record`]).
    replaced: Option<ReplacedFile>,
    /// The goal the job ran with (`None`: not known, e.g. an earlier run
    /// from before it was recorded). When it ends under another goal, its
    /// file is decided again (see [`follow_goal_change`]).
    profile: Option<TranscodeProfile>,
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
    /// The library folder can't be reached (the reason says why), or
    /// `check` stopped answering (a file in it, the work folder, the output
    /// folder).
    LibraryOffline {
        reason: String,
        check: Option<PathBuf>,
    },
    /// The disk filled up while other jobs were writing to it too.
    DiskShared,
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

fn failed(problem: ProblemKind, error: impl Into<String>) -> JobOutcome {
    JobOutcome::Failed {
        error: error.into(),
        problem,
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
        worker_ran: false,
        shared_original: false,
        input_gone: false,
        replaced: None,
        profile: None,
    };
    let done = |outcome: JobOutcome, ctx: ExecContext| (Disposition::Finished(outcome), ctx);
    let file = match db::files::get(state.db.pool(), job.file_id, true).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            return done(
                failed(
                    ProblemKind::SourceChanged,
                    "This file is no longer in the library.",
                ),
                ctx,
            );
        }
        Err(e) => {
            tracing::error!(job = %job.id, "could not load the file: {e}");
            return done(
                failed(ProblemKind::Other, crate::error::INTERNAL_MESSAGE),
                ctx,
            );
        }
    };
    ctx.file = Some(file.clone());
    let lib = match db::libraries::get(state.db.pool(), file.library_id).await {
        Ok(Some(l)) => l,
        Ok(None) => return done(JobOutcome::Cancelled, ctx),
        Err(e) => {
            tracing::error!(job = %job.id, "could not load the library: {e}");
            return done(
                failed(ProblemKind::Other, crate::error::INTERNAL_MESSAGE),
                ctx,
            );
        }
    };
    ctx.library_root = Some(PathBuf::from(&lib.path));
    ctx.library_name = Some(lib.name.clone());
    ctx.profile = Some(lib.profile.clone());

    let input = PathBuf::from(&file.path);
    let offline = |reason: String, check: Option<PathBuf>, ctx: ExecContext| {
        (
            Disposition::Requeue(Requeue::LibraryOffline { reason, check }),
            ctx,
        )
    };
    // Every look at the disk below gives way to Cancel and Stop: a share
    // that stopped answering would otherwise keep this job (and its slot)
    // "preparing" for as long as it hangs.
    macro_rules! or_cancelled {
        ($e:expr) => {
            tokio::select! {
                v = $e => v,
                () = cancel.cancelled() => return done(JobOutcome::Cancelled, ctx),
            }
        };
    }

    // An earlier run of this job may have ended while its new file was
    // being put in place (the server stopped, or its share stopped
    // answering then): what is on the disk tells whether it got there.
    match or_cancelled!(resume_earlier_run(state, job, &input, &lib.path)) {
        Resumed::NotPlaced => {}
        Resumed::Placed(outcome) => {
            // That run's goal, not the one this run would have used.
            ctx.profile = earlier_profile(state, job.id).await;
            return done(outcome, ctx);
        }
        Resumed::Unreachable { reason, check } => return offline(reason, check, ctx),
    }

    let mut restored = false;
    let meta = loop {
        let looked = or_cancelled!(fs_guard::metadata(&input, INPUT_CHECK_TIMEOUT));
        match looked {
            None => {
                let reason = or_cancelled!(offline_reason(&lib.path, &input));
                return offline(reason, Some(input.clone()), ctx);
            }
            Some(Ok(m)) if m.is_file() => break m,
            Some(_) => {
                // A whole library missing is a disconnected drive or share,
                // not a deleted file: wait for it instead of failing every
                // job.
                if let Some(reason) = or_cancelled!(library::root_unavailable(&lib.path)) {
                    return offline(reason, None, ctx);
                }
                // This job's own earlier run may have left the original
                // moved aside when the server stopped (its folder was out of
                // reach at the start): put it back and go on.
                let dirs: Vec<PathBuf> =
                    input.parent().map(Path::to_path_buf).into_iter().collect();
                if !restored && !dirs.is_empty() {
                    let recovered = or_cancelled!(recover_job_leftovers(state, job.id, &dirs));
                    let Some(recovered) = recovered else {
                        let reason = or_cancelled!(offline_reason(&lib.path, &dirs[0]));
                        return offline(reason, Some(dirs[0].clone()), ctx);
                    };
                    if recovered.contains(&input) {
                        restored = true;
                        continue;
                    }
                }
                return done(
                    failed(ProblemKind::SourceChanged, missing_input_error(&file.path)),
                    ctx,
                );
            }
        }
    };
    #[cfg(unix)]
    {
        ctx.shared_original = ctx.output_mode == OutputMode::Replace
            && std::os::unix::fs::MetadataExt::nlink(&meta) > 1;
    }
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
        _ => {
            // ffprobe reading a file whose share stopped answering would
            // only give up after its own timeout; the share is noticed
            // sooner.
            let watched = [input.clone()];
            let probed = or_cancelled!(async {
                tokio::select! {
                    p = state.toolkit.probe_file(input.clone(), PROBE_TIMEOUT) => Ok(p),
                    stuck = slow_fs::first_unanswered(&watched, WATCH_INTERVAL, INPUT_CHECK_TIMEOUT) => Err(stuck),
                }
            });
            match probed {
                Ok(Ok(p)) => {
                    if let Err(e) =
                        db::files::update_probe(state.db.pool(), file.id, meta.len(), modified, &p)
                            .await
                    {
                        tracing::warn!(job = %job.id, "could not store the new probe: {e}");
                    }
                    p
                }
                Ok(Err(e)) => return done(failed(probe_problem(&e), probe_error_message(&e)), ctx),
                Err(stuck) => {
                    let reason = or_cancelled!(offline_reason(&lib.path, &stuck));
                    return offline(reason, Some(stuck), ctx);
                }
            }
        }
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
    let converts = would_convert(state, &probe, &lib.profile, job.force, settings.output_mode);
    let preference_problem = hw.as_deref().filter(|_| converts).and_then(|hw| {
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
        return done(
            failed(
                ProblemKind::HardwareUnavailable,
                preference_failure(problem),
            ),
            ctx,
        );
    }
    let preference_problem = preference_problem.map(|(problem, _)| problem);

    let cfg = run_config(state, &settings);
    // Where the result goes (so start-up recovery can finish the
    // replacement if the server stops while it is being put in place) and
    // the goal it is made for. Another file being converted to the same
    // name right now (the same relative path in two libraries saved to one
    // output folder) would only have one of them refused at the end: this
    // one is refused now, before it is encoded. (A file that won't be
    // converted puts nothing anywhere.)
    let final_path = final_output_path(
        &input,
        lib.profile.container,
        cfg.output_mode,
        cfg.output_folder.as_deref(),
        Path::new(&lib.path),
    );
    let goal = db::files::profile_json(&lib.profile).ok();
    if let Some(p) = final_path.to_str() {
        let place = converts.then_some(p);
        match db::jobs::claim_destination(&state.db, job.id, file.id, place, goal.as_deref()).await
        {
            Ok(None) => {}
            Ok(Some(owner)) => {
                return done(
                    failed(
                        ProblemKind::Destination,
                        name_taken_error(
                            &owner,
                            lib.id,
                            &final_path,
                            cfg.output_mode,
                            cfg.output_folder.as_deref(),
                        ),
                    ),
                    ctx,
                );
            }
            Err(e) => {
                tracing::warn!(job = %job.id, "could not store where the result goes: {e}");
            }
        }
    }
    let (output_mode, output_folder) = (cfg.output_mode, cfg.output_folder.clone());
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
    ctx.worker_ran = true;
    let outcome = match state.toolkit.run_job(cfg, spec, tx, cancel.clone()).await {
        Ok(outcome) => outcome,
        Err(_) => failed(
            ProblemKind::Other,
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
    // The name in the output folder was taken: say whose file has it.
    let outcome = match outcome {
        JobOutcome::Failed {
            error,
            problem: ProblemKind::Destination,
            log_tail,
            command,
            encoder,
            attempt,
            validation,
        } if output_mode == OutputMode::Folder && output_name_taken(&error) => {
            let owner = match final_path.to_str() {
                Some(p) => db::jobs::destination_owner(state.db.pool(), p, file.id)
                    .await
                    .unwrap_or_else(|e| {
                        tracing::debug!(job = %job.id, "could not look for the other file: {e}");
                        None
                    }),
                None => None,
            };
            JobOutcome::Failed {
                error: owner.map_or(error, |owner| {
                    name_taken_error(
                        &owner,
                        lib.id,
                        &final_path,
                        output_mode,
                        output_folder.as_deref(),
                    )
                }),
                problem: ProblemKind::Destination,
                log_tail,
                command,
                encoder,
                attempt,
                validation,
            }
        }
        outcome => outcome,
    };
    // The sender is gone once `run_job` returns, so the forwarder drains and
    // ends; don't wait on a worker that leaked a clone of it.
    if tokio::time::timeout(Duration::from_secs(2), forwarder)
        .await
        .is_err()
    {
        tracing::debug!(job = %job.id, "progress forwarder did not finish in time");
    }
    // A file or folder that stopped answering made the job stop: it waits
    // in the queue, with its library offline, until it answers again.
    if let JobOutcome::NotResponding { path } = &outcome {
        let reason = tokio::select! {
            reason = offline_reason(&lib.path, path) => reason,
            () = cancel.cancelled() => stuck_reason(&lib.path, path),
        };
        return offline(reason, Some(path.clone()), ctx);
    }
    done(outcome, ctx)
}

/// The error of a job whose new file would take the name another file's
/// conversion (`owner`) has, or is about to have, at `final_path`: two
/// libraries saved to one output folder mirror their folders there without
/// the library's name, so the same relative path in both gives one name
/// (and two originals that differ only by extension give one name too).
fn name_taken_error(
    owner: &DestinationOwner,
    library_id: Uuid,
    final_path: &Path,
    mode: OutputMode,
    output_folder: Option<&Path>,
) -> String {
    let file_name = final_path.file_name().map_or_else(
        || final_path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let (verb, untouched) = if owner.running {
        ("is being converted", "")
    } else {
        ("was converted", " and that file wasn't overwritten")
    };
    match (mode, output_folder) {
        (OutputMode::Folder, Some(folder)) => {
            let name = final_path
                .strip_prefix(folder)
                .map_or_else(|_| file_name.clone(), |r| r.display().to_string());
            let folder = folder.display();
            if owner.library_id != library_id {
                let lib = &owner.library_name;
                if owner.running {
                    format!(
                        "Another library's file, from {lib}, is being converted to the same \
                         name, \"{name}\", in the output folder {folder}, so this file wasn't \
                         converted. Rename one of the two files, or convert one library at a \
                         time, each to a different output folder chosen in Settings › Output."
                    )
                } else {
                    format!(
                        "Another library's converted file, from {lib}, already uses the name \
                         \"{name}\" in the output folder {folder}, so this file wasn't \
                         converted and that file wasn't overwritten. Rename one of the two \
                         files, or convert one library at a time, each to a different output \
                         folder chosen in Settings › Output."
                    )
                }
            } else {
                format!(
                    "Another file in this library, \"{}\", {verb} to the same name, \"{name}\", \
                     in the output folder {folder}, so this file wasn't converted{untouched}. \
                     Rename one of the two files, then convert this one again.",
                    owner.relative_path
                )
            }
        }
        _ => {
            let other = Path::new(&owner.relative_path).file_name().map_or_else(
                || owner.relative_path.clone(),
                |n| n.to_string_lossy().into_owned(),
            );
            format!(
                "Another file in the same folder, \"{other}\", {verb} to the same name, \
                 \"{file_name}\", so this file wasn't converted{untouched}. Rename one of the \
                 two files, then convert this one again."
            )
        }
    }
}

/// The goal an earlier run of job `id` ran with, as it recorded it.
async fn earlier_profile(state: &AppState, id: Uuid) -> Option<TranscodeProfile> {
    let stored = db::jobs::profile(state.db.pool(), id)
        .await
        .unwrap_or_else(|e| {
            tracing::debug!(job = %id, "could not read the goal the job ran with: {e}");
            None
        });
    db::files::parse_profile(stored.as_deref())
}

/// How a job's end left its file, for [`follow_goal_change`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// Converted and recorded `done`; `probed`: the new file replaced the
    /// original and was probed, so the stored probe describes it.
    Converted { probed: bool },
    /// A file converted before stays `done` (the job had no new result).
    Kept,
    /// Skipped; `size_rule`: an encode was made and set aside (not small
    /// enough under the size rule).
    Skipped { size_rule: bool },
    /// Failed by a rule of the goal: the hardware chosen in Settings can't
    /// make its codec.
    FailedBySetting,
}

/// What [`follow_goal_change`] did with the file.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Followed {
    /// Queued again for the current goal.
    Queued,
    /// Back to `pending`: the current goal would convert it (auto-queue
    /// is off).
    Pending,
    /// Skipped with the current goal's reason.
    Skipped(String),
}

/// A job ended (`ended`) while its library's goal was no longer the one it
/// ran with (it changed while the job ran, maybe more than once): decide
/// its file again against the current goal, as the change did for the
/// files that weren't running, in the transaction that records the end.
///
/// - Skipped or failed by the goal's own rule: queued again when the
///   current goal converts it (`pending` without auto-queue), else skipped
///   with the current goal's reason. A size-rule skip stays (with the
///   current goal as its verdict's) when the change doesn't touch what the
///   size rule would say.
/// - Converted (or kept as converted) in place: queued again when the
///   current goal would convert the file as it is now (the usual rules
///   skip files already in an efficient format), with auto-queue on; it
///   stays `done` otherwise. Not in folder mode: the result already sits
///   in the output folder and a second conversion would need that name.
///
/// A job's goal is the one it was given, "Convert anyway" included: the
/// new decision uses the usual rules, and a job that ran with the current
/// goal is never decided again, so nothing loops. Nothing is done for a
/// job whose goal isn't known.
async fn follow_goal_change(
    conn: &mut sqlx::SqliteConnection,
    state: &AppState,
    job: &Job,
    ctx: &ExecContext,
    ended: Ended,
) -> anyhow::Result<Option<Followed>> {
    let Some(ran_with) = &ctx.profile else {
        return Ok(None);
    };
    let Some(current) = db::libraries::profile_conn(&mut *conn, job.library_id).await? else {
        return Ok(None);
    };
    if current == *ran_with {
        return Ok(None);
    }
    let goal = db::files::profile_json(&current)?;
    let settings = state.settings();
    let converted = match ended {
        Ended::Converted { probed } => {
            if !probed {
                return Ok(None);
            }
            true
        }
        Ended::Kept => true,
        Ended::Skipped { size_rule } => {
            if size_rule && !library::affects_output_size(ran_with, &current) {
                db::files::confirm_verdict(&mut *conn, job.file_id, &goal).await?;
                return Ok(None);
            }
            false
        }
        Ended::FailedBySetting => false,
    };
    let replacing =
        ctx.output_mode == OutputMode::Replace && settings.output_mode == OutputMode::Replace;
    if converted && !(replacing && settings.auto_queue) {
        return Ok(None);
    }
    let Some(file) = db::files::get_conn(&mut *conn, job.file_id, true).await? else {
        return Ok(None);
    };
    let Some(probe) = &file.probe else {
        return Ok(None);
    };
    let decision = match state.toolkit.decide(probe, &current) {
        Ok(decision) => decision,
        Err(_) => {
            tracing::warn!(job = %job.id, "could not decide the file again for the new goal");
            return Ok(None);
        }
    };
    let only_if = [FileStatus::Skipped, FileStatus::Failed];
    let followed = match decision {
        Decision::Transcode if settings.auto_queue => {
            let created = db::jobs::create(
                &mut *conn,
                &db::jobs::NewJob {
                    file_id: file.id,
                    library_id: file.library_id,
                    file_name: &file.file_name,
                    file_path: &file.path,
                    input_size: file.size_bytes,
                    priority: job.priority,
                    force: false,
                },
            )
            .await?;
            created.map(|_| Followed::Queued)
        }
        Decision::Transcode if !converted => db::files::set_status_where(
            &mut *conn,
            file.id,
            FileStatus::Pending,
            None,
            Some(&goal),
            &only_if,
        )
        .await?
        .then_some(Followed::Pending),
        Decision::Skip { reason } if !converted => db::files::set_status_where(
            &mut *conn,
            file.id,
            FileStatus::Skipped,
            Some(&reason),
            Some(&goal),
            &only_if,
        )
        .await?
        .then_some(Followed::Skipped(reason)),
        // A converted file the current goal leaves as it is stays done.
        _ => None,
    };
    Ok(followed)
}

/// Say in the feed what [`follow_goal_change`] did.
async fn announce_followed(
    state: &AppState,
    ctx: &ExecContext,
    name: &str,
    followed: Option<Followed>,
    refs: ActivityRefs,
) {
    let Some(followed) = followed else {
        return;
    };
    let library = ctx.library_name.as_deref().unwrap_or("this library");
    let changed = format!("The goal of {library} changed while {name} was being converted");
    let message = match followed {
        Followed::Queued => {
            state.dispatcher.wake();
            format!("{changed}, so it was queued again for the new goal.")
        }
        Followed::Pending => format!(
            "{changed}. The new goal would convert it, so it is back among the files to convert."
        ),
        Followed::Skipped(reason) => format!(
            "{changed}. Under the new goal it is left as it is: {}.",
            reason.trim_end_matches('.')
        ),
    };
    state.activity(ActivityLevel::Info, message, refs).await;
}

/// Whether the worker would convert this file (so it needs an encoder): the
/// decision the worker makes before its first attempt, including "Convert
/// anyway" and leaving alone a file whose replacement couldn't hold all its
/// subtitles or attachments. A decision that can't be made counts as
/// converting; the worker reports the problem.
fn would_convert(
    state: &AppState,
    probe: &chrysopoeia_core::ProbeInfo,
    profile: &chrysopoeia_core::TranscodeProfile,
    force: bool,
    output_mode: OutputMode,
) -> bool {
    match state.toolkit.decide(probe, profile) {
        Ok(Decision::Skip { .. }) if force => matches!(
            chrysopoeia_worker::decide_forced(probe, profile),
            Decision::Transcode
        ),
        Ok(Decision::Skip { .. }) => false,
        Ok(Decision::Transcode) => {
            force
                || output_mode != OutputMode::Replace
                || chrysopoeia_worker::replace_loss(probe, profile).is_none()
        }
        Err(_) => true,
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
///
/// A cancel recorded for the job (Cancel, Skip, a removed library) wins
/// over putting it back in the queue: a job the user cancelled ends
/// cancelled, even when its share stopped answering at the same moment.
/// The intent is read again on every try, so one that comes while the
/// result is being recorded counts too. Returns what was recorded.
async fn record(
    state: &AppState,
    job: &Job,
    disposition: Disposition,
    ctx: &ExecContext,
) -> Disposition {
    let mut delay = OUTCOME_RETRY_FIRST;
    let mut hard_failures = 0u32;
    let mut attempts = 0u32;
    let mut give_up_at: Option<Instant> = None;
    loop {
        let intent = state.dispatcher.intent_of(job.id);
        let disposition = match (&disposition, intent) {
            (
                Disposition::Requeue(_),
                Some(CancelIntent::User | CancelIntent::Skip | CancelIntent::Removed),
            ) => Disposition::Finished(JobOutcome::Cancelled),
            (d, _) => d.clone(),
        };
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
            return disposition;
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
                return disposition;
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
                return disposition;
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
                problem: Some(ProblemKind::Other),
                ..JobFinish::default()
            },
        )
        .await?;
        db::files::set_status(
            &mut tx,
            job.file_id,
            FileStatus::Failed,
            None,
            Some((error, ProblemKind::Other)),
        )
        .await?;
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
        Requeue::LibraryOffline { reason, check } => {
            state
                .dispatcher
                .mark_offline(job.library_id, reason.clone(), check.clone())
        }
        Requeue::Settling => {
            state
                .dispatcher
                .defer(job.id, Instant::now() + state.config.settle);
            false
        }
        Requeue::DiskShared => {
            state
                .dispatcher
                .defer(job.id, Instant::now() + DISK_RETRY_DELAY);
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
        Requeue::LibraryOffline { reason, .. } if newly_offline => {
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
        Requeue::LibraryOffline { .. } => {}
        Requeue::Settling => {
            tracing::debug!(job = %job.id, file = %job.file_path, "waiting for the file to finish copying");
        }
        Requeue::DiskShared => {
            tracing::info!(
                job = %job.id,
                "{} ran out of disk space while other conversions were running; it will be \
                 tried again on its own",
                crate::log_text(&job.file_name)
            );
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
            if !error.starts_with(MISSING_INPUT_ERROR) && ctx.input_gone =>
        {
            return remove_vanished(state, job, ctx, &name).await;
        }
        other => other,
    };
    match outcome {
        JobOutcome::Done {
            // Where it is was looked at before (see `prepare_record`).
            output_path: _,
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
            let replaced = ctx.replaced.clone();

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
            // A hard-linked original keeps its data on disk through its
            // other name (a seeding torrent): nothing was freed.
            let saved_total = if ctx.shared_original {
                ctx.file.as_ref().and_then(|f| f.saved_bytes).unwrap_or(0)
            } else {
                db::i64_of(first_original) - db::i64_of(current_size)
            };
            sqlx::query(
                "UPDATE files SET status = 'done', original_size_bytes = ?, saved_bytes = ?, \
                 skip_reason = NULL, error = NULL, problem = NULL, job_id = ?, updated_at = ? \
                 WHERE id = ?",
            )
            .bind(db::i64_of(first_original))
            .bind(saved_total)
            .bind(job.id.to_string())
            .bind(db::now_ts())
            .bind(job.file_id.to_string())
            .execute(&mut *tx)
            .await?;
            let saved_now = if ctx.shared_original {
                0
            } else {
                db::i64_of(original_size) - db::i64_of(output_size)
            };
            db::stats::add_savings(&mut tx, job.library_id, saved_now).await?;
            let probed = replaced.as_ref().is_some_and(|r| r.probe.is_some());
            let followed =
                follow_goal_change(&mut tx, state, job, ctx, Ended::Converted { probed }).await?;
            tx.commit().await?;

            let pct = format::percent(saved_now, original_size);
            let mut message = if ctx.shared_original {
                // Its note says why nothing was saved.
                format!("Converted {name}")
            } else if saved_now >= 0 {
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
            announce_followed(state, ctx, &name, followed, refs).await;
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
                // The verdict of the goal the job ran with.
                if let Some(ran_with) = &ctx.profile {
                    let goal = db::files::profile_json(ran_with)?;
                    db::files::confirm_verdict(&mut tx, job.file_id, &goal).await?;
                }
            }
            let ended = if kept_done {
                Ended::Kept
            } else {
                Ended::Skipped {
                    size_rule: output_size.is_some(),
                }
            };
            let followed = if exists {
                follow_goal_change(&mut tx, state, job, ctx, ended).await?
            } else {
                None
            };
            tx.commit().await?;
            if exists {
                let message = if kept_done {
                    format!("Kept {name} as it is: {reason}")
                } else {
                    format!("Skipped {name}: {reason}")
                };
                state.activity(ActivityLevel::Info, message, refs).await;
                announce_followed(state, ctx, &name, followed, refs).await;
            }
        }
        JobOutcome::Failed {
            error,
            problem,
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
                    problem: Some(problem),
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
                db::files::set_status(
                    &mut tx,
                    job.file_id,
                    FileStatus::Failed,
                    None,
                    Some((&error, problem)),
                )
                .await?;
            }
            let ended = if kept_done {
                Some(Ended::Kept)
            } else if problem == ProblemKind::HardwareUnavailable {
                Some(Ended::FailedBySetting)
            } else {
                None
            };
            let followed = match ended {
                Some(ended) if exists => {
                    follow_goal_change(&mut tx, state, job, ctx, ended).await?
                }
                _ => None,
            };
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
                announce_followed(state, ctx, &name, followed, refs).await;
            }
        }
        // Mapped to a requeue before it is recorded (see `execute`); should
        // one come here, it goes back to the queue all the same.
        JobOutcome::NotResponding { .. } => {
            let mut tx = state.db.write_tx().await?;
            db::jobs::requeue(&mut tx, job.id).await?;
            db::files::set_status(&mut tx, job.file_id, FileStatus::Queued, None, None).await?;
            tx.commit().await?;
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
        let error = missing_input_error(path);
        let exists = db::jobs::finish(
            &mut tx,
            job.id,
            JobState::Failed,
            &JobFinish {
                error: Some(error.clone()),
                problem: Some(ProblemKind::SourceChanged),
                ..JobFinish::default()
            },
        )
        .await?;
        if exists {
            db::files::set_status(
                &mut tx,
                job.file_id,
                FileStatus::Failed,
                None,
                Some((&error, ProblemKind::SourceChanged)),
            )
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

/// The file of a job that failed, as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
enum InputNow {
    /// It is there, and so is its library folder.
    There,
    /// It is gone while its library folder is there: moved or deleted.
    Gone,
    /// It, or its library folder, can't be reached (a disconnected share
    /// is not a deleted file): `check` is what didn't answer, if not the
    /// library folder.
    Unreachable {
        reason: String,
        check: Option<PathBuf>,
    },
}

/// Look at the file of a job that failed, with bounded checks (see
/// [`fs_guard`]).
async fn input_now(ctx: &ExecContext) -> InputNow {
    let (Some(file), Some(root)) = (&ctx.file, &ctx.library_root) else {
        return InputNow::There;
    };
    let root = root.to_string_lossy();
    let path = Path::new(&file.path);
    let looked = fs_guard::symlink_metadata(path, INPUT_CHECK_TIMEOUT).await;
    let Some(looked) = looked else {
        return InputNow::Unreachable {
            reason: offline_reason(&root, path).await,
            check: Some(path.to_path_buf()),
        };
    };
    if let Some(reason) = library::root_unavailable(&root).await {
        return InputNow::Unreachable {
            reason,
            check: None,
        };
    }
    match looked {
        Err(std::io::ErrorKind::NotFound) => InputNow::Gone,
        _ => InputNow::There,
    }
}

/// Why a library's jobs wait when `stuck` (in it, or a folder the jobs use)
/// didn't answer: the library folder's own problem, if it has one, else
/// that `stuck`'s folder isn't responding.
async fn offline_reason(lib_path: &str, stuck: &Path) -> String {
    match library::root_unavailable(lib_path).await {
        Some(reason) => reason,
        None => stuck_reason(lib_path, stuck),
    }
}

/// That the folder of `stuck` isn't responding: the library folder for
/// anything in the library, else the folder itself (the work folder, the
/// output folder).
fn stuck_reason(lib_path: &str, stuck: &Path) -> String {
    if stuck.starts_with(lib_path) {
        return library::not_responding(lib_path);
    }
    // A file (the temp file, say) is shown by its folder.
    let folder = match stuck.parent() {
        Some(parent) if stuck.extension().is_some() => parent,
        _ => stuck,
    };
    library::not_responding(&folder.to_string_lossy())
}

/// What an earlier run of a job left in place, as far as its new file goes.
#[derive(Debug)]
enum Resumed {
    /// Its new file isn't in place (or it never got that far).
    NotPlaced,
    /// Its new file was in place: the job is done.
    Placed(JobOutcome),
    /// Its folder doesn't answer.
    Unreachable {
        reason: String,
        check: Option<PathBuf>,
    },
}

/// An earlier run of `job` that stopped while its new file was being put in
/// place (the server stopped, or the share stopped answering then and the
/// server stopped before it answered again) left the original's backup
/// next to the new file: finish the record instead of converting the file
/// again. Replacements only (folder mode never touches the original, so it
/// leaves no backup to go by).
async fn resume_earlier_run(state: &AppState, job: &Job, input: &Path, lib_path: &str) -> Resumed {
    let Ok(Some(final_path)) = db::jobs::final_path(state.db.pool(), job.id).await else {
        return Resumed::NotPlaced;
    };
    let target = PathBuf::from(&final_path);
    if target.parent() != input.parent() {
        return Resumed::NotPlaced;
    }
    let checked = state
        .toolkit
        .resume_replace(
            input.to_path_buf(),
            target.clone(),
            job.id,
            INPUT_CHECK_TIMEOUT,
        )
        .await;
    match checked {
        Ok(Interrupted::Placed {
            size,
            original_size,
        }) => {
            tracing::debug!(job = %job.id, "an earlier run of this job had already put its new file in place");
            Resumed::Placed(resumed_outcome(job, target, size, original_size))
        }
        Ok(Interrupted::NotPlaced) => Resumed::NotPlaced,
        Err(e) if e.is::<NotAnswering>() => Resumed::Unreachable {
            reason: offline_reason(lib_path, input).await,
            check: Some(input.to_path_buf()),
        },
        Err(e) => {
            tracing::warn!(job = %job.id, "could not check an earlier run of this job: {e:#}");
            Resumed::NotPlaced
        }
    }
}

/// The outcome of a job whose earlier run turned out to have put its new
/// file at `target` before it was interrupted.
fn resumed_outcome(job: &Job, target: PathBuf, size: u64, original_size: u64) -> JobOutcome {
    JobOutcome::Done {
        output_path: target,
        output_size: size,
        original_size,
        encoder: job.encoder.clone().unwrap_or_default(),
        hw_api: job.hw_api.unwrap_or(HwApi::Software),
        attempt: job.attempt.max(1),
        validation: None,
        command: String::new(),
        notes: vec![RESUMED_NOTE.to_string()],
    }
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
    // Where each job may have left files of its own: its file's folder and
    // the folder its result was going to.
    let mut leftover_dirs: Vec<(Uuid, Vec<PathBuf>)> = Vec::new();
    for InterruptedJob { job, final_path } in jobs {
        let file = db::files::get(state.db.pool(), job.file_id, true)
            .await
            .ok()
            .flatten();
        let input = PathBuf::from(
            file.as_ref()
                .map_or(job.file_path.as_str(), |f| f.path.as_str()),
        );
        let mut dirs: Vec<PathBuf> = input.parent().map(Path::to_path_buf).into_iter().collect();
        if let Some(dir) = final_path.as_deref().and_then(|p| Path::new(p).parent())
            && !dirs.iter().any(|d| d == dir)
        {
            dirs.push(dir.to_path_buf());
        }
        leftover_dirs.push((job.id, dirs));
        let Some(final_path) = final_path else {
            continue;
        };
        // Two interrupted jobs aiming at one name can't be told apart.
        if targets.get(&final_path).copied().unwrap_or(0) > 1 {
            continue;
        }
        let target = PathBuf::from(&final_path);
        let replace = target.parent() == input.parent();
        // A folder that doesn't answer now is looked at when the job runs
        // again (replacements, see `resume_earlier_run`); start-up doesn't
        // wait for it.
        let placed = if replace {
            match state
                .toolkit
                .resume_replace(input.clone(), target.clone(), job.id, STARTUP_CHECK_TIMEOUT)
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
            let there = fs_guard::metadata(&target, STARTUP_CHECK_TIMEOUT)
                .await
                .and_then(Result::ok)
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
        let mut ctx = ExecContext {
            file,
            library_root: lib.as_ref().map(|l| PathBuf::from(&l.path)),
            library_name: lib.map(|l| l.name),
            output_mode: if replace {
                OutputMode::Replace
            } else {
                OutputMode::Folder
            },
            worker_ran: true,
            shared_original: false,
            input_gone: false,
            replaced: None,
            profile: earlier_profile(state, job.id).await,
        };
        let outcome = resumed_outcome(&job, target, size, original_size);
        tracing::debug!(job = %job.id, file = %job.file_path, "finished a conversion the last stop interrupted");
        let disposition = Disposition::Finished(outcome);
        prepare_record(state, &disposition, &mut ctx, &CancellationToken::new()).await;
        record(state, &job, disposition, &ctx).await;
        completed += 1;
    }
    // Whatever the last stop recorded and wherever the library search gets
    // to, an original the crash left moved aside is put back now (and its
    // job's temp files go), so the job finds its file when it runs again.
    for (job_id, dirs) in leftover_dirs {
        if recover_job_leftovers(state, job_id, &dirs).await.is_none() {
            tracing::info!(job = %job_id, "a folder didn't answer; the job's leftovers are looked for later");
        }
    }
    completed
}

/// Hand what a job left in `dirs` (its temp files, its backup of the
/// original) to crash recovery: the backup is put back when the original's
/// name is free. Folders that don't answer are left for the library search.
/// Returns the originals put back; `None` when putting them back didn't end
/// in time (the share stopped answering: a rename that started goes on by
/// itself and only puts an original back).
async fn recover_job_leftovers(
    state: &AppState,
    job_id: Uuid,
    dirs: &[PathBuf],
) -> Option<Vec<PathBuf>> {
    let mut found = Vec::new();
    for dir in dirs {
        let d = dir.clone();
        // One check per job and folder (see `fs_guard`).
        let key = dir.join(job_id.to_string());
        let listed = fs_guard::guarded("job_leftovers", &key, LEFTOVER_CHECK_TIMEOUT, move || {
            std::fs::read_dir(&d)
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| {
                            e.file_name()
                                .to_str()
                                .is_some_and(|n| chrysopoeia_core::paths::is_artifact_of(n, job_id))
                        })
                        .map(|e| e.path())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .await;
        found.extend(listed.unwrap_or_default());
    }
    if found.is_empty() {
        return Some(Vec::new());
    }
    tokio::time::timeout(
        LEFTOVER_CHECK_TIMEOUT,
        library::recover_leftovers(state, found, Some(job_id)),
    )
    .await
    .ok()
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
