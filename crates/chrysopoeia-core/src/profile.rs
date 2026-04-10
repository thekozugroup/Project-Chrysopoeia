//! Default transcode profiles for common encoding scenarios.

use serde::{Deserialize, Serialize};

/// A reusable transcoding profile with codec and quality settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscodeProfile {
    /// Human-readable profile name.
    pub name: String,
    /// Target video codec (e.g. "av1", "vp9").
    pub video_codec: String,
    /// Target audio codec (e.g. "opus", "flac").
    pub audio_codec: String,
    /// Quality value (CRF or equivalent; lower = better).
    pub quality: u32,
    /// Whether to prefer hardware acceleration.
    pub hw_accel_preference: bool,
    /// Encoder speed preset (codec-dependent).
    pub speed_preset: u32,
    /// Target container format.
    pub container: String,
}

/// Returns the set of built-in default transcode profiles.
pub fn default_profiles() -> Vec<TranscodeProfile> {
    vec![
        TranscodeProfile {
            name: "AV1 High Quality".to_string(),
            video_codec: "av1".to_string(),
            audio_codec: "opus".to_string(),
            quality: 22,
            hw_accel_preference: true,
            speed_preset: 4,
            container: "mkv".to_string(),
        },
        TranscodeProfile {
            name: "AV1 Fast".to_string(),
            video_codec: "av1".to_string(),
            audio_codec: "opus".to_string(),
            quality: 32,
            hw_accel_preference: true,
            speed_preset: 10,
            container: "mkv".to_string(),
        },
        TranscodeProfile {
            name: "VP9 Balanced".to_string(),
            video_codec: "vp9".to_string(),
            audio_codec: "opus".to_string(),
            quality: 28,
            hw_accel_preference: false,
            speed_preset: 4,
            container: "webm".to_string(),
        },
        TranscodeProfile {
            name: "Audio-only Opus".to_string(),
            video_codec: String::new(),
            audio_codec: "opus".to_string(),
            quality: 5,
            hw_accel_preference: false,
            speed_preset: 0,
            container: "ogg".to_string(),
        },
    ]
}
