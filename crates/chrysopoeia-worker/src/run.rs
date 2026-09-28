//! Job orchestration: plan, encode with fallbacks, verify, finalize.
//! Implemented by the worker-run agent.

use std::path::PathBuf;

use chrysopoeia_core::{
    EncoderCandidate, HwApi, JobProgress, OutputMode, ProbeInfo, TranscodeProfile,
    ValidationLevel, ValidationReport,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Settings that apply to every job.
#[derive(Debug, Clone)]
pub struct RunConfig {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Scratch directory for encodes; `None` writes next to the source.
    pub temp_dir: Option<PathBuf>,
    pub validation: ValidationLevel,
    pub output_mode: OutputMode,
    pub output_folder: Option<PathBuf>,
    pub keep_file_dates: bool,
    /// Run ffmpeg under `nice -n 10` when available.
    pub low_priority: bool,
}

/// One file to process.
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub job_id: Uuid,
    pub file_id: Uuid,
    pub input: PathBuf,
    /// Root of the library the file belongs to (for `OutputMode::Folder`).
    pub library_root: PathBuf,
    pub probe: ProbeInfo,
    pub profile: TranscodeProfile,
    /// Encoders to try in order (from `chrysopoeia_hwdetect::encoder_candidates`).
    pub candidates: Vec<EncoderCandidate>,
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq)]
pub enum JobOutcome {
    Done {
        output_path: PathBuf,
        output_size: u64,
        original_size: u64,
        encoder: String,
        hw_api: HwApi,
        attempt: u32,
        validation: Option<ValidationReport>,
        command: String,
        notes: Vec<String>,
    },
    /// Nothing written: no work needed, or the result was not worth keeping
    /// (e.g. not smaller). The original is untouched.
    Skipped {
        reason: String,
        encoder: Option<String>,
        output_size: Option<u64>,
    },
    /// The original is untouched.
    Failed {
        error: String,
        log_tail: Option<String>,
        command: Option<String>,
        encoder: Option<String>,
        attempt: u32,
        validation: Option<ValidationReport>,
    },
    Cancelled,
}

/// Run one job to completion. Progress is sent at most ~2 times per second.
/// Never panics on bad input; all problems become `Failed`.
pub async fn run_job(
    cfg: &RunConfig,
    spec: &JobSpec,
    progress: mpsc::Sender<JobProgress>,
    cancel: CancellationToken,
) -> JobOutcome {
    let _ = (cfg, spec, progress, cancel);
    todo!("implemented by the worker-run agent")
}
