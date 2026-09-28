//! `/api/presets`: goals, codecs and containers the UI offers.

use axum::Json;
use axum::extract::State;
use chrysopoeia_core::{AudioCodec, Container, Goal, HardwareInfo, TranscodeProfile, VideoCodec};
use serde::Serialize;

use crate::state::AppState;

/// One goal card.
#[derive(Debug, Serialize)]
pub struct GoalPreset {
    pub goal: Goal,
    pub title: &'static str,
    pub summary: &'static str,
    pub profile: TranscodeProfile,
}

/// A target video codec and whether this machine accelerates it.
#[derive(Debug, Serialize)]
pub struct VideoCodecPreset {
    pub codec: VideoCodec,
    pub label: &'static str,
    pub royalty_free: bool,
    pub hw_accelerated: bool,
    /// Verified encoder names.
    pub encoders: Vec<String>,
}

/// A target audio codec.
#[derive(Debug, Serialize)]
pub struct AudioCodecPreset {
    pub codec: AudioCodec,
    pub label: &'static str,
}

/// A container and what it can hold.
#[derive(Debug, Serialize)]
pub struct ContainerPreset {
    pub container: Container,
    pub label: &'static str,
    pub video: Vec<VideoCodec>,
    pub audio: Vec<AudioCodec>,
}

/// Response of `GET /api/presets`.
#[derive(Debug, Serialize)]
pub struct Presets {
    pub goals: Vec<GoalPreset>,
    pub video_codecs: Vec<VideoCodecPreset>,
    pub audio_codecs: Vec<AudioCodecPreset>,
    pub containers: Vec<ContainerPreset>,
}

/// Title and a plain one-line outcome for a goal: what the files become,
/// without codec names or speed claims (the UI has its own copy for those
/// details; this stays for compatibility).
pub fn goal_text(goal: Goal) -> (&'static str, &'static str) {
    match goal {
        Goal::SaveSpace => ("Save space", "The smallest files, for everyday watching."),
        Goal::Balanced => (
            "Balanced",
            "Much smaller files that still play on most TVs and players.",
        ),
        Goal::Compatible => (
            "Plays everywhere",
            "Files that play on every device and in every browser.",
        ),
        Goal::Archive => ("Archive", "Near-original picture quality, in less space."),
        Goal::Custom => ("Custom", "Your own combination of settings."),
    }
}

/// Video codecs this machine can encode: those with a verified encoder (or
/// a listed CPU encoder, when detection itself failed and nothing could be
/// verified). Every codec while hardware detection hasn't finished yet.
pub fn usable_video_codecs(hw: Option<&HardwareInfo>) -> Vec<VideoCodec> {
    VideoCodec::ALL
        .into_iter()
        .filter(|c| {
            hw.is_none_or(|h| {
                h.encoders
                    .iter()
                    .any(|e| e.codec == *c && (e.verified || (e.available && !e.api.is_hardware())))
            })
        })
        .collect()
}

/// Audio codecs this machine can write: copy, and those whose ffmpeg
/// encoder is available. Every codec while detection hasn't finished yet.
pub fn usable_audio_codecs(hw: Option<&HardwareInfo>) -> Vec<AudioCodec> {
    AudioCodec::ALL
        .into_iter()
        .filter(|a| {
            *a == AudioCodec::Copy
                || hw.is_none_or(|h| h.audio_encoders.iter().any(|e| e == a.ffmpeg_encoder()))
        })
        .collect()
}

/// `GET /api/presets`. Only codecs this machine can actually write are
/// offered (hardware detection lists every known encoder, including ones
/// missing from this ffmpeg build).
pub async fn get(State(state): State<AppState>) -> Json<Presets> {
    let hw = state.hardware.current();
    let video_ok = usable_video_codecs(hw.as_deref());
    let audio_ok = usable_audio_codecs(hw.as_deref());
    let goals = [
        Goal::SaveSpace,
        Goal::Balanced,
        Goal::Compatible,
        Goal::Archive,
    ]
    .into_iter()
    .map(|goal| {
        let (title, summary) = goal_text(goal);
        GoalPreset {
            goal,
            title,
            summary,
            profile: TranscodeProfile::from_goal(goal),
        }
    })
    .collect();
    let video_codecs = video_ok
        .iter()
        .copied()
        .map(|codec| VideoCodecPreset {
            codec,
            label: codec.label(),
            royalty_free: codec.royalty_free(),
            hw_accelerated: hw.as_ref().is_some_and(|h| h.has_hw_encoder(codec)),
            encoders: hw
                .as_ref()
                .map(|h| h.verified_encoders(codec).map(|e| e.name.clone()).collect())
                .unwrap_or_default(),
        })
        .collect();
    let audio_codecs = audio_ok
        .iter()
        .copied()
        .map(|codec| AudioCodecPreset {
            codec,
            label: codec.label(),
        })
        .collect();
    let containers = Container::ALL
        .into_iter()
        .map(|container| ContainerPreset {
            container,
            label: container.label(),
            video: video_ok
                .iter()
                .copied()
                .filter(|v| container.supports_video(*v))
                .collect(),
            audio: audio_ok
                .iter()
                .copied()
                .filter(|a| container.supports_audio(*a))
                .collect(),
        })
        .collect();
    Json(Presets {
        goals,
        video_codecs,
        audio_codecs,
        containers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_summaries_are_plain_outcomes() {
        for goal in [
            Goal::SaveSpace,
            Goal::Balanced,
            Goal::Compatible,
            Goal::Archive,
            Goal::Custom,
        ] {
            let (title, summary) = goal_text(goal);
            assert!(!title.is_empty());
            assert!(summary.ends_with('.'), "{summary}");
            assert_eq!(summary.matches(". ").count(), 0, "one line: {summary}");
            for jargon in [
                "AV1", "HEVC", "H.264", "H.265", "AAC", "Opus", "MP4", "MKV", "GPU", "CPU", "slow",
                "fast", "quick",
            ] {
                assert!(
                    !summary.to_lowercase().contains(&jargon.to_lowercase()),
                    "{goal:?}: {summary}"
                );
            }
        }
    }
}
