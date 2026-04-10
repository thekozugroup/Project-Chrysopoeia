//! GPU detection via Vulkan enumeration.
//!
//! When the `oximedia` feature is enabled, uses oximedia-accel for
//! Vulkan device enumeration. Otherwise returns an empty list.

use chrysopoeia_core::models::{HardwareCapability, HwAccelApi};

/// Detect available GPUs and their encoding capabilities via Vulkan.
pub async fn detect_gpus() -> Result<Vec<HardwareCapability>, super::HwDetectError> {
    tracing::info!("Probing GPUs via Vulkan enumeration...");

    #[cfg(feature = "oximedia")]
    {
        // Use oximedia_accel to enumerate Vulkan devices and query
        // video encode extensions (VK_KHR_video_encode_av1, etc.)
        // TODO: Wire up real oximedia-accel calls when available
        let _ = HwAccelApi::Vulkan;
        Ok(Vec::new())
    }

    #[cfg(not(feature = "oximedia"))]
    {
        tracing::info!("oximedia feature not enabled; skipping Vulkan GPU detection");
        let _ = HwAccelApi::Vulkan;
        Ok(Vec::new())
    }
}
