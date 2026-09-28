//! Libraries (watched folders) and the files in them.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::media::{HdrFormat, ProbeInfo};
use crate::profile::TranscodeProfile;

/// Lifecycle of a file, as the user sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileStatus {
    /// Needs work but is not queued (auto-queue off, or user removed it).
    Pending,
    Queued,
    Processing,
    /// Transcoded and verified.
    Done,
    /// No work needed, or not worth it; see `skip_reason`.
    Skipped,
    /// Last attempt failed; see `error`.
    Failed,
}

impl FileStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Done => "done",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pending" => Self::Pending,
            "queued" => Self::Queued,
            "processing" => Self::Processing,
            "done" => Self::Done,
            "skipped" => Self::Skipped,
            "failed" => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaFile {
    pub id: Uuid,
    pub library_id: Uuid,
    /// Absolute path on the server.
    pub path: String,
    /// Path relative to the library root, for display.
    pub relative_path: String,
    pub file_name: String,
    /// Current size on disk.
    pub size_bytes: u64,
    pub modified_at: DateTime<Utc>,
    pub status: FileStatus,
    /// Source container as reported by ffprobe (e.g. `matroska`).
    pub container: Option<String>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    /// Resolution label, e.g. `1080p`.
    pub resolution: Option<String>,
    pub hdr: Option<HdrFormat>,
    pub duration_secs: Option<f64>,
    pub bit_rate: Option<u64>,
    /// Size before Chrysopoeia replaced it (set once done).
    pub original_size_bytes: Option<u64>,
    /// `original_size_bytes - size_bytes` once done.
    pub saved_bytes: Option<i64>,
    pub skip_reason: Option<String>,
    pub error: Option<String>,
    /// Most recent job for this file.
    pub job_id: Option<Uuid>,
    /// Live progress (0..=100) while processing.
    pub progress: Option<f32>,
    /// Full probe; only included by the file detail endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<ProbeInfo>,
    pub scanned_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryStats {
    pub file_count: u64,
    pub total_bytes: u64,
    pub pending: u64,
    pub queued: u64,
    pub processing: u64,
    pub done: u64,
    pub skipped: u64,
    pub failed: u64,
    pub saved_bytes: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Library {
    pub id: Uuid,
    pub name: String,
    /// Absolute folder path on the server.
    pub path: String,
    pub enabled: bool,
    pub profile: TranscodeProfile,
    pub stats: LibraryStats,
    pub scanning: bool,
    pub last_scan_at: Option<DateTime<Utc>>,
    /// Set when the folder is missing or unreadable.
    pub path_error: Option<String>,
    pub created_at: DateTime<Utc>,
}
