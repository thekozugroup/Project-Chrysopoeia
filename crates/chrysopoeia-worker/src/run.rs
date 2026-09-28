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

use chrysopoeia_core::{
    EncoderCandidate, HwApi, JobProgress, JobStage, OutputMode, ProbeInfo, TranscodeProfile,
    ValidationLevel, ValidationReport,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::ffmpeg::{
    DEFAULT_STALL_TIMEOUT, FfmpegCommand, FfmpegExit, compute_progress, display_command,
    is_input_damage, run_ffmpeg,
};
use crate::finalize::{
    FileIdentity, FinalizeRequest, OriginalChanged, destination_conflict, final_output_path,
    finalize, temp_output_path,
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
        // The caller reports failures to the user (and the log); a second
        // warning here would only repeat it.
        JobOutcome::Failed { error, .. } => {
            tracing::debug!(job = %spec.job_id, %file, "failed: {error}");
        }
        JobOutcome::Cancelled => tracing::info!(job = %spec.job_id, %file, "cancelled"),
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
            _ => {
                guard.discard().await;
                remove_empty_dirs(&prepared.created_dirs).await;
            }
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
        // The new file goes into this folder (and, when replacing, the
        // original is renamed there); find out now rather than after hours
        // of encoding.
        if let Some(dir) = final_path.parent()
            && let Some(problem) = folder_not_writable(dir).await
        {
            return Err(failed(problem));
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
        let created_dirs = match create_dirs(&temp_dir).await {
            Ok(created) => created,
            Err(e) => {
                return Err(failed(format!(
                    "Could not use the temp folder {}: {e}",
                    temp_dir.display()
                )));
            }
        };
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
        let largest_kept = match self.spec.profile.min_savings_pct {
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
                what: "this file",
                strict: true,
            },
            SpaceNeed {
                dir: final_dir,
                bytes: largest_kept,
                file: None,
                what: "the new file",
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
                Err(Shortfall::Never { dir, bytes, what }) => {
                    return Err(failed(format!(
                        "Not enough free space in {} for {what} (needs about {})",
                        dir.display(),
                        human_bytes(bytes)
                    )));
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

            let run = self.encode(&args).await;
            let exit = run.exit;
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
                    if is_last {
                        tracing::debug!(job = %spec.job_id, attempt, "{error}");
                    } else {
                        tracing::info!(job = %spec.job_id, attempt, "{error}; trying the next option");
                    }
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

            // An original that ends early (a cut-off download or copy) makes
            // a short but otherwise fine encode; say so plainly instead of
            // letting it fail verification (or, unverified, replace the
            // original). Checked before the size rule: a stub is always small.
            let expected = self.spec.probe.duration_secs;
            let unverified = cfg.validation == ValidationLevel::Off;
            if let Some(stop) = source_stops_early(
                expected,
                run.encoded_secs,
                run.input_damage.is_some(),
                unverified,
            ) {
                return failure(
                    damaged_source_message(stop),
                    exit.tail().map(str::to_string),
                )
                .into_outcome();
            }

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
                Err(_) => return failed("The file no longer exists"),
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
                let too_short = report.first_failure().is_some_and(|c| c.id == "duration");
                if let Some(stop) = too_short
                    .then(|| source_stops_early(expected, run.encoded_secs, true, false))
                    .flatten()
                {
                    let mut f = failure(damaged_source_message(stop), None);
                    f.validation = Some(report.clone());
                    return f.into_outcome();
                }
                let mut f = failure(verification_error(report), None);
                f.validation = Some(report.clone());
                if candidate.api.is_hardware() && !is_last {
                    tracing::info!(
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
                    let mut f = failure(format!("{e:#}"), None);
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
                    f.error = format!("All {attempt} attempts failed. Last error: {}", f.error);
                }
                f.into_outcome()
            }
            None => failed("No encoder could be used for this file"),
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

/// Why new files can't be written in `dir` (or, when it doesn't exist yet,
/// in the closest folder above it that does), if they can't.
async fn folder_not_writable(dir: &Path) -> Option<String> {
    let dir = dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let existing = dir.ancestors().find(|d| d.is_dir())?;
        if writable(existing) {
            return None;
        }
        Some(format!(
            "Chrysopoeia doesn't have permission to write in {}, so the converted file can't be \
             put there. Check the folder's permissions (in Docker, the PUID/PGID user needs write \
             access)",
            existing.display()
        ))
    })
    .await
    .ok()
    .flatten()
}

/// Whether this process may create files in `dir` (as the kernel sees it,
/// so root may write in read-only folders).
#[cfg(unix)]
fn writable(dir: &Path) -> bool {
    rustix::fs::access(
        dir,
        rustix::fs::Access::WRITE_OK | rustix::fs::Access::EXEC_OK,
    )
    .is_ok()
}

#[cfg(not(unix))]
fn writable(dir: &Path) -> bool {
    std::fs::metadata(dir).is_ok_and(|m| !m.permissions().readonly())
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
    /// "this file" / "the new file", for the error message.
    what: &'static str,
    /// Fail when the room can never be found. A non-strict need only waits
    /// for other jobs, and is reserved as it is otherwise.
    strict: bool,
}

/// Why room could not be reserved.
#[derive(Debug)]
enum Shortfall {
    /// Not enough even if no other job were running.
    Never {
        dir: PathBuf,
        bytes: u64,
        what: &'static str,
    },
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
                what: need.what,
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
            what: "this file",
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
