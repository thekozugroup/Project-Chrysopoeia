//! Transcode jobs.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::encoder::HwApi;
use crate::validation::ValidationReport;

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
    /// ffmpeg encoder in use or used.
    pub encoder: Option<String>,
    pub hw_api: Option<HwApi>,
    /// 1-based attempt number within the fallback chain.
    pub attempt: u32,
    pub input_size: u64,
    pub output_size: Option<u64>,
    pub error: Option<String>,
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
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
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
    pub encoder: Option<String>,
    pub hw_api: Option<HwApi>,
    pub attempt: u32,
}
