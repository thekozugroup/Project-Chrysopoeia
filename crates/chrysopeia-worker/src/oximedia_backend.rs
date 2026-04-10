//! Pure oximedia transcoding backend.
//!
//! Uses the oximedia-transcode Transcoder API for native AV1, VP9, and Opus encoding.

use chrysopeia_core::models::{MediaFile, TranscodeJob};
use tokio::sync::mpsc;

use crate::progress::ProgressUpdate;

/// Transcode a media file using the native oximedia pipeline.
///
/// Supports AV1, VP9 video and Opus audio encoding with progress callbacks.
pub async fn transcode(
    media_file: &MediaFile,
    job: &TranscodeJob,
    target_codec: &str,
    progress_tx: mpsc::Sender<ProgressUpdate>,
) -> anyhow::Result<()> {
    tracing::info!(
        job_id = %job.id,
        codec = target_codec,
        "Starting oximedia native transcode"
    );

    // TODO: Implement using oximedia-transcode:
    //
    // let mut transcoder = oximedia_transcode::Transcoder::builder()
    //     .input(&media_file.path)
    //     .output(&output_path)
    //     .video_codec(target_codec)
    //     .audio_codec("opus")
    //     .on_progress(|progress| {
    //         let _ = progress_tx.try_send(ProgressUpdate {
    //             job_id: job.id,
    //             percent: progress.percent,
    //             fps: progress.fps,
    //             eta_secs: progress.eta_secs,
    //         });
    //     })
    //     .build()?;
    //
    // transcoder.run().await?;

    todo!("Implement oximedia native transcoding pipeline")
}
