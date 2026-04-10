//! Pure oximedia transcoding backend.
//!
//! Uses the oximedia-transcode Transcoder API for native AV1, VP9, and Opus encoding.
//! Only available when the `oximedia` feature is enabled.

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

    #[cfg(feature = "oximedia")]
    {
        // TODO: Implement using oximedia-transcode when available:
        //
        // let mut transcoder = oximedia_transcode::Transcoder::builder()
        //     .input(&media_file.path)
        //     .output(&output_path)
        //     .video_codec(target_codec)
        //     .audio_codec("opus")
        //     .on_progress(|progress| { ... })
        //     .build()?;
        //
        // transcoder.run().await?;
        let _ = (media_file, target_codec, &progress_tx);
        anyhow::bail!("oximedia native transcoding not yet implemented")
    }

    #[cfg(not(feature = "oximedia"))]
    {
        tracing::warn!(
            job_id = %job.id,
            "oximedia feature not enabled; cannot use native transcoding pipeline"
        );
        let _ = (media_file, target_codec, &progress_tx);
        anyhow::bail!(
            "oximedia native transcoding requires the 'oximedia' feature to be enabled"
        )
    }
}
