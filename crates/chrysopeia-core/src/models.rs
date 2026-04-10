//! Data models for media files, transcode jobs, and related entities.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A media file discovered in the library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFile {
    pub id: Uuid,
    pub path: String,
    pub size: u64,
    pub format: MediaFormat,
    pub status: MediaFileStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Status of a media file in the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaFileStatus {
    /// Discovered but not yet analyzed.
    Pending,
    /// Already in an open format; no transcoding needed.
    OpenFormat,
    /// Needs transcoding to an open format.
    NeedsTranscode,
    /// Currently being transcoded.
    Transcoding,
    /// Transcoding completed successfully.
    Transcoded,
    /// An error occurred during analysis or transcoding.
    Error,
}

/// Describes the container and codec makeup of a media file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaFormat {
    pub container: String,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub resolution: Option<Resolution>,
    pub video_bitrate: Option<u64>,
    pub audio_bitrate: Option<u64>,
    pub duration_secs: Option<f64>,
}

/// Video resolution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

/// A transcode job targeting a specific media file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscodeJob {
    pub id: Uuid,
    pub media_file_id: Uuid,
    pub status: TranscodeStatus,
    /// Progress percentage, 0-100.
    pub progress: u8,
    pub target_codec: String,
    pub target_container: String,
    pub hw_accel: bool,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error_msg: Option<String>,
    pub output_path: Option<String>,
    /// Percentage reduction in file size after transcoding.
    pub size_reduction_pct: Option<f64>,
}

/// Status of a transcode job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscodeStatus {
    Pending,
    Queued,
    Running,
    Complete,
    Failed,
    Cancelled,
}

/// Hardware capability descriptor for a single device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareCapability {
    pub device_name: String,
    pub supports_av1: bool,
    pub supports_vp9: bool,
    pub supports_hevc: bool,
    pub api: HwAccelApi,
}

/// Hardware acceleration API type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HwAccelApi {
    Vulkan,
    Vaapi,
    Nvenc,
    Qsv,
    VideoToolbox,
}

/// Aggregate statistics for the media library.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryStats {
    pub total_files: u64,
    pub transcoded: u64,
    pub pending: u64,
    pub total_size: u64,
    pub saved_size: u64,
}

/// A chat message in the conversational interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: Uuid,
    pub role: ChatRole,
    pub content: String,
    pub timestamp: DateTime<Utc>,
    /// Optionally associated transcode job.
    pub job_id: Option<Uuid>,
}

/// Role of a chat message sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatRole {
    User,
    Assistant,
    System,
}
