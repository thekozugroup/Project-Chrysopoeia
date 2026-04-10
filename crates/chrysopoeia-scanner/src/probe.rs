//! Media file probing using ffprobe.
//!
//! Shells out to `ffprobe` to extract codec, resolution, bitrate, and duration.

use std::path::Path;
use std::process::Command;

use chrysopoeia_core::models::{MediaFormat, Resolution};

/// Probe a media file using `ffprobe -print_format json`.
pub async fn probe_file(path: &Path) -> anyhow::Result<MediaFormat> {
    tracing::debug!("Probing file: {}", path.display());

    let output = Command::new("ffprobe")
        .args([
            "-v", "quiet",
            "-print_format", "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .map_err(|e| anyhow::anyhow!("ffprobe not found or failed to execute: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("ffprobe failed for {}: {stderr}", path.display());
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| anyhow::anyhow!("Failed to parse ffprobe JSON: {e}"))?;

    // Extract container format
    let container = json["format"]["format_name"]
        .as_str()
        .unwrap_or("unknown")
        .split(',')
        .next()
        .unwrap_or("unknown")
        .to_string();

    let streams = json["streams"].as_array();

    // Find video stream
    let video_stream = streams.and_then(|s| {
        s.iter().find(|s| s["codec_type"].as_str() == Some("video"))
    });

    // Find audio stream
    let audio_stream = streams.and_then(|s| {
        s.iter().find(|s| s["codec_type"].as_str() == Some("audio"))
    });

    let video_codec = video_stream.and_then(|s| s["codec_name"].as_str()).map(String::from);
    let audio_codec = audio_stream.and_then(|s| s["codec_name"].as_str()).map(String::from);

    let resolution = video_stream.and_then(|s| {
        let w = s["width"].as_u64()? as u32;
        let h = s["height"].as_u64()? as u32;
        Some(Resolution { width: w, height: h })
    });

    let video_bitrate = video_stream
        .and_then(|s| s["bit_rate"].as_str())
        .and_then(|b| b.parse::<u64>().ok());

    let audio_bitrate = audio_stream
        .and_then(|s| s["bit_rate"].as_str())
        .and_then(|b| b.parse::<u64>().ok());

    let duration_secs = json["format"]["duration"]
        .as_str()
        .and_then(|d| d.parse::<f64>().ok());

    Ok(MediaFormat {
        container,
        video_codec,
        audio_codec,
        resolution,
        video_bitrate,
        audio_bitrate,
        duration_secs,
    })
}

/// Probe a file and return its resolution if it has a video stream.
pub async fn probe_resolution(path: &Path) -> anyhow::Result<Option<Resolution>> {
    let format = probe_file(path).await?;
    Ok(format.resolution)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ffprobe_json() {
        // This test verifies our JSON parsing logic without needing ffprobe
        let json: serde_json::Value = serde_json::json!({
            "format": {
                "format_name": "matroska,webm",
                "duration": "9924.123",
                "bit_rate": "46900000"
            },
            "streams": [
                {
                    "codec_type": "video",
                    "codec_name": "h264",
                    "width": 3840,
                    "height": 2160,
                    "bit_rate": "45000000"
                },
                {
                    "codec_type": "audio",
                    "codec_name": "ac3",
                    "bit_rate": "640000"
                }
            ]
        });

        let container = json["format"]["format_name"]
            .as_str()
            .unwrap()
            .split(',')
            .next()
            .unwrap();
        assert_eq!(container, "matroska");

        let streams = json["streams"].as_array().unwrap();
        let video = streams.iter().find(|s| s["codec_type"] == "video").unwrap();
        assert_eq!(video["codec_name"].as_str().unwrap(), "h264");
        assert_eq!(video["width"].as_u64().unwrap(), 3840);
    }
}
