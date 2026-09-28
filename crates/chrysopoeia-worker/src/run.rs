//! Job orchestration: plan, encode with fallbacks, verify, finalize.
//!
//! [`run_job`] walks one file through the stages:
//!
//! 1. **Preparing** — the input still exists, `decide` agrees there is work,
//!    the destination is free and the temp folder has room.
//! 2. **Transcoding** — the attempt chain: each candidate encoder in order;
//!    a hardware encoder that decodes on the GPU is retried with CPU
//!    decoding before moving on. The first success wins.
//! 3. The size rule (`min_savings_pct`) may discard the result.
//! 4. **Verifying** — [`crate::validate_output`]. A hardware result that
//!    fails verification moves on to the next attempt (GPU encoders can
//!    produce corrupt output); any other failure fails the job.
//! 5. **Finalizing** — [`finalize`] puts the file in place.
//!
//! The original is only touched by a successful finalize. The temp file is
//! removed on every other path, including cancellation and the job future
//! being dropped (a drop guard).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrysopoeia_core::{
    EncoderCandidate, HwApi, JobProgress, JobStage, OutputMode, ProbeInfo, TranscodeProfile,
    ValidationLevel, ValidationReport,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::ffmpeg::{
    DEFAULT_STALL_TIMEOUT, FfmpegCommand, FfmpegExit, compute_progress, display_command, run_ffmpeg,
};
use crate::finalize::{
    FinalizeRequest, destination_conflict, final_output_path, finalize, temp_output_path,
};
use crate::plan::{Decision, FfmpegPlan, PlanRequest};
use crate::validate::{ValidateRequest, human_bytes, validate_output_at};

/// Settings that apply to every job.
#[derive(Debug, Clone)]
pub struct RunConfig {
    /// The ffmpeg binary.
    pub ffmpeg: PathBuf,
    /// The ffprobe binary (used by verification).
    pub ffprobe: PathBuf,
    /// Scratch directory for encodes; `None` writes next to the source.
    pub temp_dir: Option<PathBuf>,
    /// How hard to check each result before it replaces anything.
    pub validation: ValidationLevel,
    /// Replace originals, or write into `output_folder`.
    pub output_mode: OutputMode,
    /// Destination root for `OutputMode::Folder`.
    pub output_folder: Option<PathBuf>,
    /// Give the new file the original's modification and access times.
    pub keep_file_dates: bool,
    /// Run ffmpeg under `nice -n 10` when available.
    pub low_priority: bool,
}

/// One file to process.
#[derive(Debug, Clone)]
pub struct JobSpec {
    /// The job's id (also names the temp and backup files).
    pub job_id: Uuid,
    /// The file's id, echoed in progress updates.
    pub file_id: Uuid,
    /// The original file.
    pub input: PathBuf,
    /// Root of the library the file belongs to (for `OutputMode::Folder`).
    pub library_root: PathBuf,
    /// What the scanner found in the file.
    pub probe: ProbeInfo,
    /// What the library wants the file converted into.
    pub profile: TranscodeProfile,
    /// Encoders to try in order (from `chrysopoeia_hwdetect::encoder_candidates`).
    pub candidates: Vec<EncoderCandidate>,
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq)]
pub enum JobOutcome {
    /// Converted, verified and put in place.
    Done {
        /// Where the new file is now.
        output_path: PathBuf,
        /// Size of the new file in bytes.
        output_size: u64,
        /// Size of the original in bytes.
        original_size: u64,
        /// ffmpeg encoder that produced the file.
        encoder: String,
        /// Acceleration API of that encoder.
        hw_api: HwApi,
        /// 1-based attempt number within the fallback chain.
        attempt: u32,
        /// The verification report (`None` when verification is off).
        validation: Option<ValidationReport>,
        /// The ffmpeg command line, shell-quoted for display.
        command: String,
        /// Plain-language notes about compromises and fallbacks.
        notes: Vec<String>,
    },
    /// Nothing written: no work needed, or the result was not worth keeping
    /// (e.g. not smaller). The original is untouched.
    Skipped {
        /// Short sentence for the UI, e.g. "Only 4% smaller — kept the original".
        reason: String,
        /// Encoder used, when an encode was made and discarded.
        encoder: Option<String>,
        /// Size of the discarded encode, if any.
        output_size: Option<u64>,
    },
    /// The original is untouched.
    Failed {
        /// Plain-language reason.
        error: String,
        /// Last ffmpeg output lines, when ffmpeg failed.
        log_tail: Option<String>,
        /// The ffmpeg command line of the last attempt, if one ran.
        command: Option<String>,
        /// Encoder of the last attempt, if any.
        encoder: Option<String>,
        /// Number of the last attempt (0 when none ran).
        attempt: u32,
        /// The verification report when verification failed.
        validation: Option<ValidationReport>,
    },
    /// Stopped on request; no files changed.
    Cancelled,
}

/// Builds the ffmpeg command for one attempt (normally [`crate::plan::build_plan`]).
pub type PlanFn = dyn Fn(&PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> + Send + Sync;

/// Decides whether a file needs work (normally [`crate::plan::decide`]).
pub type DecideFn = dyn Fn(&ProbeInfo, &TranscodeProfile) -> Decision + Send + Sync;

/// Minimum time between two throttled progress updates.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(500);

/// How long a stage-change update may wait for room in the progress channel.
const STAGE_SEND_TIMEOUT: Duration = Duration::from_secs(2);

/// Run one job to completion. Progress is sent at most ~2 times per second.
/// Never panics on bad input; all problems become `Failed`.
pub async fn run_job(
    cfg: &RunConfig,
    spec: &JobSpec,
    progress: mpsc::Sender<JobProgress>,
    cancel: CancellationToken,
) -> JobOutcome {
    run_job_with(
        cfg,
        spec,
        &crate::plan::build_plan,
        &crate::plan::decide,
        progress,
        cancel,
    )
    .await
}

/// [`run_job`] with the planning functions supplied by the caller. Tests use
/// this to drive the attempt chain with simple, predictable commands.
pub async fn run_job_with(
    cfg: &RunConfig,
    spec: &JobSpec,
    planner: &PlanFn,
    decider: &DecideFn,
    progress: mpsc::Sender<JobProgress>,
    cancel: CancellationToken,
) -> JobOutcome {
    let reporter = Reporter::new(progress, spec.job_id, spec.file_id);
    let job = Job {
        cfg,
        spec,
        planner,
        decider,
        reporter: &reporter,
        cancel: &cancel,
    };
    let outcome = job.run().await;
    log_outcome(spec, &outcome);
    outcome
}

fn log_outcome(spec: &JobSpec, outcome: &JobOutcome) {
    let file = spec.input.display();
    match outcome {
        JobOutcome::Done {
            encoder,
            attempt,
            original_size,
            output_size,
            ..
        } => tracing::info!(
            job = %spec.job_id, %file, %encoder, attempt,
            "done: {} → {}", human_bytes(*original_size), human_bytes(*output_size)
        ),
        JobOutcome::Skipped { reason, .. } => {
            tracing::info!(job = %spec.job_id, %file, "skipped: {reason}");
        }
        JobOutcome::Failed { error, .. } => {
            tracing::warn!(job = %spec.job_id, %file, "failed: {error}");
        }
        JobOutcome::Cancelled => tracing::info!(job = %spec.job_id, %file, "cancelled"),
    }
}

/// Everything decided before the first attempt.
#[derive(Debug)]
struct Prepared {
    original_size: u64,
    temp: PathBuf,
    final_path: PathBuf,
}

/// A failed attempt, kept so the final error can describe it.
#[derive(Debug, Clone)]
struct Failure {
    error: String,
    log_tail: Option<String>,
    command: Option<String>,
    encoder: Option<String>,
    attempt: u32,
    validation: Option<ValidationReport>,
}

impl Failure {
    fn into_outcome(self) -> JobOutcome {
        JobOutcome::Failed {
            error: self.error,
            log_tail: self.log_tail.filter(|t| !t.trim().is_empty()),
            command: self.command,
            encoder: self.encoder,
            attempt: self.attempt,
            validation: self.validation,
        }
    }
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

struct Job<'a> {
    cfg: &'a RunConfig,
    spec: &'a JobSpec,
    planner: &'a PlanFn,
    decider: &'a DecideFn,
    reporter: &'a Reporter,
    cancel: &'a CancellationToken,
}

impl Job<'_> {
    async fn run(&self) -> JobOutcome {
        if self.cancel.is_cancelled() {
            return JobOutcome::Cancelled;
        }
        self.reporter.stage(JobStage::Preparing, 0.0).await;
        let prepared = match self.prepare().await {
            Ok(p) => p,
            Err(outcome) => return outcome,
        };
        self.reporter.stage(JobStage::Preparing, 100.0).await;

        let guard = TempGuard::new(prepared.temp.clone());
        let outcome = self.attempts(&prepared, &guard).await;
        match &outcome {
            JobOutcome::Done { .. } => guard.disarm(),
            _ => guard.discard().await,
        }
        outcome
    }

    async fn prepare(&self) -> Result<Prepared, JobOutcome> {
        let (cfg, spec) = (self.cfg, self.spec);
        let input_meta = match tokio::fs::metadata(&spec.input).await {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Err(failed("The file no longer exists")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(failed("The file no longer exists"));
            }
            Err(e) => return Err(failed(format!("Could not read the file: {e}"))),
        };

        if let Decision::Skip { reason } = (self.decider)(&spec.probe, &spec.profile) {
            return Err(JobOutcome::Skipped {
                reason,
                encoder: None,
                output_size: None,
            });
        }
        if spec.candidates.is_empty() {
            return Err(failed(format!(
                "No working encoder was found for {}. Check the hardware settings",
                spec.profile.video_codec.label()
            )));
        }
        if cfg.output_mode == OutputMode::Folder && cfg.output_folder.is_none() {
            return Err(failed(
                "Output to a separate folder is on, but no output folder is set",
            ));
        }

        let container = spec.profile.container;
        let temp = temp_output_path(&spec.input, container, spec.job_id, cfg.temp_dir.as_deref());
        let final_path = final_output_path(
            &spec.input,
            container,
            cfg.output_mode,
            cfg.output_folder.as_deref(),
            &spec.library_root,
        );
        if let Some(conflict) =
            destination_conflict(&spec.input, &final_path, cfg.output_mode).await
        {
            return Err(failed(conflict));
        }

        let temp_dir = temp
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        if let Err(e) = tokio::fs::create_dir_all(&temp_dir).await {
            return Err(failed(format!(
                "Could not use the temp folder {}: {e}",
                temp_dir.display()
            )));
        }
        let size = input_meta.len();
        let needed = size.saturating_add(size / 10);
        if let Some(free) = free_space(&temp_dir).await {
            if free < needed {
                return Err(failed(format!(
                    "Not enough free space in {} for this file (needs about {})",
                    temp_dir.display(),
                    human_bytes(needed)
                )));
            }
        }

        Ok(Prepared {
            original_size: size,
            temp,
            final_path,
        })
    }

    /// The attempt chain. Returns the job's outcome; the caller cleans up
    /// the temp file unless the outcome is `Done`.
    async fn attempts(&self, prepared: &Prepared, guard: &TempGuard) -> JobOutcome {
        let (cfg, spec) = (self.cfg, self.spec);
        let chain = attempt_chain(&spec.candidates);
        let mut last_failure: Option<Failure> = None;
        let mut tried: Vec<Vec<String>> = Vec::new();
        let mut attempt: u32 = 0;
        let mut first: Option<&EncoderCandidate> = None;

        for (i, candidate) in chain.iter().enumerate() {
            let is_last = i + 1 == chain.len();
            if self.cancel.is_cancelled() {
                return JobOutcome::Cancelled;
            }
            let plan = match (self.planner)(&PlanRequest {
                input: &spec.input,
                output: &prepared.temp,
                probe: &spec.probe,
                profile: &spec.profile,
                encoder: candidate,
            }) {
                Ok(plan) => plan,
                Err(e) => {
                    tracing::warn!(encoder = %candidate.name, "could not plan the encode: {e:#}");
                    last_failure = Some(Failure {
                        error: format!("Could not prepare the {} command: {e:#}", candidate.name),
                        log_tail: None,
                        command: None,
                        encoder: Some(candidate.name.clone()),
                        attempt: attempt.max(1),
                        validation: None,
                    });
                    continue;
                }
            };
            let args = normalize_args(plan.args.clone());
            if tried.contains(&args) {
                // Same command as an attempt that already failed (e.g. a
                // deinterlaced source always decodes on the CPU).
                continue;
            }
            tried.push(args.clone());
            attempt += 1;
            first.get_or_insert(candidate);

            self.reporter
                .set_attempt(&candidate.name, candidate.api, attempt);
            self.reporter.stage(JobStage::Transcoding, 0.0).await;
            guard.clear().await;
            let command = display_command(&cfg.ffmpeg, &args);
            tracing::debug!(job = %spec.job_id, attempt, "running {command}");

            let exit = self.encode(&args).await;
            let failure = |error: String, log_tail: Option<String>| Failure {
                error,
                log_tail,
                command: Some(command.clone()),
                encoder: Some(candidate.name.clone()),
                attempt,
                validation: None,
            };
            match &exit {
                FfmpegExit::Success { .. } => {}
                FfmpegExit::Cancelled => return JobOutcome::Cancelled,
                other => {
                    let error = other
                        .describe_failure(&candidate.name)
                        .unwrap_or_else(|| format!("{} failed", candidate.name));
                    tracing::warn!(job = %spec.job_id, attempt, "{error}");
                    last_failure = Some(failure(error, other.tail().map(str::to_string)));
                    guard.clear().await;
                    continue;
                }
            }

            let output_size = match tokio::fs::metadata(&prepared.temp).await {
                Ok(m) if m.len() > 0 => m.len(),
                _ => {
                    last_failure = Some(failure(
                        format!("{} finished but wrote no output", candidate.name),
                        exit.tail().map(str::to_string),
                    ));
                    guard.clear().await;
                    continue;
                }
            };
            self.reporter.stage(JobStage::Transcoding, 100.0).await;

            if let Some(reason) = size_rule(
                spec.profile.min_savings_pct,
                prepared.original_size,
                output_size,
            ) {
                return JobOutcome::Skipped {
                    reason,
                    encoder: Some(candidate.name.clone()),
                    output_size: Some(output_size),
                };
            }

            let validation = if cfg.validation == ValidationLevel::Off {
                None
            } else {
                self.reporter.stage(JobStage::Verifying, 0.0).await;
                let report = validate_output_at(
                    &ValidateRequest {
                        ffmpeg: &cfg.ffmpeg,
                        ffprobe: &cfg.ffprobe,
                        source: &spec.input,
                        source_probe: &spec.probe,
                        output: &prepared.temp,
                        profile: &spec.profile,
                        level: cfg.validation,
                        expected: plan.expected,
                    },
                    cfg.low_priority,
                    self.cancel,
                    &|pct| self.reporter.tick(pct, None, None, None),
                )
                .await;
                if self.cancel.is_cancelled() {
                    return JobOutcome::Cancelled;
                }
                self.reporter.stage(JobStage::Verifying, 100.0).await;
                Some(report)
            };

            if let Some(report) = validation.as_ref().filter(|r| !r.passed) {
                let mut f = failure(verification_error(report), None);
                f.validation = Some(report.clone());
                if candidate.api.is_hardware() && !is_last {
                    tracing::warn!(
                        job = %spec.job_id, attempt,
                        "{} output failed verification; trying the next option", candidate.name
                    );
                    last_failure = Some(f);
                    guard.clear().await;
                    continue;
                }
                return f.into_outcome();
            }

            if self.cancel.is_cancelled() {
                return JobOutcome::Cancelled;
            }
            self.reporter.stage(JobStage::Finalizing, 0.0).await;
            let placed = finalize(&FinalizeRequest {
                input: &spec.input,
                temp: &prepared.temp,
                final_path: &prepared.final_path,
                mode: cfg.output_mode,
                job_id: spec.job_id,
                keep_dates: cfg.keep_file_dates,
                force_copy: false,
            })
            .await;
            let output_size = match placed {
                Ok(size) => size,
                Err(e) => {
                    let mut f = failure(format!("{e:#}"), None);
                    f.validation = validation;
                    return f.into_outcome();
                }
            };
            self.reporter.stage(JobStage::Finalizing, 100.0).await;

            let mut notes = plan.notes;
            if let Some(first) = first.filter(|_| attempt > 1) {
                notes.push(fallback_note(first, candidate));
            }
            return JobOutcome::Done {
                output_path: prepared.final_path.clone(),
                output_size,
                original_size: prepared.original_size,
                encoder: candidate.name.clone(),
                hw_api: candidate.api,
                attempt,
                validation,
                command,
                notes,
            };
        }

        match last_failure {
            Some(mut f) => {
                if attempt > 1 {
                    f.error = format!("All {attempt} attempts failed. Last error: {}", f.error);
                }
                f.into_outcome()
            }
            None => failed("No encoder could be used for this file"),
        }
    }

    /// Run ffmpeg for one attempt, reporting transcoding progress.
    async fn encode(&self, args: &[String]) -> FfmpegExit {
        let duration = self.spec.probe.duration_secs;
        let started = Instant::now();
        let cmd = FfmpegCommand {
            program: &self.cfg.ffmpeg,
            args,
            low_priority: self.cfg.low_priority,
            stall_timeout: DEFAULT_STALL_TIMEOUT,
        };
        run_ffmpeg(
            &cmd,
            self.cancel,
            &mut |block| {
                let p = compute_progress(block, duration, started.elapsed().as_secs_f64());
                self.reporter.tick(p.percent, p.fps, p.speed, p.eta_secs);
            },
            &mut |_| {},
        )
        .await
    }
}

/// The encoders to try, in order: every candidate as given, and after a
/// hardware candidate that decodes on the GPU, the same encoder with CPU
/// decoding. Duplicates are dropped.
pub(crate) fn attempt_chain(candidates: &[EncoderCandidate]) -> Vec<EncoderCandidate> {
    let mut chain: Vec<EncoderCandidate> = Vec::with_capacity(candidates.len() * 2);
    let mut push = |c: EncoderCandidate| {
        if !chain.contains(&c) {
            chain.push(c);
        }
    };
    for c in candidates {
        push(c.clone());
        if c.api.is_hardware() && c.hw_decode {
            push(EncoderCandidate {
                hw_decode: false,
                ..c.clone()
            });
        }
    }
    chain
}

/// Make sure the command hides the banner, ignores stdin and reports
/// progress on stdout, whatever the planner produced.
pub(crate) fn normalize_args(mut args: Vec<String>) -> Vec<String> {
    let has = |args: &[String], flag: &str| args.iter().any(|a| a == flag);
    let mut prefix: Vec<String> = Vec::new();
    if !has(&args, "-hide_banner") {
        prefix.push("-hide_banner".into());
    }
    if !has(&args, "-nostdin") {
        prefix.push("-nostdin".into());
    }
    if !has(&args, "-progress") {
        prefix.extend(["-progress".into(), "pipe:1".into()]);
        if !has(&args, "-nostats") {
            prefix.push("-nostats".into());
        }
    }
    if prefix.is_empty() {
        return args;
    }
    prefix.append(&mut args);
    prefix
}

/// The size rule: `Some(reason)` when the output is not worth keeping.
pub(crate) fn size_rule(
    min_savings_pct: Option<u8>,
    original_size: u64,
    output_size: u64,
) -> Option<String> {
    let min = f64::from(min_savings_pct?);
    if original_size == 0 {
        return None;
    }
    let saved = (1.0 - output_size as f64 / original_size as f64) * 100.0;
    if saved >= min {
        return None;
    }
    Some(if saved >= 1.0 {
        format!("Only {}% smaller — kept the original", saved.floor() as u64)
    } else if saved > -1.0 {
        "About the same size — kept the original".to_string()
    } else {
        format!(
            "The new file was {}% larger — kept the original",
            (-saved).floor() as u64
        )
    })
}

/// "Verification failed: <label> — <detail>".
fn verification_error(report: &ValidationReport) -> String {
    match report.first_failure() {
        Some(check) => format!("Verification failed: {} — {}", check.label, check.detail),
        None => "Verification failed".to_string(),
    }
}

/// Plain-language note about a fallback, for the job's notes.
fn fallback_note(first: &EncoderCandidate, used: &EncoderCandidate) -> String {
    if first.name == used.name && first.hw_decode && !used.hw_decode {
        "Decoding on the GPU didn't work for this file, so it was decoded on the CPU".to_string()
    } else {
        format!(
            "{} ({}) didn't work for this file, so {} ({}) was used",
            first.name,
            first.api.label(),
            used.name,
            used.api.label()
        )
    }
}

/// Free bytes available to unprivileged users on the filesystem of `dir`.
/// `None` when unknown (the check is then skipped).
async fn free_space(dir: &Path) -> Option<u64> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || free_space_blocking(&dir))
        .await
        .ok()
        .flatten()
}

#[cfg(unix)]
fn free_space_blocking(dir: &Path) -> Option<u64> {
    let stat = rustix::fs::statvfs(dir).ok()?;
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

#[cfg(not(unix))]
fn free_space_blocking(_dir: &Path) -> Option<u64> {
    None
}

/// Sends [`JobProgress`] updates, throttled to [`PROGRESS_INTERVAL`].
/// Stage changes always go out.
struct Reporter {
    tx: mpsc::Sender<JobProgress>,
    job_id: Uuid,
    file_id: Uuid,
    state: Mutex<ReporterState>,
}

struct ReporterState {
    stage: JobStage,
    encoder: Option<String>,
    hw_api: Option<HwApi>,
    attempt: u32,
    last_sent: Option<Instant>,
}

impl Reporter {
    fn new(tx: mpsc::Sender<JobProgress>, job_id: Uuid, file_id: Uuid) -> Self {
        Self {
            tx,
            job_id,
            file_id,
            state: Mutex::new(ReporterState {
                stage: JobStage::Preparing,
                encoder: None,
                hw_api: None,
                attempt: 1,
                last_sent: None,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ReporterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_attempt(&self, encoder: &str, api: HwApi, attempt: u32) {
        let mut state = self.lock();
        state.encoder = Some(encoder.to_string());
        state.hw_api = Some(api);
        state.attempt = attempt.max(1);
    }

    fn message(
        &self,
        state: &ReporterState,
        progress: f32,
        fps: Option<f32>,
        speed: Option<f32>,
        eta_secs: Option<u64>,
    ) -> JobProgress {
        JobProgress {
            job_id: self.job_id,
            file_id: self.file_id,
            stage: state.stage,
            progress: progress.clamp(0.0, 100.0),
            fps,
            speed,
            eta_secs,
            encoder: state.encoder.clone(),
            hw_api: state.hw_api,
            attempt: state.attempt,
        }
    }

    /// Enter (or finish) a stage. Always sent, waiting briefly for room.
    async fn stage(&self, stage: JobStage, progress: f32) {
        let msg = {
            let mut state = self.lock();
            state.stage = stage;
            state.last_sent = Some(Instant::now());
            self.message(&state, progress, None, None, None)
        };
        match self.tx.try_send(msg) {
            Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => {}
            Err(mpsc::error::TrySendError::Full(msg)) => {
                let _ = tokio::time::timeout(STAGE_SEND_TIMEOUT, self.tx.send(msg)).await;
            }
        }
    }

    /// Progress within the current stage; dropped when too soon after the
    /// previous update or when the channel is full.
    fn tick(&self, progress: f32, fps: Option<f32>, speed: Option<f32>, eta_secs: Option<u64>) {
        let msg = {
            let mut state = self.lock();
            if state
                .last_sent
                .is_some_and(|last| last.elapsed() < PROGRESS_INTERVAL)
            {
                return;
            }
            state.last_sent = Some(Instant::now());
            self.message(&state, progress, fps, speed, eta_secs)
        };
        let _ = self.tx.try_send(msg);
    }
}

/// Deletes the temp file when a job ends without placing it.
struct TempGuard {
    path: PathBuf,
    armed: bool,
}

impl TempGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    /// Delete the temp file if it exists (between attempts).
    async fn clear(&self) {
        match tokio::fs::remove_file(&self.path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(path = %self.path.display(), "could not delete the temp file: {e}");
            }
        }
    }

    /// Delete the temp file and stand down.
    async fn discard(mut self) {
        self.clear().await;
        self.armed = false;
    }

    /// The file was moved into place; nothing to clean up.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Reached only when the job future is dropped mid-way. Delete off the
        // async threads when a runtime is available.
        let path = std::mem::take(&mut self.path);
        let remove = move || {
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(path = %path.display(), "could not delete the temp file: {e}");
                }
            }
        };
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(remove);
            }
            Err(_) => remove(),
        }
    }
}

#[cfg(test)]
mod tests {
    use chrysopoeia_core::{CheckStatus, ValidationCheck, VideoCodec};

    use super::*;

    fn candidate(name: &str, api: HwApi, hw_decode: bool) -> EncoderCandidate {
        EncoderCandidate {
            name: name.into(),
            codec: VideoCodec::Hevc,
            api,
            device: None,
            hw_decode,
        }
    }

    #[test]
    fn chain_retries_hardware_with_cpu_decoding() {
        let chain = attempt_chain(&[
            candidate("hevc_nvenc", HwApi::Nvenc, true),
            candidate("hevc_vaapi", HwApi::Vaapi, false),
            candidate("libx265", HwApi::Software, false),
            candidate("libx265", HwApi::Software, false),
        ]);
        let summary: Vec<_> = chain
            .iter()
            .map(|c| (c.name.as_str(), c.hw_decode))
            .collect();
        assert_eq!(
            summary,
            [
                ("hevc_nvenc", true),
                ("hevc_nvenc", false),
                ("hevc_vaapi", false),
                ("libx265", false),
            ]
        );
    }

    #[test]
    fn size_rule_messages() {
        assert_eq!(size_rule(None, 100, 200), None);
        assert_eq!(size_rule(Some(10), 100, 80), None);
        assert_eq!(size_rule(Some(10), 0, 80), None);
        assert_eq!(
            size_rule(Some(10), 1000, 953).as_deref(),
            Some("Only 4% smaller — kept the original")
        );
        assert_eq!(
            size_rule(Some(10), 1000, 1000).as_deref(),
            Some("About the same size — kept the original")
        );
        assert_eq!(
            size_rule(Some(10), 1000, 1125).as_deref(),
            Some("The new file was 12% larger — kept the original")
        );
    }

    #[test]
    fn args_are_normalized_once() {
        let args = vec!["-y".to_string(), "-i".into(), "in".into(), "out".into()];
        let normalized = normalize_args(args);
        assert_eq!(
            normalized[..5],
            [
                "-hide_banner",
                "-nostdin",
                "-progress",
                "pipe:1",
                "-nostats"
            ]
        );
        assert_eq!(normalize_args(normalized.clone()), normalized);
    }

    #[test]
    fn verification_error_names_the_first_failure() {
        let report = ValidationReport {
            passed: false,
            level: ValidationLevel::Standard,
            checks: vec![
                ValidationCheck {
                    id: "probe".into(),
                    label: "Opens correctly".into(),
                    status: CheckStatus::Pass,
                    detail: "ok".into(),
                    value: None,
                },
                ValidationCheck {
                    id: "decode".into(),
                    label: "Plays start to finish".into(),
                    status: CheckStatus::Fail,
                    detail: "Found a playback error".into(),
                    value: None,
                },
            ],
            ssim_min: None,
            ssim_avg: None,
            psnr_avg: None,
            elapsed_secs: 1.0,
        };
        assert_eq!(
            verification_error(&report),
            "Verification failed: Plays start to finish — Found a playback error"
        );
    }

    #[test]
    fn fallback_notes() {
        let gpu = candidate("hevc_nvenc", HwApi::Nvenc, true);
        let cpu_decode = candidate("hevc_nvenc", HwApi::Nvenc, false);
        let software = candidate("libx265", HwApi::Software, false);
        assert!(fallback_note(&gpu, &cpu_decode).contains("decoded on the CPU"));
        assert_eq!(
            fallback_note(&gpu, &software),
            "hevc_nvenc (NVIDIA NVENC) didn't work for this file, so libx265 (CPU) was used"
        );
    }

    #[tokio::test]
    async fn reporter_throttles_ticks_but_not_stages() {
        let (tx, mut rx) = mpsc::channel(100);
        let reporter = Reporter::new(tx, Uuid::nil(), Uuid::nil());
        reporter.stage(JobStage::Transcoding, 0.0).await;
        for i in 0..50 {
            reporter.tick(i as f32, None, None, None);
        }
        reporter.stage(JobStage::Transcoding, 100.0).await;
        drop(reporter);
        let mut got = Vec::new();
        while let Some(p) = rx.recv().await {
            got.push(p.progress);
        }
        assert_eq!(got, [0.0, 100.0]);
    }

    #[tokio::test]
    async fn temp_guard_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".x.chrysopoeia-00000000.tmp.mkv");
        tokio::fs::write(&path, b"data").await.unwrap();
        TempGuard::new(path.clone()).discard().await;
        assert!(!path.exists());

        tokio::fs::write(&path, b"data").await.unwrap();
        TempGuard::new(path.clone()).disarm();
        assert!(path.exists());

        // Dropped while armed: removed in the background.
        drop(TempGuard::new(path.clone()));
        for _ in 0..100 {
            if !path.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!path.exists());
    }

    /// The server runs jobs with `tokio::spawn`, which needs `Send` futures.
    #[test]
    fn job_futures_are_send() {
        fn assert_send<T: Send>(_: &T) {}
        let cfg = RunConfig {
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
            temp_dir: None,
            validation: ValidationLevel::Standard,
            output_mode: OutputMode::Replace,
            output_folder: None,
            keep_file_dates: true,
            low_priority: false,
        };
        let spec = JobSpec {
            job_id: Uuid::nil(),
            file_id: Uuid::nil(),
            input: PathBuf::from("/nonexistent.mkv"),
            library_root: PathBuf::from("/"),
            probe: ProbeInfo::default(),
            profile: TranscodeProfile::default(),
            candidates: Vec::new(),
        };
        let (tx, _rx) = mpsc::channel(1);
        let job = run_job(&cfg, &spec, tx, CancellationToken::new());
        assert_send(&job);
        drop(job);
    }
}
