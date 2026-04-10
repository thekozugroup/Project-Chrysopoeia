//! Core transcoding engine that manages concurrent jobs.

use std::collections::HashMap;
use std::sync::Arc;

use chrysopeia_core::models::{HardwareCapability, MediaFile, TranscodeJob, TranscodeStatus};
use tokio::sync::{mpsc, RwLock};
use uuid::Uuid;

use crate::progress::ProgressUpdate;
use crate::strategy::EncodingStrategy;

/// The main transcoding engine. Holds hardware capabilities and manages
/// concurrent transcode jobs.
pub struct TranscodeEngine {
    /// Detected hardware capabilities.
    capabilities: Vec<HardwareCapability>,
    /// Maximum number of concurrent jobs.
    max_concurrent: usize,
    /// Currently running jobs, keyed by job ID.
    active_jobs: Arc<RwLock<HashMap<Uuid, tokio::task::JoinHandle<()>>>>,
    /// Channel for progress updates from workers.
    progress_tx: mpsc::Sender<ProgressUpdate>,
}

impl TranscodeEngine {
    /// Create a new transcode engine with detected capabilities.
    pub fn new(
        capabilities: Vec<HardwareCapability>,
        max_concurrent: usize,
        progress_tx: mpsc::Sender<ProgressUpdate>,
    ) -> Self {
        Self {
            capabilities,
            max_concurrent,
            active_jobs: Arc::new(RwLock::new(HashMap::new())),
            progress_tx,
        }
    }

    /// Process a transcode job for the given media file.
    ///
    /// Selects the optimal encoding strategy and dispatches to the
    /// appropriate backend (oximedia native or FFmpeg).
    pub async fn process_job(
        &self,
        job: &mut TranscodeJob,
        media_file: &MediaFile,
    ) -> anyhow::Result<()> {
        let strategy = crate::strategy::select_strategy(media_file, &self.capabilities);
        tracing::info!(
            job_id = %job.id,
            strategy = ?strategy,
            "Processing transcode job"
        );

        job.status = TranscodeStatus::Running;
        job.started_at = Some(chrono::Utc::now());

        let result = match strategy {
            EncodingStrategy::Skip => {
                tracing::info!(job_id = %job.id, "File already in open format, skipping");
                Ok(())
            }
            EncodingStrategy::OximediaNative { codec } => {
                crate::oximedia_backend::transcode(
                    media_file,
                    job,
                    &codec,
                    self.progress_tx.clone(),
                )
                .await
            }
            EncodingStrategy::FfmpegCli {
                codec,
                hw_accel_flags,
            } => {
                crate::ffmpeg_backend::transcode(
                    media_file,
                    job,
                    &codec,
                    hw_accel_flags.as_deref(),
                    self.progress_tx.clone(),
                )
                .await
            }
        };

        match result {
            Ok(()) => {
                job.status = TranscodeStatus::Complete;
                job.completed_at = Some(chrono::Utc::now());
                job.progress = 100;
            }
            Err(e) => {
                job.status = TranscodeStatus::Failed;
                job.completed_at = Some(chrono::Utc::now());
                job.error_msg = Some(e.to_string());
            }
        }

        Ok(())
    }

    /// Cancel a running transcode job.
    pub async fn cancel_job(&self, job_id: Uuid) -> anyhow::Result<()> {
        let mut active = self.active_jobs.write().await;
        if let Some(handle) = active.remove(&job_id) {
            handle.abort();
            tracing::info!(job_id = %job_id, "Cancelled transcode job");
        }
        Ok(())
    }

    /// Returns the number of currently active jobs.
    pub async fn active_job_count(&self) -> usize {
        self.active_jobs.read().await.len()
    }

    /// Returns `true` if there is capacity for another concurrent job.
    pub async fn has_capacity(&self) -> bool {
        self.active_job_count().await < self.max_concurrent
    }
}
