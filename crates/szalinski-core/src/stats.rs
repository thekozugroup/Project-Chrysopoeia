//! Aggregates for the overview screen.

use serde::{Deserialize, Serialize};

use crate::event::QueueState;
use crate::library::LibraryStats;

/// Count of files (and their bytes) per codec, resolution or other bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodecCount {
    pub name: String,
    pub files: u64,
    pub bytes: u64,
}

/// Space saved on one day.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavingsPoint {
    /// `YYYY-MM-DD` (UTC).
    pub date: String,
    pub saved_bytes: i64,
    pub files: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    pub totals: LibraryStats,
    pub video_codecs: Vec<CodecCount>,
    pub audio_codecs: Vec<CodecCount>,
    pub resolutions: Vec<CodecCount>,
    /// Last 30 days, oldest first; days without activity are included as 0.
    pub savings_history: Vec<SavingsPoint>,
    /// Projected additional savings for files still pending or queued, based
    /// on the average ratio achieved so far. `None` until enough files are done.
    pub projected_savings_bytes: Option<i64>,
    pub queue: QueueState,
}
