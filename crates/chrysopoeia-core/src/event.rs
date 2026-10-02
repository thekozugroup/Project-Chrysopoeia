//! Real-time events pushed to the web UI over `GET /api/ws`.
//!
//! Every message is a JSON object with a `type` field, e.g.
//! `{"type":"job.progress","job_id":"...","progress":42.0,...}`.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::hardware::HardwareInfo;
use crate::job::{Job, JobProgress, ProblemKind};
use crate::library::{Library, LibraryStats, MediaFile};
use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActivityLevel {
    Info,
    Success,
    Warning,
    Error,
}

/// A line in the activity feed. Persisted; `GET /api/activity` returns the
/// most recent entries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityEntry {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub level: ActivityLevel,
    pub message: String,
    pub file_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub library_id: Option<Uuid>,
    /// What kind of problem the entry is about, for an entry written about
    /// a failed or skipped file whose problem is known (the same values as
    /// `Job::problem`), so the UI can group it without reading the
    /// sentence. `None` for everything else.
    #[serde(default)]
    pub problem: Option<ProblemKind>,
}

/// State of the job queue.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueState {
    /// User paused processing. Running jobs finish; no new ones start.
    pub paused: bool,
    pub running: u32,
    pub queued: u32,
    /// Effective concurrent job limit.
    pub max_jobs: u32,
    /// True when `max_jobs` comes from hardware detection.
    pub max_jobs_auto: bool,
    /// Where `max_jobs` comes from, so the UI can explain it.
    #[serde(default)]
    pub max_jobs_source: MaxJobsSource,
    /// True when jobs are waiting for the configured active hours.
    pub waiting_for_schedule: bool,
}

/// Origin of the effective concurrent job limit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaxJobsSource {
    /// Hardware detection (`recommended_jobs.total`).
    #[default]
    Auto,
    /// The `MAX_JOBS` environment variable, used while Settings say Automatic.
    Env,
    /// A number saved in Settings.
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanPhase {
    /// Walking folders.
    Discovering,
    /// Probing new or changed files.
    Analyzing,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanProgress {
    pub library_id: Uuid,
    pub library_name: String,
    pub phase: ScanPhase,
    /// Media files found on disk so far.
    pub discovered: u64,
    /// Files probed so far in this scan.
    pub analyzed: u64,
    /// Files that need probing (new or changed).
    pub to_analyze: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Event {
    #[serde(rename = "job.progress")]
    JobProgress(JobProgress),
    #[serde(rename = "job.updated")]
    JobUpdated { job: Job },
    #[serde(rename = "file.updated")]
    FileUpdated { file: MediaFile },
    /// Many files changed (e.g. after a scan); clients should refetch lists.
    #[serde(rename = "files.changed")]
    FilesChanged { library_id: Option<Uuid> },
    #[serde(rename = "library.updated")]
    LibraryUpdated { library: Library },
    #[serde(rename = "library.removed")]
    LibraryRemoved { id: Uuid },
    #[serde(rename = "scan.progress")]
    ScanProgress(ScanProgress),
    #[serde(rename = "queue.state")]
    QueueState(QueueState),
    #[serde(rename = "stats.updated")]
    StatsUpdated { totals: LibraryStats },
    #[serde(rename = "hardware.updated")]
    HardwareUpdated { hardware: Box<HardwareInfo> },
    #[serde(rename = "settings.updated")]
    SettingsUpdated { settings: Box<Settings> },
    #[serde(rename = "activity")]
    Activity { entry: ActivityEntry },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobStage;

    #[test]
    fn tagged_json_shape() {
        let ev = Event::JobProgress(JobProgress {
            job_id: Uuid::nil(),
            file_id: Uuid::nil(),
            stage: JobStage::Transcoding,
            progress: 42.5,
            fps: Some(120.0),
            speed: Some(4.8),
            eta_secs: Some(90),
            encoder: Some("hevc_nvenc".into()),
            hw_api: Some(crate::encoder::HwApi::Nvenc),
            attempt: 1,
        });
        let v: serde_json::Value = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "job.progress");
        assert_eq!(v["stage"], "transcoding");
        assert_eq!(v["hw_api"], "nvenc");

        let ev = Event::QueueState(QueueState {
            paused: true,
            ..Default::default()
        });
        let v: serde_json::Value = serde_json::to_value(&ev).unwrap();
        assert_eq!(v["type"], "queue.state");
        assert_eq!(v["paused"], true);
    }
}
