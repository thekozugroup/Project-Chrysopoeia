//! Encoding strategy selection based on media file and hardware capabilities.

use chrysopoeia_core::codec;
use chrysopoeia_core::models::{HardwareCapability, HwAccelApi, MediaFile};

/// The selected encoding strategy for a given file.
#[derive(Debug, Clone)]
pub enum EncodingStrategy {
    /// File is already in an open format; no work needed.
    Skip,
    /// Use oximedia native encoding pipeline.
    OximediaNative { codec: String },
    /// Use FFmpeg CLI with optional hw accel flags.
    FfmpegCli {
        codec: String,
        hw_accel_flags: Option<String>,
    },
}

/// Select the best encoding strategy for a media file given available hardware.
///
/// Decision tree:
/// 1. Already open format -> Skip
/// 2. Vulkan AV1 available -> OximediaNative AV1
/// 3. GPU AV1 via FFmpeg -> FfmpegCli with hw accel
/// 4. Vulkan VP9 available -> OximediaNative VP9
/// 5. CPU AV1 via FFmpeg -> FfmpegCli (libsvtav1)
/// 6. Fallback -> FfmpegCli VP9 (libvpx-vp9)
pub fn select_strategy(
    media_file: &MediaFile,
    capabilities: &[HardwareCapability],
) -> EncodingStrategy {
    // Already open? Skip.
    if codec::is_open_format(&media_file.format) {
        return EncodingStrategy::Skip;
    }

    // Prefer oximedia native AV1 via Vulkan
    if capabilities
        .iter()
        .any(|c| c.api == HwAccelApi::Vulkan && c.supports_av1)
    {
        return EncodingStrategy::OximediaNative {
            codec: "av1".to_string(),
        };
    }

    // GPU AV1 via FFmpeg
    if let Some(cap) = capabilities
        .iter()
        .find(|c| c.supports_av1 && c.api != HwAccelApi::Vulkan)
    {
        let flags = match cap.api {
            HwAccelApi::Nvenc => "cuda",
            HwAccelApi::Vaapi => "vaapi",
            HwAccelApi::Qsv => "qsv",
            _ => "auto",
        };
        return EncodingStrategy::FfmpegCli {
            codec: "av1".to_string(),
            hw_accel_flags: Some(flags.to_string()),
        };
    }

    // oximedia native VP9 via Vulkan
    if capabilities
        .iter()
        .any(|c| c.api == HwAccelApi::Vulkan && c.supports_vp9)
    {
        return EncodingStrategy::OximediaNative {
            codec: "vp9".to_string(),
        };
    }

    // CPU AV1 via FFmpeg (libsvtav1)
    EncodingStrategy::FfmpegCli {
        codec: "av1".to_string(),
        hw_accel_flags: None,
    }
}
