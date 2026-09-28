//! Skip decisions and ffmpeg argument construction. Pure functions only.

use std::path::Path;

use chrysopoeia_core::{EncoderCandidate, ProbeInfo, TranscodeProfile};

/// Whether a file needs work under a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Transcode,
    /// No work needed. `reason` is a short sentence for the UI, e.g.
    /// "Already AV1 — more efficient than the HEVC target".
    Skip { reason: String },
}

/// Decide whether `probe` needs transcoding under `profile`.
pub fn decide(probe: &ProbeInfo, profile: &TranscodeProfile) -> Decision {
    let _ = (probe, profile);
    todo!("implemented by the worker-plan agent")
}

/// Inputs for [`build_plan`].
#[derive(Debug, Clone, Copy)]
pub struct PlanRequest<'a> {
    pub input: &'a Path,
    /// Where ffmpeg writes (a temp path; the extension matches the container).
    pub output: &'a Path,
    pub probe: &'a ProbeInfo,
    pub profile: &'a TranscodeProfile,
    pub encoder: &'a EncoderCandidate,
}

/// Number of streams of each kind the output will contain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamSummary {
    pub video: u32,
    pub audio: u32,
    pub subtitle: u32,
}

/// A ready-to-run ffmpeg invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegPlan {
    /// Arguments after the `ffmpeg` binary. Always includes `-y`,
    /// `-progress pipe:1 -nostats`, explicit `-map`s and `-f <muxer>`.
    pub args: Vec<String>,
    /// Plain-language notes about compromises, e.g. "Removed 2 picture-based
    /// subtitles because MP4 can't hold them".
    pub notes: Vec<String>,
    /// What the output should contain; used by verification.
    pub expected: StreamSummary,
}

/// Build the ffmpeg command for one attempt.
pub fn build_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let _ = req;
    todo!("implemented by the worker-plan agent")
}
