//! FFmpeg CLI transcoding backend.
//!
//! Spawns ffmpeg as a child process, parses real-time progress from
//! `-progress pipe:1`, and supports cancellation.

use std::path::Path;
use std::process::Stdio;

use chrysopoeia_core::models::{MediaFile, TranscodeJob};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::progress::ProgressUpdate;

/// Result of a successful transcode.
#[derive(Debug)]
pub struct TranscodeResult {
    pub output_path: String,
    pub output_size: u64,
}

/// Transcode a media file using ffmpeg.
///
/// Spawns ffmpeg, reads progress from stdout (`-progress pipe:1`),
/// and sends updates via `progress_tx`. Respects `cancel` token.
pub async fn transcode(
    media_file: &MediaFile,
    job: &TranscodeJob,
    video_encoder: &str,
    audio_encoder: &str,
    crf: u32,
    hw_accel: Option<&str>,
    progress_tx: mpsc::Sender<ProgressUpdate>,
    cancel: CancellationToken,
) -> anyhow::Result<TranscodeResult> {
    let tmp_output = compute_tmp_path(&media_file.path, &job.target_container);
    let final_output = compute_output_path(&media_file.path, &job.target_container);

    let duration_secs = media_file.format.duration_secs.unwrap_or(0.0);
    let args = build_args(
        &media_file.path,
        &tmp_output,
        video_encoder,
        audio_encoder,
        crf,
        hw_accel,
    );

    tracing::info!(
        job_id = %job.id,
        video_encoder,
        audio_encoder,
        crf,
        hw_accel = hw_accel.unwrap_or("none"),
        "Starting FFmpeg transcode"
    );

    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped()) // -progress pipe:1 writes here
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("Failed to spawn ffmpeg: {e}"))?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");

    // Read progress from stdout in a background task
    let job_id = job.id;
    let ptx = progress_tx.clone();
    let progress_task = tokio::spawn(async move {
        let reader = BufReader::new(stdout);
        let mut lines = reader.lines();
        let mut current_time_us: u64 = 0;
        let mut current_speed: Option<String> = None;
        let mut current_fps: Option<f64> = None;

        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(val) = line.strip_prefix("out_time_us=") {
                current_time_us = val.trim().parse().unwrap_or(0);
            } else if let Some(val) = line.strip_prefix("speed=") {
                let s = val.trim().trim_end_matches('x');
                if let Ok(_spd) = s.parse::<f64>() {
                    current_speed = Some(val.trim().to_string());
                }
            } else if let Some(val) = line.strip_prefix("fps=") {
                current_fps = val.trim().parse().ok();
            } else if line.starts_with("progress=") {
                // End of a progress block — emit update
                let elapsed_secs = current_time_us as f64 / 1_000_000.0;
                let pct = if duration_secs > 0.0 {
                    ((elapsed_secs / duration_secs) * 100.0).min(100.0) as u8
                } else {
                    0
                };

                let eta = if pct > 0 && duration_secs > 0.0 {
                    let speed_val = current_speed
                        .as_ref()
                        .and_then(|s| s.trim_end_matches('x').parse::<f64>().ok())
                        .unwrap_or(1.0);
                    if speed_val > 0.0 {
                        let remaining_secs = duration_secs - elapsed_secs;
                        Some(remaining_secs / speed_val)
                    } else {
                        None
                    }
                } else {
                    None
                };

                let _ = ptx
                    .send(ProgressUpdate {
                        job_id,
                        percent: pct,
                        speed: current_speed.clone(),
                        fps: current_fps,
                        eta_secs: eta,
                    })
                    .await;
            }
        }
    });

    // Capture stderr for error messages
    let stderr_task = tokio::spawn(async move {
        let reader = BufReader::new(stderr);
        let mut lines = reader.lines();
        let mut output = String::new();
        while let Ok(Some(line)) = lines.next_line().await {
            output.push_str(&line);
            output.push('\n');
        }
        output
    });

    // Wait for completion or cancellation
    let status = tokio::select! {
        result = child.wait() => result?,
        _ = cancel.cancelled() => {
            tracing::info!(job_id = %job_id, "Transcode cancelled, killing ffmpeg");
            child.kill().await.ok();
            // Clean up temp file
            tokio::fs::remove_file(&tmp_output).await.ok();
            anyhow::bail!("Transcode cancelled");
        }
    };

    progress_task.abort();
    let stderr_output = stderr_task.await.unwrap_or_default();

    if !status.success() {
        tokio::fs::remove_file(&tmp_output).await.ok();
        let code = status.code().unwrap_or(-1);
        // Take last 500 chars of stderr for the error message
        let err_tail: String = stderr_output.chars().rev().take(500).collect::<String>().chars().rev().collect();
        anyhow::bail!("ffmpeg exited with code {code}: {err_tail}");
    }

    // Rename temp to final
    tokio::fs::rename(&tmp_output, &final_output).await
        .map_err(|e| anyhow::anyhow!("Failed to rename output: {e}"))?;

    let output_size = tokio::fs::metadata(&final_output).await?.len();

    // Send 100% completion
    let _ = progress_tx
        .send(ProgressUpdate {
            job_id,
            percent: 100,
            speed: None,
            fps: None,
            eta_secs: Some(0.0),
        })
        .await;

    tracing::info!(
        job_id = %job_id,
        output_path = %final_output,
        output_size,
        "Transcode complete"
    );

    Ok(TranscodeResult {
        output_path: final_output,
        output_size,
    })
}

/// Build ffmpeg arguments.
fn build_args(
    input: &str,
    output: &str,
    video_encoder: &str,
    audio_encoder: &str,
    crf: u32,
    hw_accel: Option<&str>,
) -> Vec<String> {
    let mut args = vec![
        "-y".to_string(),
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
    ];

    // Hardware acceleration init
    if let Some(accel) = hw_accel {
        match accel {
            "cuda" | "nvenc" => {
                args.extend(["-hwaccel".to_string(), "cuda".to_string()]);
                args.extend(["-hwaccel_output_format".to_string(), "cuda".to_string()]);
            }
            "vaapi" => {
                args.extend(["-hwaccel".to_string(), "vaapi".to_string()]);
                args.extend([
                    "-hwaccel_device".to_string(),
                    "/dev/dri/renderD128".to_string(),
                ]);
            }
            "qsv" => {
                args.extend(["-hwaccel".to_string(), "qsv".to_string()]);
            }
            "videotoolbox" => {
                args.extend(["-hwaccel".to_string(), "videotoolbox".to_string()]);
            }
            _ => {}
        }
    }

    // Input
    args.extend(["-i".to_string(), input.to_string()]);

    // Video encoding
    args.extend(["-c:v".to_string(), video_encoder.to_string()]);

    // CRF / quality (not all encoders use -crf)
    match video_encoder {
        "libsvtav1" => {
            args.extend(["-crf".to_string(), crf.to_string()]);
            args.extend(["-preset".to_string(), "6".to_string()]); // balanced speed/quality
        }
        "libvpx-vp9" => {
            args.extend(["-crf".to_string(), crf.to_string()]);
            args.extend(["-b:v".to_string(), "0".to_string()]); // CRF mode for VP9
        }
        "libx265" => {
            args.extend(["-crf".to_string(), crf.to_string()]);
            args.extend(["-preset".to_string(), "medium".to_string()]);
        }
        "libx264" => {
            args.extend(["-crf".to_string(), crf.to_string()]);
            args.extend(["-preset".to_string(), "medium".to_string()]);
        }
        // HW encoders use -qp or -cq instead of -crf
        "av1_nvenc" | "hevc_nvenc" | "h264_nvenc" => {
            args.extend(["-cq".to_string(), crf.to_string()]);
            args.extend(["-preset".to_string(), "p5".to_string()]);
        }
        _ => {
            args.extend(["-crf".to_string(), crf.to_string()]);
        }
    }

    // Audio encoding
    if audio_encoder == "copy" {
        args.extend(["-c:a".to_string(), "copy".to_string()]);
    } else {
        args.extend(["-c:a".to_string(), audio_encoder.to_string()]);
        match audio_encoder {
            "libopus" => {
                args.extend(["-b:a".to_string(), "128k".to_string()]);
            }
            "aac" => {
                args.extend(["-b:a".to_string(), "128k".to_string()]);
            }
            _ => {}
        }
    }

    // Machine-readable progress to stdout
    args.extend(["-progress".to_string(), "pipe:1".to_string()]);

    // Output
    args.push(output.to_string());

    args
}

/// Compute temp output path (used during transcode, renamed on success).
fn compute_tmp_path(input: &str, target_container: &str) -> String {
    let p = Path::new(input);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let parent = p.parent().unwrap_or(Path::new("."));
    parent
        .join(format!("{stem}.chrysopoeia.tmp.{target_container}"))
        .to_string_lossy()
        .into_owned()
}

/// Compute final output path.
fn compute_output_path(input: &str, target_container: &str) -> String {
    let p = Path::new(input);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("output");
    let parent = p.parent().unwrap_or(Path::new("."));
    parent
        .join(format!("{stem}.{target_container}"))
        .to_string_lossy()
        .into_owned()
}

/// Map a codec name to the ffmpeg encoder name.
pub fn codec_to_encoder(codec: &str, hw_accel: Option<&str>) -> &'static str {
    match (codec, hw_accel) {
        ("av1", Some("nvenc" | "cuda")) => "av1_nvenc",
        ("av1", Some("vaapi")) => "av1_vaapi",
        ("av1", Some("qsv")) => "av1_qsv",
        ("av1", _) => "libsvtav1",
        ("vp9", _) => "libvpx-vp9",
        ("hevc", Some("nvenc" | "cuda")) => "hevc_nvenc",
        ("hevc", Some("vaapi")) => "hevc_vaapi",
        ("hevc", Some("qsv")) => "hevc_qsv",
        ("hevc", Some("videotoolbox")) => "hevc_videotoolbox",
        ("hevc", _) => "libx265",
        ("h264", Some("nvenc" | "cuda")) => "h264_nvenc",
        ("h264", Some("vaapi")) => "h264_vaapi",
        ("h264", Some("qsv")) => "h264_qsv",
        ("h264", Some("videotoolbox")) => "h264_videotoolbox",
        ("h264", _) => "libx264",
        ("opus", _) => "libopus",
        ("flac", _) => "flac",
        ("aac", _) => "aac",
        ("copy", _) => "copy",
        (other, _) => {
            tracing::warn!("Unknown codec {other}, passing through");
            // Return a static str — we can't return `other` due to lifetime
            "copy"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_args_software_av1() {
        let args = build_args("/input.mkv", "/output.mkv", "libsvtav1", "libopus", 28, None);
        assert!(args.contains(&"-c:v".to_string()));
        assert!(args.contains(&"libsvtav1".to_string()));
        assert!(args.contains(&"-crf".to_string()));
        assert!(args.contains(&"28".to_string()));
        assert!(args.contains(&"libopus".to_string()));
        assert!(args.contains(&"-progress".to_string()));
    }

    #[test]
    fn test_build_args_nvenc() {
        let args = build_args("/in.mkv", "/out.mkv", "av1_nvenc", "copy", 28, Some("cuda"));
        assert!(args.contains(&"-hwaccel".to_string()));
        assert!(args.contains(&"cuda".to_string()));
        assert!(args.contains(&"av1_nvenc".to_string()));
        assert!(args.contains(&"copy".to_string()));
    }

    #[test]
    fn test_codec_to_encoder() {
        assert_eq!(codec_to_encoder("av1", None), "libsvtav1");
        assert_eq!(codec_to_encoder("av1", Some("nvenc")), "av1_nvenc");
        assert_eq!(codec_to_encoder("hevc", Some("vaapi")), "hevc_vaapi");
        assert_eq!(codec_to_encoder("opus", None), "libopus");
        assert_eq!(codec_to_encoder("copy", None), "copy");
    }

    #[test]
    fn test_output_paths() {
        assert_eq!(
            compute_output_path("/mnt/media/movie.mkv", "mkv"),
            "/mnt/media/movie.mkv"
        );
        assert_eq!(
            compute_tmp_path("/mnt/media/movie.mkv", "mkv"),
            "/mnt/media/movie.chrysopoeia.tmp.mkv"
        );
    }
}
