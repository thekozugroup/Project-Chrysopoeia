//! Job orchestration: plan, encode with fallbacks, verify, finalize.
//!
//! [`run_job`] walks one file through the stages:
//!
//! 1. **Preparing** — the input still exists, `decide` agrees there is work,
//!    the destination is free, and the temp folder (and, when the new file
//!    must be copied to another filesystem, the destination) has room once
//!    the space promised to other running jobs is counted. A job whose room
//!    is held by other jobs waits for them.
//! 2. **Transcoding** — the attempt chain: each candidate encoder in order;
//!    a hardware encoder that decodes on the GPU is retried with CPU
//!    decoding before moving on. The first success wins.
//! 3. The size rule (`min_savings_pct`) may discard the result. If the
//!    original was replaced or modified while it was being encoded (e.g. a
//!    Sonarr/Radarr upgrade), the result is discarded too.
//! 4. **Verifying** — [`crate::validate_output`]. A hardware result that
//!    fails verification moves on to the next attempt (GPU encoders can
//!    produce corrupt output); any other failure fails the job.
//! 5. **Finalizing** — [`finalize`] puts the file in place.
//!
//! The original is only touched by a successful finalize. The temp file is
//! removed on every other path, including cancellation and the job future
//! being dropped (a drop guard).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use chrysopoeia_core::plain::io_reason;
use chrysopoeia_core::{
    EncoderCandidate, HwApi, JobProgress, JobStage, OutputMode, ProbeInfo, ProblemKind,
    TranscodeProfile, ValidationLevel, ValidationReport,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::ffmpeg::{
    DEFAULT_STALL_TIMEOUT, FfmpegCommand, FfmpegExit, compute_progress, display_command,
    is_input_damage, lower_first, run_ffmpeg,
};
use crate::finalize::{
    FileIdentity, FinalizeRequest, OriginalChanged, PlaceError, destination_conflict,
    destination_name, final_output_path, finalize, temp_output_path,
};
use crate::plan::{Decision, FfmpegPlan, PlanRequest, where_encoded};
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
    /// "Convert anyway": convert even when the file is already efficient or
    /// already in the target format ([`crate::plan::decide_forced`]), and
    /// keep the result whatever its size (`min_savings_pct` is not applied).
    /// Verification still applies.
    pub force: bool,
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
        /// Plain-language reason: what happened and what to do.
        error: String,
        /// What kind of problem it is, so the UI can offer the right fix.
        problem: ProblemKind,
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

/// How often a job waiting for disk space held by other jobs looks again.
const SPACE_RETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Verification progress needed before an ETA is estimated from its pace.
const VERIFY_ETA_MIN_PERCENT: f32 = 2.0;

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

/// The worker's own record of how a job ended, at debug level: the server
/// writes the one plain line per finished job that the log shows by default
/// (its activity entry), so an info line here would only repeat it.
fn log_outcome(spec: &JobSpec, outcome: &JobOutcome) {
    let file = spec.input.display();
    match outcome {
        JobOutcome::Done {
            encoder,
            attempt,
            original_size,
            output_size,
            ..
        } => tracing::debug!(
            job = %spec.job_id, %file, %encoder, attempt,
            "done: {} → {}", human_bytes(*original_size), human_bytes(*output_size)
        ),
        JobOutcome::Skipped { reason, .. } => {
            tracing::debug!(job = %spec.job_id, %file, "skipped: {reason}");
        }
        JobOutcome::Failed { error, .. } => {
            tracing::debug!(job = %spec.job_id, %file, "failed: {error}");
        }
        JobOutcome::Cancelled => tracing::debug!(job = %spec.job_id, %file, "cancelled"),
    }
}

/// Everything decided before the first attempt.
#[derive(Debug)]
struct Prepared {
    original_size: u64,
    /// The original as it was when the job started.
    identity: FileIdentity,
    temp: PathBuf,
    final_path: PathBuf,
    /// Folders created for the temp file, outermost first; removed again
    /// (when empty) if the job does not finish.
    created_dirs: Vec<PathBuf>,
    /// Disk space promised to this job; released when the job ends.
    _space: SpaceGuard,
}

/// The reason given when the original changed during the job.
fn original_changed() -> String {
    OriginalChanged.to_string()
}

/// A failed attempt, kept so the final error can describe it.
#[derive(Debug, Clone)]
struct Failure {
    error: String,
    problem: ProblemKind,
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
            problem: self.problem,
            log_tail: self.log_tail.filter(|t| !t.trim().is_empty()),
            command: self.command,
            encoder: self.encoder,
            attempt: self.attempt,
            validation: self.validation,
        }
    }
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

struct Job<'a> {
    cfg: &'a RunConfig,
    spec: &'a JobSpec,
    planner: &'a PlanFn,
    decider: &'a DecideFn,
    reporter: &'a Reporter,
    cancel: &'a CancellationToken,
}

impl Job<'_> {
    /// The size rule for this job: the profile's, except when converting
    /// anyway (the user wants the new file whatever its size).
    fn min_savings_pct(&self) -> Option<u8> {
        if self.spec.force {
            None
        } else {
            self.spec.profile.min_savings_pct
        }
    }

    /// Where the encode is written while it is being made.
    fn temp_place(&self) -> Place {
        if self.cfg.temp_dir.is_some() {
            Place::WorkFolder
        } else {
            Place::Destination
        }
    }

    async fn run(&self) -> JobOutcome {
        if self.cancel.is_cancelled() {
            return JobOutcome::Cancelled;
        }
        self.reporter.stage(JobStage::Preparing, 0.0).await;
        // A folder on a share that stopped answering blocks the checks
        // below; Cancel and Stop must still end the job at once.
        let prepared = tokio::select! {
            prepared = self.prepare() => match prepared {
                Ok(p) => p,
                Err(outcome) => return outcome,
            },
            () = self.cancel.cancelled() => return JobOutcome::Cancelled,
        };
        self.reporter.stage(JobStage::Preparing, 100.0).await;

        let guard = TempGuard::new(prepared.temp.clone());
        let outcome = self.attempts(&prepared, &guard).await;
        match &outcome {
            JobOutcome::Done { .. } => guard.disarm(),
            _ => {
                guard.discard().await;
                remove_empty_dirs(&prepared.created_dirs).await;
            }
        }
        outcome
    }

    async fn prepare(&self) -> Result<Prepared, JobOutcome> {
        let (cfg, spec) = (self.cfg, self.spec);
        let looked = tokio::time::timeout(INPUT_CHECK_TIMEOUT, tokio::fs::metadata(&spec.input));
        let input_meta = match looked.await {
            Err(_) => return Err(failed(ProblemKind::UnreadableSource, not_answering())),
            Ok(Ok(m)) if m.is_file() => m,
            Ok(Ok(_)) => return Err(failed(ProblemKind::SourceChanged, SOURCE_GONE)),
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(failed(ProblemKind::SourceChanged, SOURCE_GONE));
            }
            Ok(Err(e)) => {
                return Err(failed(
                    ProblemKind::UnreadableSource,
                    unreadable_original(&e),
                ));
            }
        };

        let decision = match (self.decider)(&spec.probe, &spec.profile) {
            // Converting anyway sets aside the rules about files that are
            // already efficient, not those that protect the file.
            Decision::Skip { .. } if spec.force => {
                crate::plan::decide_forced(&spec.probe, &spec.profile)
            }
            decision => decision,
        };
        if let Decision::Skip { reason } = decision {
            return Err(JobOutcome::Skipped {
                reason,
                encoder: None,
                output_size: None,
            });
        }
        // Replacing the original with a file that can't hold all of its
        // subtitles or attachments would lose them for good; converting
        // anyway (or into a separate folder, which keeps the original) is
        // the user's call.
        if cfg.output_mode == OutputMode::Replace
            && !spec.force
            && let Some(reason) = crate::plan::replace_loss(&spec.probe, &spec.profile)
        {
            return Err(JobOutcome::Skipped {
                reason,
                encoder: None,
                output_size: None,
            });
        }
        if spec.candidates.is_empty() {
            return Err(failed(
                ProblemKind::HardwareUnavailable,
                format!(
                    "Nothing on this server can make {} video right now, so this file wasn't \
                     converted. Choose Automatic under Hardware in Settings, or allow converting \
                     on the CPU.",
                    spec.profile.video_codec.label()
                ),
            ));
        }
        if cfg.output_mode == OutputMode::Folder && cfg.output_folder.is_none() {
            return Err(failed(
                ProblemKind::Destination,
                "Saving converted files to a separate folder is turned on, but no folder is \
                 chosen. Choose one in Settings > Output.",
            ));
        }

        let container = spec.profile.container;
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
            return Err(failed(ProblemKind::Destination, conflict));
        }
        // The new file goes into this folder (and, when replacing, the
        // original is renamed there); find out now rather than after hours
        // of encoding.
        if let Some(dir) = final_path.parent()
            && let Some(problem) =
                folder_not_writable(dir, Place::Destination, cfg.output_mode).await
        {
            return Err(failed(ProblemKind::Destination, problem));
        }

        // Without a temp folder the encode is written next to where it ends
        // up: beside the original, or (folder mode) in the output folder, so
        // a read-only library works and no cross-disk copy is needed.
        let temp_home = match (&cfg.temp_dir, cfg.output_mode) {
            (Some(dir), _) => Some(dir.clone()),
            (None, OutputMode::Folder) => final_path.parent().map(Path::to_path_buf),
            (None, OutputMode::Replace) => None,
        };
        let temp = temp_output_path(&spec.input, container, spec.job_id, temp_home.as_deref());
        let temp_dir = temp
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let place = self.temp_place();
        let created_dirs = match create_dirs(&temp_dir).await {
            Ok(created) => created,
            Err(e) => {
                let (problem, error) = folder_unusable(&temp_dir, place, cfg.output_mode, &e);
                return Err(failed(problem, error));
            }
        };
        // A work folder of its own must take new files too (the folder the
        // new file goes to was checked above).
        if place == Place::WorkFolder
            && let Some(problem) = folder_not_writable(&temp_dir, place, cfg.output_mode).await
        {
            remove_empty_dirs(&created_dirs).await;
            return Err(failed(ProblemKind::WorkFolder, problem));
        }
        let size = input_meta.len();
        let space = match self.reserve_space(size, &temp, &final_path).await {
            Ok(space) => space,
            Err(outcome) => {
                remove_empty_dirs(&created_dirs).await;
                return Err(outcome);
            }
        };

        Ok(Prepared {
            original_size: size,
            identity: FileIdentity::of(&input_meta),
            temp,
            final_path,
            created_dirs,
            _space: space,
        })
    }

    /// Reserve room for the encode in the temp folder (about 1.1x the
    /// original) and, when the result will be copied to another filesystem,
    /// for the copy there. Waits while other running jobs hold the room.
    ///
    /// Only the temp folder can fail the job here: the new file's size is
    /// unknown until it is made, and converting to save space on a nearly
    /// full disk is exactly what the destination check must not refuse.
    /// There the reservation (at most the largest result the size rule
    /// keeps) only makes parallel jobs take turns, and `finalize` checks the
    /// real size before copying.
    async fn reserve_space(
        &self,
        input_size: u64,
        temp: &Path,
        final_path: &Path,
    ) -> Result<SpaceGuard, JobOutcome> {
        let needed = input_size.saturating_add(input_size / 10);
        let largest_kept = match self.min_savings_pct() {
            Some(p) => input_size / 100 * u64::from(100u8.saturating_sub(p)),
            None => input_size,
        };
        let temp_dir = temp.parent().unwrap_or(Path::new(".")).to_path_buf();
        let final_dir = final_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let needs = vec![
            SpaceNeed {
                dir: temp_dir,
                bytes: needed,
                file: Some(temp.to_path_buf()),
                strict: true,
            },
            SpaceNeed {
                dir: final_dir,
                bytes: largest_kept,
                file: None,
                strict: false,
            },
        ];
        let mut waiting = false;
        loop {
            let attempt = needs.clone();
            let result = tokio::task::spawn_blocking(move || try_reserve(&attempt))
                .await
                .unwrap_or(Ok(SpaceGuard::default()));
            match result {
                Ok(guard) => return Ok(guard),
                Err(Shortfall::Never { dir, bytes }) => {
                    return Err(failed(
                        ProblemKind::DiskFull,
                        no_room_message(&dir, bytes, self.temp_place(), self.cfg.output_mode),
                    ));
                }
                Err(Shortfall::Busy { dir }) => {
                    if !waiting {
                        waiting = true;
                        tracing::info!(
                            job = %self.spec.job_id,
                            "waiting for other jobs to free space in {}", dir.display()
                        );
                    }
                    tokio::select! {
                        () = self.cancel.cancelled() => return Err(JobOutcome::Cancelled),
                        () = tokio::time::sleep(SPACE_RETRY_INTERVAL) => {}
                    }
                }
            }
        }
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
                    let (problem, error) = plan_failure(&e);
                    last_failure = Some(Failure {
                        error,
                        problem,
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

            let run = self.encode(&args).await;
            let exit = run.exit;
            let failure = |problem: ProblemKind, error: String, log_tail: Option<String>| Failure {
                error,
                problem,
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
                    let (problem, error) = encode_failure(
                        other,
                        candidate.api,
                        &prepared.temp,
                        self.temp_place(),
                        cfg.output_mode,
                    );
                    let f = failure(problem, error, other.tail().map(str::to_string));
                    // A full disk or a folder that can't be written stops
                    // every way of converting alike, and so does a converter
                    // that can't be started: say so now instead of starting
                    // another (maybe hours long) encode that fails the same.
                    if affects_every_attempt(problem, other) {
                        return f.into_outcome();
                    }
                    // The job's notes (or its error) tell the user; the log
                    // keeps the attempt-by-attempt detail at debug level.
                    let next = if is_last {
                        ""
                    } else {
                        "; trying the next option"
                    };
                    tracing::debug!(job = %spec.job_id, attempt, "{}{next}", f.error);
                    last_failure = Some(f);
                    guard.clear().await;
                    continue;
                }
            }

            let output_size = match tokio::fs::metadata(&prepared.temp).await {
                Ok(m) if m.len() > 0 => m.len(),
                _ => {
                    last_failure = Some(failure(
                        ProblemKind::Encoder,
                        format!(
                            "Converting {} finished without writing a new file, so the original \
                             was left unchanged. Try again, or look at the job's log for details.",
                            where_encoded(candidate.api)
                        ),
                        exit.tail().map(str::to_string),
                    ));
                    guard.clear().await;
                    continue;
                }
            };
            self.reporter.stage(JobStage::Transcoding, 100.0).await;

            // An original that ends early (a cut-off download or copy) makes
            // a short but otherwise fine encode; say so plainly instead of
            // letting it fail verification (or, unverified, replace the
            // original). Checked before the size rule: a stub is always small.
            // Only a CPU decode tells about the file itself: a GPU decoder
            // that gives up early gets the next attempt (CPU decoding) first.
            let expected = self.spec.probe.duration_secs;
            let unverified = cfg.validation == ValidationLevel::Off;
            let conclusive = !(candidate.api.is_hardware() && candidate.hw_decode) || is_last;
            if let Some(stop) = source_stops_early(
                expected,
                run.encoded_secs,
                run.input_damage.is_some(),
                unverified,
            ) {
                let f = failure(
                    ProblemKind::UnreadableSource,
                    damaged_source_message(stop),
                    exit.tail().map(str::to_string),
                );
                if conclusive {
                    return f.into_outcome();
                }
                tracing::debug!(
                    job = %spec.job_id, attempt,
                    "{} stopped early while decoding on the GPU; trying the next option",
                    candidate.name
                );
                last_failure = Some(f);
                guard.clear().await;
                continue;
            }

            if let Some(reason) =
                size_rule(self.min_savings_pct(), prepared.original_size, output_size)
            {
                return JobOutcome::Skipped {
                    reason,
                    encoder: Some(candidate.name.clone()),
                    output_size: Some(output_size),
                };
            }

            // The encode read the file that was here when the job started;
            // if it has been replaced since, the result is of an old version.
            match FileIdentity::read(&spec.input).await {
                Ok(now) if now == prepared.identity => {}
                Ok(_) => {
                    return JobOutcome::Skipped {
                        reason: original_changed(),
                        encoder: Some(candidate.name.clone()),
                        output_size: Some(output_size),
                    };
                }
                Err(_) => {
                    return failed(
                        ProblemKind::SourceChanged,
                        "The file is no longer there. It was moved or deleted while it was being \
                         converted. If it was moved, scan the library again to find it.",
                    );
                }
            }

            let validation = if cfg.validation == ValidationLevel::Off {
                None
            } else {
                self.reporter.stage(JobStage::Verifying, 0.0).await;
                let verify_started = Instant::now();
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
                    &|pct| {
                        let eta = verify_eta(pct, verify_started.elapsed());
                        self.reporter.tick(pct, None, None, eta);
                    },
                )
                .await;
                if self.cancel.is_cancelled() {
                    return JobOutcome::Cancelled;
                }
                self.reporter.stage(JobStage::Verifying, 100.0).await;
                Some(report)
            };

            if let Some(report) = validation.as_ref().filter(|r| !r.passed) {
                // Too short, and the encoder saw the input end early: the
                // original is what's incomplete, not the new file.
                let too_short =
                    conclusive && report.first_failure().is_some_and(|c| c.id == "duration");
                if let Some(stop) = too_short
                    .then(|| source_stops_early(expected, run.encoded_secs, true, false))
                    .flatten()
                {
                    let mut f = failure(
                        ProblemKind::UnreadableSource,
                        damaged_source_message(stop),
                        None,
                    );
                    f.validation = Some(report.clone());
                    return f.into_outcome();
                }
                let mut f = failure(ProblemKind::Verification, verification_error(report), None);
                f.validation = Some(report.clone());
                if candidate.api.is_hardware() && !is_last {
                    tracing::debug!(
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
                original: Some(prepared.identity),
                force_copy: false,
            })
            .await;
            let placed = match placed {
                Ok(placed) => placed,
                Err(e) if e.is::<OriginalChanged>() => {
                    return JobOutcome::Skipped {
                        reason: original_changed(),
                        encoder: Some(candidate.name.clone()),
                        output_size: Some(output_size),
                    };
                }
                Err(e) => {
                    let (problem, error) = match e.downcast_ref::<PlaceError>() {
                        Some(placing) => (placing.problem, placing.message.clone()),
                        None => {
                            tracing::warn!(job = %spec.job_id, "could not put the new file in place: {e:#}");
                            (
                                ProblemKind::Other,
                                "The new file couldn't be put in place, so the original was kept. \
                                 The details are in the server log."
                                    .to_string(),
                            )
                        }
                    };
                    let mut f = failure(problem, error, None);
                    f.validation = validation;
                    return f.into_outcome();
                }
            };
            let output_size = placed.size;
            self.reporter.stage(JobStage::Finalizing, 100.0).await;

            let mut notes = plan.notes;
            notes.extend(placed.notes);
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
                    f.error = format!(
                        "None of the {attempt} ways Chrysopoeia tried could convert this file. {}",
                        f.error
                    );
                }
                f.into_outcome()
            }
            None => failed(
                ProblemKind::Encoder,
                "Chrysopoeia found no way to convert this file here, so it was left unchanged. \
                 The details are in the server log.",
            ),
        }
    }

    /// Run ffmpeg for one attempt, reporting transcoding progress.
    async fn encode(&self, args: &[String]) -> EncodeRun {
        let duration = self
            .spec
            .probe
            .duration_secs
            .filter(|d| d.is_finite() && *d > 0.0)
            .or_else(|| estimated_duration(&self.spec.probe));
        let started = Instant::now();
        let cmd = FfmpegCommand {
            program: &self.cfg.ffmpeg,
            args,
            low_priority: self.cfg.low_priority,
            stall_timeout: DEFAULT_STALL_TIMEOUT,
        };
        let mut encoded_secs: Option<f64> = None;
        let mut input_damage: Option<String> = None;
        let exit = run_ffmpeg(
            &cmd,
            self.cancel,
            &mut |block| {
                if let Some(t) = block.out_time_secs {
                    encoded_secs = Some(t);
                }
                let p = compute_progress(block, duration, started.elapsed().as_secs_f64());
                self.reporter.tick(p.percent, p.fps, p.speed, p.eta_secs);
            },
            &mut |line| {
                if input_damage.is_none() && is_input_damage(line) {
                    input_damage = Some(line.trim().to_string());
                }
            },
        )
        .await;
        EncodeRun {
            exit,
            encoded_secs,
            input_damage,
        }
    }
}

/// One encode attempt: how ffmpeg ended, and what it said about the input.
#[derive(Debug)]
struct EncodeRun {
    exit: FfmpegExit,
    /// How far into the source the encode got (the last progress report).
    encoded_secs: Option<f64>,
    /// The first line in which ffmpeg reported a damaged or cut-off input.
    input_damage: Option<String>,
}

/// Where the original stops, when an encode shows it ends clearly before
/// the length its container claims (a cut-off download or copy). Needs
/// evidence (`damage_seen`: ffmpeg reported the input damaged, or
/// verification found the result too short), except that a result under
/// half the claimed length is enough on its own when nothing verifies it.
pub(crate) fn source_stops_early(
    claimed_secs: Option<f64>,
    encoded_secs: Option<f64>,
    damage_seen: bool,
    unverified: bool,
) -> Option<f64> {
    let claimed = claimed_secs.filter(|d| d.is_finite() && *d >= 1.0)?;
    let encoded = encoded_secs
        .filter(|t| t.is_finite())
        .unwrap_or(0.0)
        .max(0.0);
    let clearly_short = claimed - encoded > (claimed * 0.05).max(2.0);
    let evidence = damage_seen || (unverified && encoded < claimed / 2.0);
    (clearly_short && evidence).then_some(encoded)
}

/// "The original file appears damaged or incomplete (it stops after 0.1 s).
/// It was left unchanged."
pub(crate) fn damaged_source_message(stop_secs: f64) -> String {
    format!(
        "The original file appears damaged or incomplete (it stops after {}). It was left \
         unchanged.",
        short_duration(stop_secs)
    )
}

/// A length for messages: `0.1 s`, `42.5 s`, `3:07`, `1:02:09`.
fn short_duration(secs: f64) -> String {
    let secs = secs.max(0.1);
    if secs < 60.0 {
        return format!("{secs:.1} s");
    }
    let total = secs.round() as u64;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
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

/// Make sure the command hides the banner, ignores stdin, reports progress
/// on stdout and tags every log line with its level (so a failure can be
/// told apart from warnings and statistics), whatever the planner produced.
pub(crate) fn normalize_args(mut args: Vec<String>) -> Vec<String> {
    let has = |args: &[String], flag: &str| args.iter().any(|a| a == flag);
    let mut prefix: Vec<String> = Vec::new();
    match args
        .iter()
        .position(|a| a == "-loglevel" || a == "-v")
        .map(|i| i + 1)
    {
        Some(i) if i < args.len() => {
            let level = &args[i];
            if !level.split('+').any(|flag| flag == "level") {
                args[i] = format!("level+{level}");
            }
        }
        _ => prefix.extend(["-loglevel".into(), "level+warning".into()]),
    }
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

/// The length of a file whose container reports none (some damaged or
/// raw streams), estimated from its size and bitrate, so encoding progress
/// can still be shown.
fn estimated_duration(probe: &ProbeInfo) -> Option<f64> {
    let bits_per_sec = probe.bit_rate.filter(|b| *b > 0).or_else(|| {
        let sum: u64 = probe.streams.iter().filter_map(|s| s.bit_rate).sum();
        (sum > 0).then_some(sum)
    })?;
    let secs = probe.size_bytes as f64 * 8.0 / bits_per_sec as f64;
    (secs.is_finite() && secs >= 1.0).then_some(secs)
}

/// Seconds left in verification, estimated from its pace so far.
fn verify_eta(percent: f32, elapsed: Duration) -> Option<u64> {
    if !(VERIFY_ETA_MIN_PERCENT..100.0).contains(&percent) || elapsed < Duration::from_secs(2) {
        return None;
    }
    let secs = elapsed.as_secs_f64() * f64::from(100.0 - percent) / f64::from(percent);
    (secs.is_finite() && secs >= 0.0).then(|| secs.round() as u64)
}

/// A folder a job writes in, for messages about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    /// The work folder chosen in Settings (or `TEMP_DIR`), where the new
    /// file is made.
    WorkFolder,
    /// The folder the new file ends up in: beside the original, or in the
    /// output folder. Without a work folder the new file is made there too.
    Destination,
}

/// Why new files can't be written in `dir` (or, when it doesn't exist yet,
/// in the closest folder above it that does), if they can't.
async fn folder_not_writable(dir: &Path, place: Place, mode: OutputMode) -> Option<String> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let existing = dir.ancestors().find(|d| d.is_dir())?;
        let blocked = write_access(existing).err()?;
        let shown = existing.display();
        Some(match (place, blocked) {
            (Place::WorkFolder, WriteBlock::ReadOnly) => format!(
                "The work folder {shown} is on a read-only drive, so nothing can be converted \
                 there. Choose another work folder in Settings > Output."
            ),
            (Place::WorkFolder, WriteBlock::Denied) => format!(
                "Chrysopoeia doesn't have permission to write in the work folder {shown}. Check \
                 its permissions (in Docker, the PUID/PGID user needs write access), or choose \
                 another work folder in Settings > Output."
            ),
            (Place::Destination, blocked) => destination_blocked(existing, mode, blocked),
        })
    })
    .await
    .ok()
    .flatten()
}

/// The message when the folder the new file goes to (`dir`) doesn't take
/// new files, e.g. "The output folder /out is on a read-only drive, …".
fn destination_blocked(dir: &Path, mode: OutputMode, blocked: WriteBlock) -> String {
    let folder = destination_name(dir, mode);
    let other_place = match mode {
        OutputMode::Folder => "choose another output folder in Settings > Output",
        OutputMode::Replace => "save converted files to a separate folder in Settings > Output",
    };
    match blocked {
        WriteBlock::ReadOnly => format!(
            "{} is on a read-only drive, so the converted file can't be put there. Make the \
             drive writable, or {other_place}.",
            capitalize_first(&folder)
        ),
        WriteBlock::Denied => format!(
            "Chrysopoeia doesn't have permission to write in {folder}, so the converted file \
             can't be put there. Check the folder's permissions (in Docker, the PUID/PGID user \
             needs write access), or {other_place}."
        ),
    }
}

/// Why a folder doesn't take new files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WriteBlock {
    /// It is on a read-only drive or mount.
    ReadOnly,
    /// The permissions don't allow it.
    Denied,
}

/// Whether this process may create files in `dir` (as the kernel sees it,
/// so root may write in folders without write permission, but not on a
/// read-only drive).
#[cfg(unix)]
fn write_access(dir: &Path) -> Result<(), WriteBlock> {
    match rustix::fs::access(
        dir,
        rustix::fs::Access::WRITE_OK | rustix::fs::Access::EXEC_OK,
    ) {
        Ok(()) => Ok(()),
        Err(rustix::io::Errno::ROFS) => Err(WriteBlock::ReadOnly),
        Err(_) => Err(WriteBlock::Denied),
    }
}

#[cfg(not(unix))]
fn write_access(dir: &Path) -> Result<(), WriteBlock> {
    match std::fs::metadata(dir) {
        Ok(m) if !m.permissions().readonly() => Ok(()),
        _ => Err(WriteBlock::Denied),
    }
}

/// The problem and message when the folder for the new file (`dir`, in
/// `place`) couldn't be created, e.g. "The work folder /temp can't be used
/// because a file with that name is in the way. Fix it, or choose another
/// work folder, in Settings > Output."
fn folder_unusable(
    dir: &Path,
    place: Place,
    mode: OutputMode,
    e: &std::io::Error,
) -> (ProblemKind, String) {
    use std::io::ErrorKind as K;
    let (what, fix, problem) = match (place, mode) {
        (Place::WorkFolder, _) => (
            format!("The work folder {}", dir.display()),
            "Fix it, or choose another work folder, in Settings > Output.",
            ProblemKind::WorkFolder,
        ),
        (Place::Destination, OutputMode::Folder) => (
            capitalize_first(&destination_name(dir, mode)),
            "Fix it, or choose another output folder, in Settings > Output.",
            ProblemKind::Destination,
        ),
        (Place::Destination, OutputMode::Replace) => (
            capitalize_first(&destination_name(dir, mode)),
            "Check that folder, then try again.",
            ProblemKind::Destination,
        ),
    };
    match e.kind() {
        K::AlreadyExists | K::NotADirectory => (
            problem,
            format!("{what} can't be used because a file with that name is in the way. {fix}"),
        ),
        K::PermissionDenied => (
            problem,
            format!(
                "{what} can't be created because Chrysopoeia doesn't have permission to write \
                 in the folder above it (in Docker, the PUID/PGID user needs write access). {fix}"
            ),
        ),
        K::ReadOnlyFilesystem => (
            problem,
            format!("{what} can't be created because its drive is read-only. {fix}"),
        ),
        K::StorageFull | K::QuotaExceeded => (
            ProblemKind::DiskFull,
            format!(
                "{what} can't be created because the disk is full. Free up some space there. \
                 {fix}"
            ),
        ),
        _ => (
            problem,
            format!("{what} can't be used because {}. {fix}", io_reason(e)),
        ),
    }
}

/// The message when a disk can never hold the new file: `dir` needs about
/// `bytes` and has less free space, even with no other job running.
fn no_room_message(dir: &Path, bytes: u64, place: Place, mode: OutputMode) -> String {
    let shown = dir.display();
    let size = human_bytes(bytes);
    match (place, mode) {
        (Place::WorkFolder, _) => format!(
            "There isn't enough free space in the work folder {shown} to convert this file (it \
             needs about {size}). Free up some space there, or choose a work folder on a bigger \
             disk in Settings > Output."
        ),
        (Place::Destination, OutputMode::Folder) => format!(
            "There isn't enough free space in the output folder {shown} for this file (it needs \
             about {size}). Free up some space there, or choose another output folder in \
             Settings > Output."
        ),
        (Place::Destination, OutputMode::Replace) => format!(
            "There isn't enough free space next to the original in {shown} to convert this file \
             (it needs about {size}). Free up some space on that disk, or choose a work folder \
             on another disk in Settings > Output."
        ),
    }
}

/// What to say when a job's original file is gone before it started.
const SOURCE_GONE: &str = "The file is no longer there. It may have been moved or deleted. If \
    it was moved, scan the library again to find it.";

/// How long the original may take to answer when the job starts.
const INPUT_CHECK_TIMEOUT: Duration = Duration::from_secs(60);

/// Why the job stopped when the original didn't answer at the start.
fn not_answering() -> String {
    format!(
        "This file didn't answer for {} seconds, so it wasn't converted. If it's on a network \
         share or an external drive, check the connection, then try again.",
        INPUT_CHECK_TIMEOUT.as_secs()
    )
}

/// Why the original couldn't be read when the job started.
fn unreadable_original(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        "Chrysopoeia doesn't have permission to read this file. Check its permissions (in \
         Docker, the PUID/PGID user needs read access), then try again."
            .to_string()
    } else {
        format!(
            "This file couldn't be read because {}. Check that its drive or share is connected, \
             then try again.",
            io_reason(e)
        )
    }
}

/// The problem and message for a conversion the planner couldn't prepare.
/// Its reasons are sentences for users already.
fn plan_failure(e: &anyhow::Error) -> (ProblemKind, String) {
    let problem = if e.downcast_ref::<crate::plan::SourceProblem>().is_some() {
        ProblemKind::UnreadableSource
    } else {
        ProblemKind::Other
    };
    (
        problem,
        chrysopoeia_core::plain::strip_os_error(&format!("{e:#}")),
    )
}

/// Whether a failed encode would fail the same way with any other encoder:
/// the disk or a folder is the problem, or ffmpeg itself couldn't start.
fn affects_every_attempt(problem: ProblemKind, exit: &FfmpegExit) -> bool {
    matches!(
        problem,
        ProblemKind::DiskFull | ProblemKind::WorkFolder | ProblemKind::Destination
    ) || matches!(exit, FfmpegExit::NotStarted { .. })
}

/// Longest part of ffmpeg's own words quoted in an error.
const MAX_QUOTED_CHARS: usize = 160;

/// The problem and message for an encode that ffmpeg ended with an error.
/// Well-known causes (a full disk, a read-only folder, a missing GPU
/// driver) are said plainly; anything else quotes ffmpeg's most useful line
/// after a plain sentence.
fn encode_failure(
    exit: &FfmpegExit,
    api: HwApi,
    temp: &Path,
    place: Place,
    mode: OutputMode,
) -> (ProblemKind, String) {
    let converting = where_encoded(api);
    let folder = temp.parent().unwrap_or(Path::new("."));
    let shown = folder.display();
    let (code, tail) = match exit {
        FfmpegExit::Failed { code, tail } => (*code, tail.as_str()),
        FfmpegExit::Stalled { after, .. } => {
            return (
                ProblemKind::Encoder,
                format!(
                    "Converting {converting} stopped making progress for {} minutes, so it was \
                     stopped. The original was left unchanged. Try again; if it happens again, \
                     the job's log has the details.",
                    after.as_secs().div_ceil(60)
                ),
            );
        }
        FfmpegExit::NotStarted { error } => {
            return (
                ProblemKind::Other,
                format!("The converter couldn't be started, so nothing was converted. {error}"),
            );
        }
        FfmpegExit::Success { .. } | FfmpegExit::Cancelled => {
            return (
                ProblemKind::Other,
                "The conversion ended unexpectedly, so the original was left unchanged."
                    .to_string(),
            );
        }
    };
    let reason = crate::ffmpeg::failure_reason(tail.lines());
    let lower = reason.as_deref().unwrap_or("").to_ascii_lowercase();
    let full = || {
        let fix = match (place, mode) {
            (Place::WorkFolder, _) => {
                "Free up some space there, or choose a work folder on a bigger disk in \
                 Settings > Output."
            }
            (Place::Destination, OutputMode::Folder) => {
                "Free up some space there, or choose another output folder in Settings > Output."
            }
            (Place::Destination, OutputMode::Replace) => {
                "Free up some space on that disk, or choose a work folder on another disk in \
                 Settings > Output."
            }
        };
        let what = match place {
            Place::WorkFolder => format!("the work folder {shown}"),
            Place::Destination => destination_name(folder, mode),
        };
        format!(
            "The disk ran out of space while the new file was being written in {what}, so the \
             original was left unchanged. {fix}"
        )
    };
    if lower.contains("no space left on device") || lower.contains("disk quota exceeded") {
        return (ProblemKind::DiskFull, full());
    }
    let writing_fails = lower.contains("read-only file system")
        || (lower.contains("permission denied")
            && (lower.contains("output")
                || lower.contains(&temp.to_string_lossy().to_ascii_lowercase())));
    if writing_fails {
        let problem = match place {
            Place::WorkFolder => ProblemKind::WorkFolder,
            Place::Destination => ProblemKind::Destination,
        };
        let (what, other_place) = match (place, mode) {
            (Place::WorkFolder, _) => (
                format!("the work folder {shown}"),
                ", or choose another work folder in Settings > Output",
            ),
            (Place::Destination, OutputMode::Folder) => (
                destination_name(folder, mode),
                ", or choose another output folder in Settings > Output",
            ),
            (Place::Destination, OutputMode::Replace) => (destination_name(folder, mode), ""),
        };
        return (
            problem,
            format!(
                "The new file couldn't be written in {what}: the drive is read-only or \
                 Chrysopoeia doesn't have permission there. The original was left unchanged. \
                 Check the folder's permissions (in Docker, the PUID/PGID user needs write \
                 access){other_place}."
            ),
        );
    }
    if let Some(hint) = reason.as_deref().and_then(crate::ffmpeg::explain_failure) {
        return (
            ProblemKind::Encoder,
            format!(
                "Converting {converting} didn't work: {}. The original was left unchanged.",
                lower_first(hint.trim_end_matches('.'))
            ),
        );
    }
    let said = reason
        .map(|r| chrysopoeia_core::plain::strip_os_error(&r))
        .filter(|r| !r.is_empty())
        .map(|r| {
            let short: String = r.chars().take(MAX_QUOTED_CHARS).collect();
            let cut = if short.len() < r.len() { "…" } else { "" };
            format!(" ffmpeg said: \"{}{cut}\".", short.trim_end_matches('.'))
        });
    let stopped = if code.is_none() {
        "was stopped by the system (it may have run out of memory)"
    } else {
        "stopped with an error"
    };
    (
        ProblemKind::Encoder,
        match said {
            Some(said) => format!(
                "Converting {converting} {stopped}, so the original was left unchanged.{said}"
            ),
            None => format!(
                "Converting {converting} {stopped}, so the original was left unchanged. The \
                 job's log has the details."
            ),
        },
    )
}

/// Create `dir` and any missing parents. Returns the folders that were
/// created, outermost first.
async fn create_dirs(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut missing = Vec::new();
    let mut current = Some(dir);
    while let Some(d) = current {
        if d.as_os_str().is_empty() || tokio::fs::try_exists(d).await.unwrap_or(true) {
            break;
        }
        missing.push(d.to_path_buf());
        current = d.parent();
    }
    tokio::fs::create_dir_all(dir).await?;
    missing.reverse();
    Ok(missing)
}

/// Remove folders created by [`create_dirs`] again, innermost first, as
/// long as they are empty.
async fn remove_empty_dirs(created: &[PathBuf]) {
    for dir in created.iter().rev() {
        if tokio::fs::remove_dir(dir).await.is_err() {
            break;
        }
    }
}

/// What to do after a failed verification; the job's error ends with it.
const VERIFICATION_FIX: &str =
    "The original was kept. Try again, or choose lighter checks in Settings > Output.";

/// Why a result failed verification, and what to do, for the job's error
/// and the activity feed. A check's label says what passing means ("Same
/// length as the original"), so it is never used here: the failure is
/// phrased per check, e.g. "The new file is shorter than the original (0.1 s
/// instead of 8.0 s). The original was kept. Try again, or …".
pub(crate) fn verification_error(report: &ValidationReport) -> String {
    let what = failed_check_sentence(report);
    format!("{}. {VERIFICATION_FIX}", what.trim_end_matches('.'))
}

/// A check's detail without a trailing similarity score ("… (similarity
/// 0.50)"), a number that means little to most people; the report keeps it.
fn without_score(detail: &str) -> &str {
    match detail.rfind(" (") {
        Some(i) if detail.ends_with(')') && detail[i..].contains("similarity") => {
            detail[..i].trim_end()
        }
        _ => detail,
    }
}

/// What went wrong in the first failing check of `report`.
fn failed_check_sentence(report: &ValidationReport) -> String {
    let Some(check) = report.first_failure() else {
        return "The new file didn't pass verification".to_string();
    };
    let detail = without_score(check.detail.trim().trim_end_matches('.'));
    // Checks whose detail names a measurement rather than the problem get a
    // plain statement of the problem first.
    let lead = match check.id.as_str() {
        "streams" => Some("The new file doesn't have the tracks it should"),
        "decode" => Some("The new file doesn't play start to finish"),
        // A picture that doesn't match, or pictures that couldn't be
        // compared at all (no video track found, a frame that can't be
        // read): the latter is no mismatch.
        "visual" if detail.is_empty() || is_picture_mismatch(detail) => {
            Some("The new file doesn't look like the original")
        }
        "visual" => Some("The new file couldn't be compared with the original"),
        "black_frames" | "frozen_frames" if !detail.starts_with("The new file") => {
            Some("The new file has more black or frozen video than the original")
        }
        // probe, duration and the rest describe the problem themselves:
        // "The new file could not be opened: …", "The new file is shorter
        // than the original (0.1 s instead of 8.0 s)".
        "probe" | "duration" if detail.is_empty() => {
            Some("The new file isn't a complete copy of the original")
        }
        _ if detail.is_empty() => Some("The new file didn't pass verification"),
        _ => None,
    };
    match (lead, detail.is_empty()) {
        (Some(lead), true) => lead.to_string(),
        (Some(lead), false) => format!("{lead}. {}", capitalize_first(detail)),
        (None, _) => capitalize_first(detail),
    }
}

/// Whether a failing visual check's detail says the pictures differ (rather
/// than that they couldn't be compared).
fn is_picture_mismatch(detail: &str) -> bool {
    [
        "looks different",
        "different from the original",
        "doesn't match the original",
        "is damaged",
    ]
    .iter()
    .any(|phrase| detail.contains(phrase))
}

/// `text` with its first letter in upper case.
pub(crate) fn capitalize_first(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Plain-language note about a fallback, for the job's notes.
fn fallback_note(first: &EncoderCandidate, used: &EncoderCandidate) -> String {
    if first.name == used.name && first.hw_decode && !used.hw_decode {
        "Decoding on the GPU didn't work for this file, so it was decoded on the CPU".to_string()
    } else if first.api == used.api {
        format!(
            "The first way of converting {} didn't work for this file, so another one was used",
            where_encoded(used.api)
        )
    } else {
        format!(
            "Converting {} didn't work for this file, so it was converted {}",
            where_encoded(first.api),
            where_encoded(used.api)
        )
    }
}

/// Disk space promised to running jobs, so parallel jobs on one filesystem
/// do not all count the same free space.
static RESERVATIONS: Mutex<Vec<Reservation>> = Mutex::new(Vec::new());

/// Source of reservation ids.
static NEXT_RESERVATION: AtomicU64 = AtomicU64::new(1);

/// Space promised to one job on one filesystem.
#[derive(Debug, Clone)]
struct Reservation {
    id: u64,
    device: u64,
    bytes: u64,
    /// The file the job is writing there; what it already holds is no
    /// longer free and so no longer outstanding.
    file: Option<PathBuf>,
}

impl Reservation {
    /// Bytes the job has written so far.
    fn written(&self) -> u64 {
        self.file
            .as_ref()
            .and_then(|f| std::fs::metadata(f).ok())
            .map_or(0, |m| m.len())
    }
}

/// Room one job needs in one folder.
#[derive(Debug, Clone)]
struct SpaceNeed {
    dir: PathBuf,
    bytes: u64,
    file: Option<PathBuf>,
    /// Fail when the room can never be found. A non-strict need only waits
    /// for other jobs, and is reserved as it is otherwise.
    strict: bool,
}

/// Why room could not be reserved.
#[derive(Debug)]
enum Shortfall {
    /// Not enough even if no other job were running.
    Never { dir: PathBuf, bytes: u64 },
    /// Other running jobs hold the room; try again later.
    Busy { dir: PathBuf },
}

/// Releases a job's reservations when dropped (the job ended).
#[derive(Debug, Default)]
struct SpaceGuard {
    ids: Vec<u64>,
}

impl Drop for SpaceGuard {
    fn drop(&mut self) {
        if self.ids.is_empty() {
            return;
        }
        let mut all = RESERVATIONS.lock().unwrap_or_else(PoisonError::into_inner);
        all.retain(|r| !self.ids.contains(&r.id));
    }
}

/// Reserve every need at once, or none. Needs on the same filesystem as an
/// earlier one are merged into it (a temp file beside its destination needs
/// the room only once). Blocking: stats files and filesystems.
fn try_reserve(needs: &[SpaceNeed]) -> Result<SpaceGuard, Shortfall> {
    try_reserve_with(needs, &crate::finalize::filesystem_of)
}

/// [`try_reserve`] with the filesystem lookup supplied (tests).
fn try_reserve_with(
    needs: &[SpaceNeed],
    filesystem_of: &dyn Fn(&Path) -> Option<(u64, u64)>,
) -> Result<SpaceGuard, Shortfall> {
    // (device, need) with duplicates on one filesystem dropped.
    let mut distinct: Vec<(u64, &SpaceNeed)> = Vec::with_capacity(needs.len());
    for need in needs {
        let Some((device, _)) = filesystem_of(&need.dir) else {
            continue;
        };
        if !distinct.iter().any(|(d, _)| *d == device) {
            distinct.push((device, need));
        }
    }
    let mut all = RESERVATIONS.lock().unwrap_or_else(PoisonError::into_inner);
    for (device, need) in &distinct {
        let Some((_, free)) = filesystem_of(&need.dir) else {
            continue;
        };
        let others: Vec<(u64, u64)> = all
            .iter()
            .filter(|r| r.device == *device)
            .map(|r| (r.bytes, r.written()))
            .collect();
        let held: u64 = others.iter().map(|(b, w)| b.saturating_sub(*w)).sum();
        let written: u64 = others.iter().map(|(_, w)| *w).sum();
        if free.saturating_sub(held) >= need.bytes {
            continue;
        }
        // Other jobs' promises (and the files they are writing) are released
        // when they end, so wait for them if that would be enough.
        if !others.is_empty() && (held > 0 || free.saturating_add(written) >= need.bytes) {
            return Err(Shortfall::Busy {
                dir: need.dir.clone(),
            });
        }
        if need.strict {
            return Err(Shortfall::Never {
                dir: need.dir.clone(),
                bytes: need.bytes,
            });
        }
    }
    let mut guard = SpaceGuard::default();
    for (device, need) in distinct {
        let id = NEXT_RESERVATION.fetch_add(1, Ordering::Relaxed);
        all.push(Reservation {
            id,
            device,
            bytes: need.bytes,
            file: need.file.clone(),
        });
        guard.ids.push(id);
    }
    Ok(guard)
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
            if let Err(e) = std::fs::remove_file(&path)
                && e.kind() != std::io::ErrorKind::NotFound
            {
                tracing::warn!(path = %path.display(), "could not delete the temp file: {e}");
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
            normalized[..7],
            [
                "-loglevel",
                "level+warning",
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
    fn log_levels_get_tagged() {
        let level_of = |args: &[&str]| {
            let args = normalize_args(args.iter().map(|a| a.to_string()).collect());
            let i = args
                .iter()
                .position(|a| a == "-loglevel" || a == "-v")
                .unwrap();
            args[i + 1].clone()
        };
        assert_eq!(
            level_of(&["-loglevel", "warning", "-i", "x"]),
            "level+warning"
        );
        assert_eq!(level_of(&["-v", "error", "-i", "x"]), "level+error");
        assert_eq!(
            level_of(&["-loglevel", "repeat+level+info"]),
            "repeat+level+info"
        );
        assert_eq!(level_of(&["-loglevel", "level+error"]), "level+error");
    }

    #[test]
    fn unknown_lengths_are_estimated_from_the_bitrate() {
        let mut probe = ProbeInfo {
            size_bytes: 10_000_000,
            ..Default::default()
        };
        assert_eq!(estimated_duration(&probe), None);
        probe.streams = vec![
            chrysopoeia_core::StreamInfo {
                bit_rate: Some(3_000_000),
                ..Default::default()
            },
            chrysopoeia_core::StreamInfo {
                bit_rate: Some(1_000_000),
                ..Default::default()
            },
        ];
        assert_eq!(estimated_duration(&probe), Some(20.0));
        probe.bit_rate = Some(8_000_000);
        assert_eq!(estimated_duration(&probe), Some(10.0));
    }

    #[test]
    fn cut_off_originals_are_recognised() {
        // The truncated test MKV: its header claims 6 s, ffmpeg reported the
        // file ends prematurely after the first frames.
        assert_eq!(
            source_stops_early(Some(6.0), Some(0.0), true, false),
            Some(0.0)
        );
        assert_eq!(
            damaged_source_message(0.0),
            "The original file appears damaged or incomplete (it stops after 0.1 s). It was \
             left unchanged."
        );
        // Complete encodes and small differences are fine.
        assert_eq!(source_stops_early(Some(6.0), Some(5.96), true, false), None);
        assert_eq!(
            source_stops_early(Some(7200.0), Some(7150.0), true, false),
            None
        );
        // Short without any sign of damage: verification decides.
        assert_eq!(
            source_stops_early(Some(600.0), Some(100.0), false, false),
            None
        );
        // ...unless nothing verifies the result.
        assert_eq!(
            source_stops_early(Some(600.0), Some(100.0), false, true),
            Some(100.0)
        );
        assert_eq!(
            source_stops_early(Some(600.0), Some(400.0), false, true),
            None
        );
        // Unknown or tiny claimed lengths prove nothing.
        assert_eq!(source_stops_early(None, Some(1.0), true, true), None);
        assert_eq!(source_stops_early(Some(0.5), Some(0.0), true, true), None);
        assert_eq!(short_duration(187.0), "3:07");
        assert_eq!(short_duration(3729.0), "1:02:09");
        assert_eq!(short_duration(42.46), "42.5 s");
    }

    #[test]
    fn verification_eta_follows_its_pace() {
        assert_eq!(verify_eta(1.0, Duration::from_secs(10)), None);
        assert_eq!(verify_eta(50.0, Duration::from_secs(1)), None);
        assert_eq!(verify_eta(25.0, Duration::from_secs(60)), Some(180));
        assert_eq!(verify_eta(100.0, Duration::from_secs(60)), None);
    }

    fn need(dir: &str, bytes: u64, file: Option<&Path>) -> SpaceNeed {
        SpaceNeed {
            dir: PathBuf::from(dir),
            bytes,
            file: file.map(Path::to_path_buf),
            strict: true,
        }
    }

    #[test]
    fn space_reservations_account_for_other_jobs() {
        // Two fake filesystems: /cache (device 901, 100 bytes free) and
        // /array (device 902, 1000 bytes free). Unique devices keep this
        // test independent of any other reservation in the process.
        let fs = |dir: &Path| -> Option<(u64, u64)> {
            if dir.starts_with("/cache") {
                Some((901, 100))
            } else if dir.starts_with("/array") {
                Some((902, 1000))
            } else {
                None
            }
        };
        let dir = tempfile::tempdir().unwrap();
        let partial = dir.path().join("partial.tmp");
        std::fs::write(&partial, [0u8; 30]).unwrap();

        // Temp and destination on different filesystems: both reserved.
        let first = try_reserve_with(
            &[
                need("/cache/t", 80, Some(&partial)),
                need("/array/m", 80, None),
            ],
            &fs,
        )
        .unwrap();
        assert_eq!(first.ids.len(), 2);

        // 100 free, 80 promised of which 30 written: only 50 left for others.
        match try_reserve_with(&[need("/cache/u", 60, None)], &fs) {
            Err(Shortfall::Busy { dir }) => assert_eq!(dir, Path::new("/cache/u")),
            other => panic!("expected Busy, got {other:?}"),
        }
        assert!(try_reserve_with(&[need("/cache/u", 50, None)], &fs).is_ok());

        // More than the disk can ever hold fails at once...
        match try_reserve_with(&[need("/cache/v", 5000, None)], &fs) {
            Err(Shortfall::Busy { .. }) => {}
            other => panic!("expected Busy while /cache is in use, got {other:?}"),
        }
        // ...unless it is only an estimate (the destination of a copy).
        let estimate = SpaceNeed {
            strict: false,
            ..need("/array/n", 5000, None)
        };
        match try_reserve_with(std::slice::from_ref(&estimate), &fs) {
            // /array holds the first job's promise: take turns.
            Err(Shortfall::Busy { .. }) => {}
            other => panic!("expected Busy, got {other:?}"),
        }

        // Once the first job ends, its room is free again.
        drop(first);
        assert!(try_reserve_with(&[need("/cache/u", 60, None)], &fs).is_ok());
        match try_reserve_with(&[need("/cache/v", 5000, None)], &fs) {
            Err(Shortfall::Never { bytes, .. }) => assert_eq!(bytes, 5000),
            other => panic!("expected Never, got {other:?}"),
        }
        assert!(try_reserve_with(&[estimate], &fs).is_ok());

        // Unknown filesystems are not checked; one filesystem is reserved once.
        let merged = try_reserve_with(
            &[
                need("/array/a", 10, None),
                need("/array/b", 10, None),
                need("/x", 1, None),
            ],
            &fs,
        )
        .unwrap();
        assert_eq!(merged.ids.len(), 1);
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
            "The new file doesn't play start to finish. Found a playback error. The original \
             was kept. Try again, or choose lighter checks in Settings > Output."
        );
    }

    /// The error never repeats a check's pass-form label, which reads as if
    /// the check passed ("Same length as the original — The new file is …").
    #[test]
    fn verification_errors_use_failure_phrasing_per_check() {
        let failing = |id: &str, label: &str, detail: &str| ValidationReport {
            passed: false,
            level: ValidationLevel::Standard,
            checks: vec![ValidationCheck {
                id: id.into(),
                label: label.into(),
                status: CheckStatus::Fail,
                detail: detail.into(),
                value: None,
            }],
            ssim_min: None,
            ssim_avg: None,
            psnr_avg: None,
            elapsed_secs: 1.0,
        };
        let cases = [
            (
                "duration",
                "Same length as the original",
                "The new file is shorter than the original (0.1 s instead of 8.0 s)",
                "The new file is shorter than the original (0.1 s instead of 8.0 s)",
            ),
            (
                "probe",
                "Opens correctly",
                "The new file could not be opened: Invalid data found",
                "The new file could not be opened: Invalid data found",
            ),
            (
                "streams",
                "All tracks present",
                "Expected 1 video and 2 audio tracks but found 1 video track",
                "The new file doesn't have the tracks it should. Expected 1 video and 2 audio \
                 tracks but found 1 video track",
            ),
            (
                "visual",
                "Looks like the original",
                "A frame near 0:10 looks very different from the original (similarity 0.40)",
                "The new file doesn't look like the original. A frame near 0:10 looks very \
                 different from the original",
            ),
            (
                "visual",
                "Looks like the original",
                "The picture near 1:05 doesn't match the original (similarity 0.70)",
                "The new file doesn't look like the original. The picture near 1:05 doesn't \
                 match the original",
            ),
            (
                "visual",
                "Looks like the original",
                "The picture from 10.0 s to 11.7 s is damaged (similarity 0.20)",
                "The new file doesn't look like the original. The picture from 10.0 s to 11.7 s \
                 is damaged",
            ),
            (
                "visual",
                "Looks like the original",
                "The picture from 1:00 to 1:02 suddenly looks different (similarity 0.35)",
                "The new file doesn't look like the original. The picture from 1:00 to 1:02 \
                 suddenly looks different",
            ),
            // Not a mismatch: the pictures couldn't be compared at all.
            (
                "visual",
                "Looks like the original",
                "The video track could not be found for comparison",
                "The new file couldn't be compared with the original. The video track could \
                 not be found for comparison",
            ),
            (
                "visual",
                "Looks like the original",
                "No picture could be read from the new file at 0:30",
                "The new file couldn't be compared with the original. No picture could be read \
                 from the new file at 0:30",
            ),
            (
                "black_frames",
                "No extra black frames",
                "The new file has 3.0 s more black video than the original",
                "The new file has 3.0 s more black video than the original",
            ),
            (
                "decode",
                "Plays start to finish",
                "",
                "The new file doesn't play start to finish",
            ),
        ];
        for (id, label, detail, expected) in cases {
            let error = verification_error(&failing(id, label, detail));
            assert_eq!(error, format!("{expected}. {VERIFICATION_FIX}"), "{id}");
            assert!(!error.contains(label), "{id}: {error}");
            assert!(!error.contains("similarity"), "{id}: {error}");
        }
    }

    #[test]
    fn fallback_notes() {
        let gpu = candidate("hevc_nvenc", HwApi::Nvenc, true);
        let cpu_decode = candidate("hevc_nvenc", HwApi::Nvenc, false);
        let software = candidate("libx265", HwApi::Software, false);
        assert!(fallback_note(&gpu, &cpu_decode).contains("decoded on the CPU"));
        assert_eq!(
            fallback_note(&gpu, &software),
            "Converting on the NVIDIA GPU didn't work for this file, so it was converted on the CPU"
        );
        let qsv = candidate("hevc_qsv", HwApi::Qsv, false);
        let other_qsv = candidate("hevc_qsv_alt", HwApi::Qsv, false);
        assert_eq!(
            fallback_note(&qsv, &other_qsv),
            "The first way of converting with Intel Quick Sync didn't work for this file, so \
             another one was used"
        );
    }

    fn failed_run(tail: &str) -> FfmpegExit {
        FfmpegExit::Failed {
            code: Some(1),
            tail: tail.to_string(),
        }
    }

    #[test]
    fn encode_failures_get_a_problem_kind_and_plain_words() {
        let temp = Path::new("/temp/.a.chrysopoeia-1.tmp.mkv");
        let disk_full = failed_run(
            "[out#0/matroska @ 0x1] [error] Error writing trailer: No space left on device",
        );
        let (problem, error) = encode_failure(
            &disk_full,
            HwApi::Software,
            temp,
            Place::WorkFolder,
            OutputMode::Replace,
        );
        assert_eq!(problem, ProblemKind::DiskFull);
        assert_eq!(
            error,
            "The disk ran out of space while the new file was being written in the work folder \
             /temp, so the original was left unchanged. Free up some space there, or choose a \
             work folder on a bigger disk in Settings > Output."
        );

        let read_only = failed_run(
            "[out#0/matroska @ 0x1] [error] Error opening output /m/.a.tmp.mkv: Read-only file \
             system",
        );
        let (problem, error) = encode_failure(
            &read_only,
            HwApi::Software,
            Path::new("/m/.a.tmp.mkv"),
            Place::Destination,
            OutputMode::Replace,
        );
        assert_eq!(problem, ProblemKind::Destination);
        assert!(
            error.starts_with("The new file couldn't be written in the original's folder /m:"),
            "{error}"
        );
        assert!(!error.contains("work folder"), "{error}");
        let (problem, error) = encode_failure(
            &read_only,
            HwApi::Software,
            Path::new("/out/.a.tmp.mkv"),
            Place::Destination,
            OutputMode::Folder,
        );
        assert_eq!(problem, ProblemKind::Destination);
        assert!(
            error.starts_with("The new file couldn't be written in the output folder /out:"),
            "{error}"
        );
        assert!(error.ends_with("or choose another output folder in Settings > Output."));
        let (_, error) = encode_failure(
            &disk_full,
            HwApi::Software,
            Path::new("/out/.a.tmp.mkv"),
            Place::Destination,
            OutputMode::Folder,
        );
        assert!(
            error.starts_with(
                "The disk ran out of space while the new file was being written in the output \
                 folder /out,"
            ),
            "{error}"
        );
        let (problem, _) = encode_failure(
            &read_only,
            HwApi::Software,
            temp,
            Place::WorkFolder,
            OutputMode::Replace,
        );
        assert_eq!(problem, ProblemKind::WorkFolder);

        let driver = failed_run("[h264_nvenc @ 0x1] [error] Cannot load libcuda.so.1");
        let (problem, error) = encode_failure(
            &driver,
            HwApi::Nvenc,
            temp,
            Place::WorkFolder,
            OutputMode::Replace,
        );
        assert_eq!(problem, ProblemKind::Encoder);
        assert_eq!(
            error,
            "Converting on the NVIDIA GPU didn't work: the NVIDIA driver could not be loaded. \
             Check that the GPU is passed through to the container. The original was left \
             unchanged."
        );

        let odd = failed_run("[libsvtav1 @ 0x1] [error] Svt[error]: Instance 1: odd width");
        let (problem, error) = encode_failure(
            &odd,
            HwApi::Software,
            temp,
            Place::WorkFolder,
            OutputMode::Replace,
        );
        assert_eq!(problem, ProblemKind::Encoder);
        assert!(
            error.starts_with(
                "Converting on the CPU stopped with an error, so the original was left \
                 unchanged. ffmpeg said: \""
            ),
            "{error}"
        );

        let stalled = FfmpegExit::Stalled {
            after: Duration::from_secs(600),
            tail: String::new(),
        };
        let (problem, error) = encode_failure(
            &stalled,
            HwApi::Vaapi,
            temp,
            Place::WorkFolder,
            OutputMode::Replace,
        );
        assert_eq!(problem, ProblemKind::Encoder);
        assert!(
            error.starts_with("Converting on the GPU (VA-API) stopped making progress for 10"),
            "{error}"
        );
        for (_, error) in [
            encode_failure(
                &disk_full,
                HwApi::Nvenc,
                temp,
                Place::Destination,
                OutputMode::Folder,
            ),
            encode_failure(
                &odd,
                HwApi::Software,
                temp,
                Place::Destination,
                OutputMode::Folder,
            ),
        ] {
            for jargon in ["libsvtav1", "libx264", "exit code", "os error"] {
                assert!(!error.contains(jargon), "{error}");
            }
        }
    }

    #[test]
    fn unusable_work_folders_say_what_is_in_the_way() {
        let exists = std::io::Error::from_raw_os_error(17);
        let (problem, error) = folder_unusable(
            Path::new("/temp"),
            Place::WorkFolder,
            OutputMode::Replace,
            &exists,
        );
        assert_eq!(problem, ProblemKind::WorkFolder);
        assert_eq!(
            error,
            "The work folder /temp can't be used because a file with that name is in the way. \
             Fix it, or choose another work folder, in Settings > Output."
        );
        let full = std::io::Error::from_raw_os_error(28);
        let (problem, error) = folder_unusable(
            Path::new("/out/TV"),
            Place::Destination,
            OutputMode::Folder,
            &full,
        );
        assert_eq!(problem, ProblemKind::DiskFull);
        assert_eq!(
            error,
            "The output folder /out/TV can't be created because the disk is full. Free up some \
             space there. Fix it, or choose another output folder, in Settings > Output."
        );
        let odd = std::io::Error::from_raw_os_error(5);
        let (problem, error) = folder_unusable(
            Path::new("/temp"),
            Place::WorkFolder,
            OutputMode::Replace,
            &odd,
        );
        assert_eq!(problem, ProblemKind::WorkFolder);
        assert!(
            error.contains("because the disk reported a read or write error"),
            "{error}"
        );
    }

    /// A folder the new file can't go to is named for what it is, with the
    /// fix that fits the output mode.
    #[test]
    fn blocked_destinations_say_which_folder_and_what_to_do() {
        let dir = Path::new("/out/TV");
        assert_eq!(
            destination_blocked(dir, OutputMode::Folder, WriteBlock::ReadOnly),
            "The output folder /out/TV is on a read-only drive, so the converted file can't be \
             put there. Make the drive writable, or choose another output folder in Settings > \
             Output."
        );
        assert_eq!(
            destination_blocked(dir, OutputMode::Folder, WriteBlock::Denied),
            "Chrysopoeia doesn't have permission to write in the output folder /out/TV, so the \
             converted file can't be put there. Check the folder's permissions (in Docker, the \
             PUID/PGID user needs write access), or choose another output folder in Settings > \
             Output."
        );
        let dir = Path::new("/media/Films");
        assert_eq!(
            destination_blocked(dir, OutputMode::Replace, WriteBlock::ReadOnly),
            "The original's folder /media/Films is on a read-only drive, so the converted file \
             can't be put there. Make the drive writable, or save converted files to a \
             separate folder in Settings > Output."
        );
        assert!(
            destination_blocked(dir, OutputMode::Replace, WriteBlock::Denied).starts_with(
                "Chrysopoeia doesn't have permission to write in the original's folder \
                 /media/Films, so"
            )
        );
    }

    #[test]
    fn no_room_messages_name_the_folder_and_the_fix() {
        let gb = 1_500_000_000;
        assert_eq!(
            no_room_message(
                Path::new("/temp"),
                gb,
                Place::WorkFolder,
                OutputMode::Replace
            ),
            "There isn't enough free space in the work folder /temp to convert this file (it \
             needs about 1.5 GB). Free up some space there, or choose a work folder on a bigger \
             disk in Settings > Output."
        );
        assert!(
            no_room_message(
                Path::new("/m/TV"),
                gb,
                Place::Destination,
                OutputMode::Replace
            )
            .starts_with("There isn't enough free space next to the original in /m/TV")
        );
        assert!(
            no_room_message(
                Path::new("/out"),
                gb,
                Place::Destination,
                OutputMode::Folder
            )
            .starts_with("There isn't enough free space in the output folder /out")
        );
    }

    #[test]
    fn planning_problems_with_the_original_are_unreadable_sources() {
        let source: anyhow::Error =
            crate::plan::SourceProblem("No audio can be read.".into()).into();
        assert_eq!(
            plan_failure(&source),
            (
                ProblemKind::UnreadableSource,
                "No audio can be read.".to_string()
            )
        );
        let other = anyhow::anyhow!("The path has odd characters. Renaming the file fixes this.");
        assert_eq!(plan_failure(&other).0, ProblemKind::Other);
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
            force: false,
        };
        let (tx, _rx) = mpsc::channel(1);
        let job = run_job(&cfg, &spec, tx, CancellationToken::new());
        assert_send(&job);
        drop(job);
    }
}
