//! Detected hardware and the encoders that actually work on it.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::codec::VideoCodec;
use crate::encoder::HwApi;

/// User preference for which acceleration to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum HwPreference {
    /// Use the best verified hardware encoder, else the CPU.
    #[default]
    Auto,
    /// CPU only.
    Cpu,
    Nvenc,
    Qsv,
    Vaapi,
    VideoToolbox,
    Amf,
    Rkmpp,
    V4l2m2m,
}

impl HwPreference {
    /// The specific API this preference pins, if any.
    pub fn api(self) -> Option<HwApi> {
        match self {
            Self::Auto => None,
            Self::Cpu => Some(HwApi::Software),
            Self::Nvenc => Some(HwApi::Nvenc),
            Self::Qsv => Some(HwApi::Qsv),
            Self::Vaapi => Some(HwApi::Vaapi),
            Self::VideoToolbox => Some(HwApi::VideoToolbox),
            Self::Amf => Some(HwApi::Amf),
            Self::Rkmpp => Some(HwApi::Rkmpp),
            Self::V4l2m2m => Some(HwApi::V4l2m2m),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GpuVendor {
    Nvidia,
    Intel,
    Amd,
    Apple,
    Other,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuInfo {
    pub model: String,
    /// Logical CPUs visible to this process.
    pub logical_cores: u32,
    pub physical_cores: Option<u32>,
    /// CPU quota from the container runtime (cgroup `cpu.max`), in cores.
    pub cgroup_limit: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MemoryInfo {
    pub total_bytes: u64,
    pub available_bytes: u64,
    /// Memory limit from the container runtime (cgroup `memory.max`).
    pub cgroup_limit_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GpuDevice {
    pub vendor: GpuVendor,
    /// Marketing name when known (e.g. "NVIDIA GeForce RTX 3060").
    pub name: String,
    /// DRM render node, e.g. `/dev/dri/renderD128`. NVIDIA GPUs have none.
    pub render_node: Option<String>,
    /// Kernel driver (e.g. `i915`, `xe`, `amdgpu`, `nvidia`).
    pub driver: Option<String>,
}

/// One encoder and whether it works here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncoderStatus {
    /// ffmpeg encoder name.
    pub name: String,
    pub codec: VideoCodec,
    pub api: HwApi,
    /// Listed by `ffmpeg -encoders`.
    pub available: bool,
    /// A short test encode succeeded.
    pub verified: bool,
    /// Device used for the test (render node), if any.
    pub device: Option<String>,
    /// Why verification failed, in plain language plus the ffmpeg error tail.
    pub error: Option<String>,
}

/// An encoder the worker may try, in order, for a job. Produced by
/// `chrysopoeia_hwdetect::encoder_candidates`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderCandidate {
    /// ffmpeg encoder name.
    pub name: String,
    pub codec: VideoCodec,
    pub api: HwApi,
    /// Render node for VA-API / QSV.
    pub device: Option<String>,
    /// Decode on the GPU too (fastest). The worker retries the same encoder
    /// with CPU decoding if this fails.
    pub hw_decode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfmpegInfo {
    pub ffmpeg_path: String,
    pub ffprobe_path: String,
    pub found: bool,
    pub ffprobe_found: bool,
    /// First line of `ffmpeg -version`.
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecommendation {
    /// Suggested concurrent jobs when encoding on the CPU.
    pub cpu_jobs: u32,
    /// Suggested concurrent jobs when encoding on the GPU (0 if none).
    pub gpu_jobs: u32,
    /// What "automatic" resolves to with the current hardware preference.
    pub total: u32,
    /// One sentence explaining the numbers.
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SetupHintLevel {
    Info,
    Warning,
    Error,
}

/// A problem or tip about the hardware setup, written for non-experts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupHint {
    pub level: SetupHintLevel,
    pub title: String,
    pub detail: String,
    /// Copy-pasteable fix, e.g. a Docker flag.
    pub fix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HardwareInfo {
    pub cpu: CpuInfo,
    pub memory: MemoryInfo,
    pub gpus: Vec<GpuDevice>,
    pub encoders: Vec<EncoderStatus>,
    pub ffmpeg: FfmpegInfo,
    pub recommended_jobs: JobRecommendation,
    pub hints: Vec<SetupHint>,
    /// True when running inside a container.
    pub in_container: bool,
    pub detected_at: DateTime<Utc>,
}

impl HardwareInfo {
    /// Verified encoders for a codec.
    pub fn verified_encoders(&self, codec: VideoCodec) -> impl Iterator<Item = &EncoderStatus> {
        self.encoders
            .iter()
            .filter(move |e| e.codec == codec && e.verified)
    }

    /// Whether any verified hardware encoder exists for a codec.
    pub fn has_hw_encoder(&self, codec: VideoCodec) -> bool {
        self.verified_encoders(codec).any(|e| e.api.is_hardware())
    }
}
