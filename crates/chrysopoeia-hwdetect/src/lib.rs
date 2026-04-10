//! Hardware capability detection for Chrysopeia.
//!
//! Detects available GPUs, hardware encoders, and recommends
//! the best encoding path for the current system.

pub mod ffmpeg;
pub mod gpu;
pub mod recommend;

use chrysopoeia_core::models::HardwareCapability;

/// Error type for hardware detection failures.
#[derive(Debug, thiserror::Error)]
pub enum HwDetectError {
    #[error("GPU detection failed: {0}")]
    GpuDetection(String),

    #[error("FFmpeg detection failed: {0}")]
    FfmpegDetection(String),

    #[error("No suitable hardware found")]
    NoHardware,
}

/// Detect all available hardware encoding capabilities on this system.
///
/// Combines GPU detection via oximedia-accel with FFmpeg encoder probing
/// to build a complete picture of available hardware.
pub async fn detect_hardware() -> Result<Vec<HardwareCapability>, HwDetectError> {
    let mut capabilities = Vec::new();

    // Try GPU detection via oximedia-accel Vulkan enumeration
    match gpu::detect_gpus().await {
        Ok(gpu_caps) => capabilities.extend(gpu_caps),
        Err(e) => tracing::warn!("GPU detection failed, continuing with FFmpeg: {e}"),
    }

    // Try FFmpeg encoder detection as fallback / supplement
    match ffmpeg::detect_ffmpeg_encoders().await {
        Ok(ffmpeg_caps) => capabilities.extend(ffmpeg_caps),
        Err(e) => tracing::warn!("FFmpeg encoder detection failed: {e}"),
    }

    if capabilities.is_empty() {
        tracing::info!("No hardware acceleration detected; CPU-only encoding will be used");
    }

    Ok(capabilities)
}
