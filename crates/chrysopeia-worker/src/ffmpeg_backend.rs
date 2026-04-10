//! FFmpeg CLI fallback transcoding backend.
//!
//! Builds and executes ffmpeg commands with appropriate hardware acceleration
//! flags, parsing progress from stderr.

use chrysopeia_core::models::{MediaFile, TranscodeJob};
use tokio::sync::mpsc;

use crate::progress::ProgressUpdate;

/// Transcode a media file using the FFmpeg CLI.
///
/// Constructs an ffmpeg command with the specified codec and optional
/// hardware acceleration flags. Parses progress from stderr output.
pub async fn transcode(
    media_file: &MediaFile,
    job: &TranscodeJob,
    target_codec: &str,
    hw_accel_flags: Option<&str>,
    progress_tx: mpsc::Sender<ProgressUpdate>,
) -> anyhow::Result<()> {
    let output_path = compute_output_path(&media_file.path, &job.target_container);
    let args = build_ffmpeg_args(
        &media_file.path,
        &output_path,
        target_codec,
        hw_accel_flags,
    );

    tracing::info!(
        job_id = %job.id,
        codec = target_codec,
        hw_accel = hw_accel_flags.unwrap_or("none"),
        "Starting FFmpeg transcode"
    );

    // TODO: Spawn ffmpeg process, read stderr for progress lines,
    // parse "time=HH:MM:SS.ms" to compute percentage against total duration,
    // and send progress updates via progress_tx.
    //
    // let mut child = tokio::process::Command::new("ffmpeg")
    //     .args(&args)
    //     .stderr(std::process::Stdio::piped())
    //     .spawn()?;
    //
    // parse_ffmpeg_progress(child.stderr.take().unwrap(), job, progress_tx).await?;
    // child.wait().await?;

    let _ = (args, progress_tx);
    todo!("Implement FFmpeg CLI transcoding with progress parsing")
}

/// Build the ffmpeg argument list for a transcode job.
fn build_ffmpeg_args(
    input: &str,
    output: &str,
    codec: &str,
    hw_accel: Option<&str>,
) -> Vec<String> {
    let mut args = vec!["-y".to_string(), "-hide_banner".to_string()];

    // Add hardware acceleration init flags
    if let Some(accel) = hw_accel {
        args.extend(["-hwaccel".to_string(), accel.to_string()]);
    }

    args.extend(["-i".to_string(), input.to_string()]);

    // Map codec name to ffmpeg encoder
    let encoder = match codec {
        "av1" => {
            if hw_accel == Some("nvenc") {
                "av1_nvenc"
            } else if hw_accel == Some("vaapi") {
                "av1_vaapi"
            } else if hw_accel == Some("qsv") {
                "av1_qsv"
            } else {
                "libsvtav1"
            }
        }
        "vp9" => "libvpx-vp9",
        "opus" => "libopus",
        other => other,
    };

    args.extend(["-c:v".to_string(), encoder.to_string()]);
    args.extend(["-c:a".to_string(), "libopus".to_string()]);
    args.push(output.to_string());

    args
}

/// Compute the output file path by replacing the extension.
fn compute_output_path(input: &str, target_container: &str) -> String {
    let stem = std::path::Path::new(input)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let parent = std::path::Path::new(input)
        .parent()
        .unwrap_or(std::path::Path::new("."));
    parent
        .join(format!("{stem}.chrysopeia.{target_container}"))
        .to_string_lossy()
        .into_owned()
}
