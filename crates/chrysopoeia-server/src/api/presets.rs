//! `/api/presets`: goals, codecs and containers the UI offers.

use axum::Json;
use axum::extract::State;
use chrysopoeia_core::{AudioCodec, Container, Goal, TranscodeProfile, VideoCodec};
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

/// Title and one-line trade-off for a goal.
pub fn goal_text(goal: Goal) -> (&'static str, &'static str) {
    match goal {
        Goal::SaveSpace => (
            "Save space",
            "Smallest files. AV1 video with Opus audio; slower to convert without a recent GPU.",
        ),
        Goal::Balanced => (
            "Balanced",
            "Much smaller files that play on most TVs. HEVC video, original audio kept.",
        ),
        Goal::Compatible => (
            "Plays everywhere",
            "Plays on every device and browser. H.264 with AAC audio in MP4; files may not shrink much.",
        ),
        Goal::Archive => (
            "Archive",
            "Near-original quality in less space. High-quality AV1, original audio kept.",
        ),
        Goal::Custom => ("Custom", "Your own combination of settings."),
    }
}

/// `GET /api/presets`
pub async fn get(State(state): State<AppState>) -> Json<Presets> {
    let hw = state.hardware.current();
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
    let video_codecs = VideoCodec::ALL
        .into_iter()
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
    let audio_codecs = AudioCodec::ALL
        .into_iter()
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
            video: VideoCodec::ALL
                .into_iter()
                .filter(|v| container.supports_video(*v))
                .collect(),
            audio: AudioCodec::ALL
                .into_iter()
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
