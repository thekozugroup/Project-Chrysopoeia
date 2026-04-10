//! Unified progress tracking for transcode backends.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A progress update emitted by a transcoding backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressUpdate {
    /// The job this update belongs to.
    pub job_id: Uuid,
    /// Completion percentage (0-100).
    pub percent: u8,
    /// Encoding speed (e.g. "1.4x").
    pub speed: Option<String>,
    /// Current encoding speed in frames per second.
    pub fps: Option<f64>,
    /// Estimated seconds remaining.
    pub eta_secs: Option<f64>,
}

/// High-level event emitted by the transcode engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProgressEvent {
    /// A job has started transcoding.
    Started { job_id: Uuid, filename: String },
    /// Progress update during transcoding.
    Progress(ProgressUpdate),
    /// A job completed successfully.
    Complete {
        job_id: Uuid,
        output_path: String,
        output_size: u64,
    },
    /// A job was skipped (already in target format).
    Skipped { job_id: Uuid, reason: String },
    /// A job failed.
    Failed { job_id: Uuid, error: String },
    /// A job was cancelled.
    Cancelled { job_id: Uuid },
}

/// A receiver handle for progress updates from the worker pool.
pub struct ProgressReceiver {
    rx: tokio::sync::broadcast::Receiver<ProgressEvent>,
}

impl ProgressReceiver {
    /// Create a new progress receiver wrapping a broadcast channel.
    pub fn new(rx: tokio::sync::broadcast::Receiver<ProgressEvent>) -> Self {
        Self { rx }
    }

    /// Wait for the next progress event.
    pub async fn recv(&mut self) -> Option<ProgressEvent> {
        self.rx.recv().await.ok()
    }
}
