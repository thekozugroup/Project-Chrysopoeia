//! Transcode jobs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::encoder::HwApi;
use crate::validation::{ValidationCheck, ValidationReport};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    /// Transcoded, verified and put in place.
    Done,
    /// Nothing to do, or the result was not worth keeping (see `skip_reason`).
    Skipped,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_finished(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Skipped | Self::Failed | Self::Cancelled
        )
    }
}

/// Why a file could not be converted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    /// The original is damaged, truncated or not really a video. Retrying
    /// won't help; the user should check or replace the file.
    UnreadableSource,
    /// The work (temp) folder is missing, not writable or unusable.
    WorkFolder,
    /// The library or output folder can't be written (read-only mount,
    /// permissions), so the result can't be put in place.
    Destination,
    /// Not enough free space for the work file or the result.
    DiskFull,
    /// The encoder failed on every attempt (including hardware errors).
    Encoder,
    /// The chosen hardware isn't available and CPU fallback is off.
    HardwareUnavailable,
    /// The new file failed verification on every attempt.
    Verification,
    /// The original changed or disappeared while it was being converted.
    SourceChanged,
    /// Anything else.
    Other,
}

/// Where a running job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStage {
    Waiting,
    Preparing,
    Transcoding,
    Verifying,
    Finalizing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub file_id: Uuid,
    pub library_id: Uuid,
    pub file_name: String,
    pub file_path: String,
    pub state: JobState,
    pub stage: JobStage,
    /// Higher runs first.
    pub priority: i32,
    /// Overall progress 0..=100 for the current stage.
    pub progress: f32,
    pub fps: Option<f32>,
    /// Realtime multiple (2.5 = 2.5x realtime).
    pub speed: Option<f32>,
    pub eta_secs: Option<u64>,
    /// How `progress` was worked out while transcoding (see
    /// [`ProgressBasis`]). `None` outside transcoding, and for jobs a server
    /// older than this field recorded: `progress` is then as it says.
    #[serde(default)]
    pub progress_basis: Option<ProgressBasis>,
    /// Video frames the current attempt has encoded so far (transcoding
    /// only, when ffmpeg says).
    #[serde(default)]
    pub frames: Option<u64>,
    /// Seconds since the current attempt started encoding (transcoding
    /// only).
    #[serde(default)]
    pub elapsed_secs: Option<u64>,
    /// ffmpeg encoder in use or used.
    pub encoder: Option<String>,
    pub hw_api: Option<HwApi>,
    /// 1-based attempt number within the fallback chain.
    pub attempt: u32,
    /// Every way of converting the file this job tried, in order, each with
    /// how it ended: the attempts of its latest run, the one still running
    /// left out. Empty for a job that hasn't finished an attempt, and for
    /// jobs that finished before this was recorded.
    #[serde(default)]
    pub attempts: Vec<JobAttempt>,
    pub input_size: u64,
    pub output_size: Option<u64>,
    /// The disk space this conversion actually released: the input's size
    /// minus the output's in the usual case; 0 when the original's space
    /// was not released (the original was hard-linked, so replacing it
    /// freed nothing) or the result is not smaller. `None` for a job that
    /// is not done, and for one that finished before this was recorded.
    #[serde(default)]
    pub freed_bytes: Option<u64>,
    /// The file name of the result when it differs from the original's
    /// (for example the extension changed to `.mkv`). `None` when the name
    /// is the same, when the job is not done, or when it is not known.
    #[serde(default)]
    pub output_name: Option<String>,
    pub error: Option<String>,
    /// Machine-readable cause of `error`, so the UI can group problems and
    /// offer the right fix without parsing sentences.
    #[serde(default)]
    pub problem: Option<ProblemKind>,
    pub skip_reason: Option<String>,
    pub validation: Option<ValidationReport>,
    /// The ffmpeg command line of the final attempt.
    pub command: Option<String>,
    /// Last lines of ffmpeg output when something went wrong.
    pub log_tail: Option<String>,
    /// Plain-language notes about compromises the conversion made, e.g.
    /// "Removed 2 picture-based subtitles because MP4 can't hold them".
    #[serde(default)]
    pub notes: Vec<String>,
    /// "Convert anyway": queued with `force`, so the file is converted even
    /// when it is already efficient or in the target format, and kept
    /// whatever its size. Verification still applies.
    #[serde(default)]
    pub force: bool,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

/// How a running job's `progress` was worked out while transcoding.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressBasis {
    /// From how far into the file ffmpeg says the new file is (its output
    /// time over the original's length), or 100 when ffmpeg says it has
    /// finished.
    Time,
    /// Estimated from the video frames encoded over the frames the original
    /// should have (its length times its frame rate): ffmpeg didn't say how
    /// far it is, or what it said is behind the video (ffmpeg 7 reports the
    /// track that is furthest behind, such as a subtitle track that has had
    /// no line yet).
    Frames,
    /// Not known: ffmpeg didn't say how far it is, and the frames encoded
    /// can't be turned into a share of the file. `progress` is 0 and means
    /// nothing, and there is no time left; show the frames encoded and the
    /// time spent instead.
    #[default]
    Unknown,
}

/// How one attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptResult {
    /// It made a new file that passed its checks (or checks are off).
    Succeeded,
    /// ffmpeg failed, or the new file failed a check.
    Failed,
}

/// One way of converting the file a job tried: an encoder, with the
/// original decoded on the GPU or on the CPU (see `Job::attempts`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobAttempt {
    /// 1-based, as `Job::attempt`.
    pub attempt: u32,
    /// ffmpeg encoder, e.g. `hevc_vaapi`.
    pub encoder: String,
    pub hw_api: HwApi,
    /// The GPU's render node (VA-API, Quick Sync), e.g. `/dev/dri/renderD128`.
    #[serde(default)]
    pub device: Option<String>,
    /// The GPU decoded the original; `false`: the CPU did.
    pub hw_decode: bool,
    /// How long the attempt took, its checks included, in seconds.
    pub elapsed_secs: f64,
    pub result: AttemptResult,
    /// What went wrong, in plain words (a failed attempt).
    #[serde(default)]
    pub error: Option<String>,
    /// What kind of problem `error` is.
    #[serde(default)]
    pub problem: Option<ProblemKind>,
    /// The check the new file failed, with its plain reason, when a check
    /// is what failed.
    #[serde(default)]
    pub failed_check: Option<ValidationCheck>,
    /// The ffmpeg command line, shell-quoted for display.
    #[serde(default)]
    pub command: Option<String>,
    /// The last lines ffmpeg printed, for a failed attempt (at most a dozen).
    #[serde(default)]
    pub log_tail: Option<String>,
}

/// Live progress for a running job. Sent by the worker, forwarded over the
/// WebSocket as `job.progress`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobProgress {
    pub job_id: Uuid,
    pub file_id: Uuid,
    pub stage: JobStage,
    pub progress: f32,
    pub fps: Option<f32>,
    pub speed: Option<f32>,
    pub eta_secs: Option<u64>,
    /// See `Job::progress_basis`.
    #[serde(default)]
    pub progress_basis: Option<ProgressBasis>,
    /// See `Job::frames`.
    #[serde(default)]
    pub frames: Option<u64>,
    /// See `Job::elapsed_secs`.
    #[serde(default)]
    pub elapsed_secs: Option<u64>,
    pub encoder: Option<String>,
    pub hw_api: Option<HwApi>,
    pub attempt: u32,
    /// The attempts so far, sent by the worker with the update that follows
    /// the end of an attempt. The server stores them and sends the job as
    /// `job.updated`; `job.progress` events never carry them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<Vec<JobAttempt>>,
}
