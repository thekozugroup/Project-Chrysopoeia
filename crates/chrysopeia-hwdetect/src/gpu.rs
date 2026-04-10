//! GPU detection via oximedia-accel Vulkan enumeration.

use chrysopeia_core::models::{HardwareCapability, HwAccelApi};

/// Detect available GPUs and their encoding capabilities via Vulkan.
pub async fn detect_gpus() -> Result<Vec<HardwareCapability>, super::HwDetectError> {
    // TODO: Use oximedia_accel to enumerate Vulkan devices and query
    // video encode extensions (VK_KHR_video_encode_av1, VK_KHR_video_encode_h265, etc.)
    tracing::info!("Probing GPUs via oximedia-accel Vulkan enumeration...");

    let mut capabilities = Vec::new();

    // Placeholder: in production this calls oximedia_accel::vulkan::enumerate_devices()
    // and checks each device's supported encode profiles.
    let _ = &capabilities;

    // Example of what a detected device would look like:
    // capabilities.push(HardwareCapability {
    //     device_name: "NVIDIA RTX 4090".to_string(),
    //     supports_av1: true,
    //     supports_vp9: false,
    //     supports_hevc: true,
    //     api: HwAccelApi::Vulkan,
    // });

    todo!("Implement Vulkan GPU enumeration via oximedia-accel")
}
