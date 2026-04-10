//! Core transcoding engine with worker pool and cancellation support.

use std::collections::HashMap;
use std::sync::Arc;

use chrysopeia_core::models::{HardwareCapability, MediaFile, TranscodeJob, TranscodeStatus};
use tokio::sync::{broadcast, mpsc, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::ffmpeg_backend;
use crate::progress::{ProgressEvent, ProgressUpdate};
use crate::strategy::{self, EncodingStrategy};

/// Active job handle for tracking and cancellation.
struct ActiveJob {
    handle: tokio::task::JoinHandle<()>,
    cancel: CancellationToken,
}

/// The main transcoding engine.
pub struct TranscodeEngine {
    capabilities: Vec<HardwareCapability>,
    semaphore: Arc<Semaphore>,
    active_jobs: Arc<RwLock<HashMap<Uuid, ActiveJob>>>,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    event_tx: broadcast::Sender<ProgressEvent>,
}

impl TranscodeEngine {
    /// Create a new engine.
    ///
    /// `event_tx` is a broadcast channel for high-level events (the server
    /// subscribes and forwards to WebSocket clients).
    pub fn new(
        capabilities: Vec<HardwareCapability>,
        max_concurrent: usize,
        event_tx: broadcast::Sender<ProgressEvent>,
    ) -> Self {
        let (progress_tx, _progress_rx) = mpsc::channel(256);
        Self {
            capabilities,
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            active_jobs: Arc::new(RwLock::new(HashMap::new())),
            progress_tx,
            event_tx,
        }
    }

    /// Submit a transcode job. Blocks until a concurrency slot is available.
    pub async fn process_job(
        &self,
        job: &mut TranscodeJob,
        media_file: &MediaFile,
        target_video: &str,
        target_audio: &str,
        crf: u32,
    ) -> anyhow::Result<()> {
        let strat = strategy::select_strategy(media_file, &self.capabilities);

        match strat {
            EncodingStrategy::Skip => {
                tracing::info!(job_id = %job.id, "File already in open format, skipping");
                job.status = TranscodeStatus::Complete;
                let _ = self.event_tx.send(ProgressEvent::Skipped {
                    job_id: job.id,
                    reason: "Already in open format".into(),
                });
                return Ok(());
            }
            EncodingStrategy::OximediaNative { .. } => {
                // oximedia not wired yet — fall through to ffmpeg
                tracing::info!(job_id = %job.id, "oximedia native not available, using ffmpeg");
            }
            EncodingStrategy::FfmpegCli { .. } => {}
        }

        // Determine encoder names
        let hw_accel = match &strat {
            EncodingStrategy::FfmpegCli { hw_accel_flags, .. } => hw_accel_flags.as_deref(),
            _ => None,
        };
        let video_encoder = ffmpeg_backend::codec_to_encoder(target_video, hw_accel);
        let audio_encoder = ffmpeg_backend::codec_to_encoder(target_audio, None);

        // Acquire concurrency slot
        let _permit = self.semaphore.acquire().await?;

        job.status = TranscodeStatus::Running;
        job.started_at = Some(chrono::Utc::now());

        let cancel = CancellationToken::new();
        let (ptx, mut prx) = mpsc::channel::<ProgressUpdate>(64);

        let _ = self.event_tx.send(ProgressEvent::Started {
            job_id: job.id,
            filename: media_file.path.clone(),
        });

        // Forward per-job progress updates to the broadcast channel
        let event_tx = self.event_tx.clone();
        let forward_task = tokio::spawn(async move {
            while let Some(update) = prx.recv().await {
                let _ = event_tx.send(ProgressEvent::Progress(update));
            }
        });

        // Run the actual transcode
        let result = ffmpeg_backend::transcode(
            media_file,
            job,
            video_encoder,
            audio_encoder,
            crf,
            hw_accel,
            ptx,
            cancel.clone(),
        )
        .await;

        forward_task.abort();

        match result {
            Ok(tr) => {
                job.status = TranscodeStatus::Complete;
                job.completed_at = Some(chrono::Utc::now());
                job.progress = 100;
                job.output_path = Some(tr.output_path.clone());
                job.size_reduction_pct = Some(
                    (1.0 - tr.output_size as f64 / media_file.size as f64) * 100.0,
                );
                let _ = self.event_tx.send(ProgressEvent::Complete {
                    job_id: job.id,
                    output_path: tr.output_path,
                    output_size: tr.output_size,
                });
            }
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("cancelled") {
                    job.status = TranscodeStatus::Cancelled;
                    let _ = self.event_tx.send(ProgressEvent::Cancelled { job_id: job.id });
                } else {
                    job.status = TranscodeStatus::Failed;
                    job.error_msg = Some(msg.clone());
                    let _ = self.event_tx.send(ProgressEvent::Failed {
                        job_id: job.id,
                        error: msg,
                    });
                }
                job.completed_at = Some(chrono::Utc::now());
            }
        }

        // Remove from active jobs
        self.active_jobs.write().await.remove(&job.id);

        Ok(())
    }

    /// Cancel a running job.
    pub async fn cancel_job(&self, job_id: Uuid) -> bool {
        let active = self.active_jobs.read().await;
        if let Some(job) = active.get(&job_id) {
            job.cancel.cancel();
            true
        } else {
            false
        }
    }

    /// Cancel all running jobs (for graceful shutdown).
    pub async fn cancel_all(&self) {
        let active = self.active_jobs.read().await;
        for (id, job) in active.iter() {
            tracing::info!(job_id = %id, "Cancelling job for shutdown");
            job.cancel.cancel();
        }
    }

    /// Number of currently active jobs.
    pub async fn active_count(&self) -> usize {
        self.active_jobs.read().await.len()
    }

    /// Subscribe to progress events.
    pub fn subscribe(&self) -> broadcast::Receiver<ProgressEvent> {
        self.event_tx.subscribe()
    }
}
