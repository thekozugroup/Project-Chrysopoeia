//! Encoding path recommendation based on detected hardware.

use chrysopoeia_core::models::HardwareCapability;

/// Recommended encoding strategy in priority order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingPath {
    /// Native oximedia AV1 encoding (best quality + performance).
    OximediaAv1,
    /// GPU-accelerated AV1 via FFmpeg (NVENC/VAAPI/QSV).
    GpuAv1Ffmpeg,
    /// Native oximedia VP9 encoding.
    OximediaVp9,
    /// CPU-based AV1 encoding via FFmpeg (slow but universal).
    CpuAv1Ffmpeg,
    /// CPU-based VP9 encoding (fallback).
    CpuVp9,
}

/// Given detected hardware capabilities, recommend the best encoding path.
///
/// Priority order:
/// 1. oximedia native AV1 (requires Vulkan AV1 support)
/// 2. GPU AV1 via FFmpeg
/// 3. oximedia native VP9
/// 4. CPU AV1 via FFmpeg
/// 5. CPU VP9
pub fn recommend_encoding_path(capabilities: &[HardwareCapability]) -> EncodingPath {
    // Check for Vulkan devices with AV1 support (oximedia native)
    let has_vulkan_av1 = capabilities.iter().any(|c| {
        c.api == chrysopoeia_core::models::HwAccelApi::Vulkan && c.supports_av1
    });
    if has_vulkan_av1 {
        return EncodingPath::OximediaAv1;
    }

    // Check for GPU AV1 via FFmpeg (NVENC, VAAPI, QSV)
    let has_gpu_av1 = capabilities.iter().any(|c| {
        c.supports_av1
            && matches!(
                c.api,
                chrysopoeia_core::models::HwAccelApi::Nvenc
                    | chrysopoeia_core::models::HwAccelApi::Vaapi
                    | chrysopoeia_core::models::HwAccelApi::Qsv
            )
    });
    if has_gpu_av1 {
        return EncodingPath::GpuAv1Ffmpeg;
    }

    // Check for Vulkan VP9 support (oximedia native)
    let has_vulkan_vp9 = capabilities.iter().any(|c| {
        c.api == chrysopoeia_core::models::HwAccelApi::Vulkan && c.supports_vp9
    });
    if has_vulkan_vp9 {
        return EncodingPath::OximediaVp9;
    }

    // Fallback: CPU AV1 is preferred over CPU VP9 for quality/size
    EncodingPath::CpuAv1Ffmpeg
}
