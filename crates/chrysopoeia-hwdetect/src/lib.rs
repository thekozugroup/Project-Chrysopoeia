//! Hardware and encoder detection.
//!
//! Finds CPUs, memory limits and GPUs, checks which ffmpeg encoders are
//! compiled in, and runs a tiny test encode with each hardware encoder so the
//! UI only offers what actually works inside this container.
//!
//! Public API (fixed; see docs/ARCHITECTURE.md):
//! - [`detect`] — full detection, never fails (problems become `hints`).
//! - [`recommend_jobs`] — concurrent job count for the hardware.
//! - [`encoder_candidates`] — ordered encoders to try for a job.

use std::path::PathBuf;

use chrysopoeia_core::{
    EncoderCandidate, HardwareInfo, HwPreference, JobRecommendation, VideoCodec,
};

pub mod devices;
pub mod encoders;
pub mod hints;
pub mod recommend;

/// Inputs for [`detect`].
#[derive(Debug, Clone)]
pub struct DetectOptions {
    pub ffmpeg: PathBuf,
    pub ffprobe: PathBuf,
    /// Run a short test encode per hardware encoder (recommended). When
    /// false, encoders listed by ffmpeg are reported as unverified.
    pub verify_encoders: bool,
    /// Root for `/proc`, `/sys` and `/dev` lookups. `/` in production; tests
    /// point it at a fake tree.
    pub system_root: PathBuf,
    /// Preference used to compute `recommended_jobs.total`.
    pub preference: HwPreference,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
            verify_encoders: true,
            system_root: PathBuf::from("/"),
            preference: HwPreference::Auto,
        }
    }
}

/// Detect everything. Never returns an error: missing ffmpeg, missing GPUs and
/// failing encoders are reported through `HardwareInfo::hints`.
pub async fn detect(opts: &DetectOptions) -> HardwareInfo {
    let _ = opts;
    todo!("implemented by the hwdetect agent")
}

/// Recommend concurrent jobs for this hardware and preference.
pub fn recommend_jobs(hw: &HardwareInfo, preference: HwPreference) -> JobRecommendation {
    let _ = (hw, preference);
    todo!("implemented by the hwdetect agent")
}

/// Ordered list of encoders to try for `codec`.
///
/// Verified hardware encoders matching `preference` come first (each with
/// `hw_decode: true`; the worker retries the same encoder with CPU decoding
/// before moving on). The software encoder is appended when `preference` is
/// `Cpu`, when there is no usable hardware encoder, or when `cpu_fallback` is
/// true. Never returns an empty list if a software encoder is available.
pub fn encoder_candidates(
    hw: &HardwareInfo,
    codec: VideoCodec,
    preference: HwPreference,
    cpu_fallback: bool,
) -> Vec<EncoderCandidate> {
    let _ = (hw, codec, preference, cpu_fallback);
    todo!("implemented by the hwdetect agent")
}
