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
    /// Current encoding speed in frames per second (if applicable).
    pub fps: Option<f64>,
    /// Estimated seconds remaining.
    pub eta_secs: Option<f64>,
}

/// A receiver handle for progress updates from the worker pool.
pub struct ProgressReceiver {
    rx: tokio::sync::mpsc::Receiver<ProgressUpdate>,
}

impl ProgressReceiver {
    /// Create a new progress receiver wrapping a channel.
    pub fn new(rx: tokio::sync::mpsc::Receiver<ProgressUpdate>) -> Self {
        Self { rx }
    }

    /// Wait for the next progress update.
    pub async fn recv(&mut self) -> Option<ProgressUpdate> {
        self.rx.recv().await
    }
}
