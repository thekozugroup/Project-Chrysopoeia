//! Media file probing using oximedia-container.
//!
//! Reads container metadata to extract codec, resolution, bitrate, and duration.

use std::path::Path;

use chrysopeia_core::models::{MediaFormat, Resolution};

/// Probe a media file and extract its format information.
///
/// Uses oximedia-container to read container metadata and extract
/// codec info, resolution, bitrate, and duration.
pub async fn probe_file(path: &Path) -> anyhow::Result<MediaFormat> {
    tracing::debug!("Probing file: {}", path.display());

    // TODO: Use oximedia_container to open the file and read metadata.
    // Placeholder implementation extracts container from extension.
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("unknown")
        .to_lowercase();

    let container = match extension.as_str() {
        "mkv" => "matroska",
        "mp4" | "m4a" => "mp4",
        "avi" => "avi",
        "mov" => "mov",
        "webm" => "webm",
        "ogg" | "oga" | "ogv" => "ogg",
        "flac" => "flac",
        "mp3" => "mp3",
        "wav" => "wav",
        "ts" => "mpegts",
        "wmv" => "wmv",
        "flv" => "flv",
        other => other,
    }
    .to_string();

    // TODO: Replace with actual oximedia-container probing:
    // let reader = oximedia_container::open(path)?;
    // let streams = reader.streams();
    // Extract video/audio codec, resolution, bitrate, duration from streams.

    Ok(MediaFormat {
        container,
        video_codec: None,  // TODO: extract from container probe
        audio_codec: None,  // TODO: extract from container probe
        resolution: None,   // TODO: extract from container probe
        video_bitrate: None,
        audio_bitrate: None,
        duration_secs: None,
    })
}

/// Probe a file and return its resolution if it has a video stream.
pub async fn probe_resolution(path: &Path) -> anyhow::Result<Option<Resolution>> {
    let format = probe_file(path).await?;
    Ok(format.resolution)
}
