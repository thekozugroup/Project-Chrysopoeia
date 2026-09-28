//! Transcode profiles: what a library should be converted into.
//!
//! Users pick a [`Goal`]; each goal expands to a full [`TranscodeProfile`].
//! Advanced users can edit individual fields, which turns the goal into
//! [`Goal::Custom`] in the UI.

use serde::{Deserialize, Serialize};

use crate::codec::{AudioCodec, Container, VideoCodec};

/// Plain-language presets shown in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Goal {
    /// AV1 video, Opus audio, MKV. Smallest files.
    SaveSpace,
    /// HEVC video, original audio, MKV. Fast with hardware, plays on most TVs.
    Balanced,
    /// H.264 video, AAC audio, MP4. Plays on everything.
    Compatible,
    /// AV1 at high quality, original audio. For keeping masters.
    Archive,
    /// Anything the user configured by hand.
    Custom,
}

/// Quality target, mapped per encoder to CRF / CQ / QP / global_quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityLevel {
    Smallest,
    Small,
    Balanced,
    High,
    Best,
}

impl QualityLevel {
    /// 0 (smallest) ..= 4 (best).
    pub fn step(self) -> u8 {
        match self {
            Self::Smallest => 0,
            Self::Small => 1,
            Self::Balanced => 2,
            Self::High => 3,
            Self::Best => 4,
        }
    }
}

/// Encoder effort. Slower presets give smaller files at the same quality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedPreset {
    Fast,
    Balanced,
    Thorough,
}

/// What to do with subtitle tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtitlePolicy {
    /// Keep every subtitle the target container can hold, converting text
    /// formats where needed.
    Keep,
    /// Remove all subtitles.
    Drop,
}

/// Full description of the output a library wants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscodeProfile {
    pub goal: Goal,
    pub video_codec: VideoCodec,
    pub audio_codec: AudioCodec,
    pub container: Container,
    pub quality: QualityLevel,
    pub speed: SpeedPreset,
    /// Advanced: raw encoder quality value (CRF, CQ, QP...). Overrides
    /// `quality` when set. Interpreted in the chosen encoder's own scale.
    #[serde(default)]
    pub quality_override: Option<u32>,
    /// Downscale video taller than this many lines (e.g. 1080). `None` keeps
    /// the source resolution.
    #[serde(default)]
    pub max_height: Option<u32>,
    pub subtitles: SubtitlePolicy,
    /// Keep only audio tracks in these ISO 639 languages (e.g. `["eng",
    /// "jpn"]`). Empty keeps all. Tracks without a language tag are always
    /// kept, and at least one audio track is always kept.
    #[serde(default)]
    pub audio_languages: Vec<String>,
    /// Same as `audio_languages`, for subtitles.
    #[serde(default)]
    pub subtitle_languages: Vec<String>,
    /// Skip files whose video is already at least as efficient as the target
    /// codec (e.g. AV1 files when the target is HEVC). When false, only files
    /// already in exactly the target format are skipped.
    pub skip_efficient: bool,
    /// Discard the result unless it is at least this many percent smaller than
    /// the original. `None` keeps the result whatever its size.
    #[serde(default)]
    pub min_savings_pct: Option<u8>,
}

impl TranscodeProfile {
    /// Expand a goal into its default profile. `Custom` returns the
    /// `SaveSpace` settings with the goal set to `Custom`.
    pub fn from_goal(goal: Goal) -> Self {
        let base = Self {
            goal,
            video_codec: VideoCodec::Av1,
            audio_codec: AudioCodec::Opus,
            container: Container::Mkv,
            quality: QualityLevel::Balanced,
            speed: SpeedPreset::Balanced,
            quality_override: None,
            max_height: None,
            subtitles: SubtitlePolicy::Keep,
            audio_languages: Vec::new(),
            subtitle_languages: Vec::new(),
            skip_efficient: true,
            min_savings_pct: Some(10),
        };
        match goal {
            Goal::SaveSpace | Goal::Custom => base,
            Goal::Balanced => Self {
                video_codec: VideoCodec::Hevc,
                audio_codec: AudioCodec::Copy,
                ..base
            },
            Goal::Compatible => Self {
                video_codec: VideoCodec::H264,
                audio_codec: AudioCodec::Aac,
                container: Container::Mp4,
                quality: QualityLevel::High,
                skip_efficient: false,
                min_savings_pct: None,
                ..base
            },
            Goal::Archive => Self {
                audio_codec: AudioCodec::Copy,
                quality: QualityLevel::Best,
                speed: SpeedPreset::Thorough,
                min_savings_pct: Some(5),
                ..base
            },
        }
    }

    /// Fix combinations the container cannot hold. Returns a list of
    /// human-readable adjustments that were made.
    pub fn normalize(&mut self) -> Vec<String> {
        let mut notes = Vec::new();
        if !self.container.supports_video(self.video_codec) {
            let old = self.container;
            self.container = Container::Mkv;
            notes.push(format!(
                "{} can't hold {} video, so the container was changed to MKV",
                old.label(),
                self.video_codec.label()
            ));
        }
        if !self.container.supports_audio(self.audio_codec) {
            let fallback = self.container.fallback_audio();
            notes.push(format!(
                "{} can't hold {} audio, so audio will be {}",
                self.container.label(),
                self.audio_codec.label(),
                fallback.label()
            ));
            self.audio_codec = fallback;
        }
        if let Some(p) = self.min_savings_pct {
            self.min_savings_pct = Some(p.min(90));
        }
        notes
    }
}

impl Default for TranscodeProfile {
    fn default() -> Self {
        Self::from_goal(Goal::SaveSpace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goals_are_valid_as_shipped() {
        for goal in [
            Goal::SaveSpace,
            Goal::Balanced,
            Goal::Compatible,
            Goal::Archive,
            Goal::Custom,
        ] {
            let mut p = TranscodeProfile::from_goal(goal);
            assert!(p.normalize().is_empty(), "{goal:?} needed normalizing");
        }
    }

    #[test]
    fn normalize_fixes_webm_hevc() {
        let mut p = TranscodeProfile {
            video_codec: VideoCodec::Hevc,
            container: Container::Webm,
            audio_codec: AudioCodec::Aac,
            ..TranscodeProfile::default()
        };
        let notes = p.normalize();
        assert_eq!(p.container, Container::Mkv);
        assert_eq!(p.audio_codec, AudioCodec::Aac);
        assert_eq!(notes.len(), 1);
    }

    #[test]
    fn json_roundtrip_with_missing_optional_fields() {
        let json = r#"{"goal":"balanced","video_codec":"hevc","audio_codec":"copy",
            "container":"mkv","quality":"balanced","speed":"fast","subtitles":"keep",
            "skip_efficient":true}"#;
        let p: TranscodeProfile = serde_json::from_str(json).unwrap();
        assert_eq!(p.min_savings_pct, None);
        assert!(p.audio_languages.is_empty());
    }
}
