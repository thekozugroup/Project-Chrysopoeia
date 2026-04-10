//! Codec detection helpers for determining open-format compliance.
//!
//! An "open format" file uses royalty-free codecs in open containers:
//! Video: AV1, VP9, VP8, Theora
//! Audio: Opus, Vorbis, FLAC
//! Containers: WebM, MKV, OGG

use crate::models::MediaFormat;

/// Set of video codecs considered "open" (royalty-free).
const OPEN_VIDEO_CODECS: &[&str] = &["av1", "vp9", "vp8", "theora"];

/// Set of audio codecs considered "open" (royalty-free).
const OPEN_AUDIO_CODECS: &[&str] = &["opus", "vorbis", "flac"];

/// Containers that support open codecs natively.
const OPEN_CONTAINERS: &[&str] = &["webm", "mkv", "matroska", "ogg", "oga", "ogv"];

/// Returns `true` if the video codec (if present) is an open format.
pub fn is_open_video_codec(codec: &str) -> bool {
    OPEN_VIDEO_CODECS
        .iter()
        .any(|c| codec.to_lowercase().contains(c))
}

/// Returns `true` if the audio codec (if present) is an open format.
pub fn is_open_audio_codec(codec: &str) -> bool {
    OPEN_AUDIO_CODECS
        .iter()
        .any(|c| codec.to_lowercase().contains(c))
}

/// Returns `true` if the container is an open format.
pub fn is_open_container(container: &str) -> bool {
    OPEN_CONTAINERS
        .iter()
        .any(|c| container.to_lowercase().contains(c))
}

/// Determine whether a media file is already in a fully open format.
///
/// A file is considered "open" when:
/// - Its container is open (WebM, MKV, OGG), AND
/// - Its video codec (if any) is open (AV1, VP9, VP8, Theora), AND
/// - Its audio codec (if any) is open (Opus, Vorbis, FLAC).
pub fn is_open_format(format: &MediaFormat) -> bool {
    if !is_open_container(&format.container) {
        return false;
    }

    if let Some(ref vc) = format.video_codec {
        if !is_open_video_codec(vc) {
            return false;
        }
    }

    if let Some(ref ac) = format.audio_codec {
        if !is_open_audio_codec(ac) {
            return false;
        }
    }

    true
}

/// Returns `true` if this file needs transcoding to an open format.
pub fn needs_transcode(format: &MediaFormat) -> bool {
    !is_open_format(format)
}
