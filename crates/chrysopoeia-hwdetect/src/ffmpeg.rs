//! FFmpeg fallback detection for hardware encoders.

use chrysopoeia_core::models::{HardwareCapability, HwAccelApi};
use tokio::process::Command;

/// Known FFmpeg hardware encoder names and their corresponding API/codec info.
const KNOWN_HW_ENCODERS: &[(&str, HwAccelApi, bool, bool, bool)] = &[
    // (encoder_name, api, av1, vp9, hevc)
    ("av1_nvenc", HwAccelApi::Nvenc, true, false, false),
    ("hevc_nvenc", HwAccelApi::Nvenc, false, false, true),
    ("h264_nvenc", HwAccelApi::Nvenc, false, false, false),
    ("av1_vaapi", HwAccelApi::Vaapi, true, false, false),
    ("h264_vaapi", HwAccelApi::Vaapi, false, false, false),
    ("hevc_vaapi", HwAccelApi::Vaapi, false, false, true),
    ("av1_qsv", HwAccelApi::Qsv, true, false, false),
    ("h264_qsv", HwAccelApi::Qsv, false, false, false),
    ("hevc_qsv", HwAccelApi::Qsv, false, false, true),
    ("h264_videotoolbox", HwAccelApi::VideoToolbox, false, false, false),
    ("hevc_videotoolbox", HwAccelApi::VideoToolbox, false, false, true),
];

/// Detect available hardware encoders by shelling out to `ffmpeg -encoders`.
pub async fn detect_ffmpeg_encoders() -> Result<Vec<HardwareCapability>, super::HwDetectError> {
    tracing::info!("Probing FFmpeg for hardware encoders...");

    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .await
        .map_err(|e| super::HwDetectError::FfmpegDetection(format!("Failed to run ffmpeg: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_ffmpeg_encoders(&stdout)
}

/// Parse the output of `ffmpeg -encoders` to find known hardware encoders.
fn parse_ffmpeg_encoders(output: &str) -> Result<Vec<HardwareCapability>, super::HwDetectError> {
    let mut capabilities = Vec::new();
    // Group by API to merge capabilities per device
    let mut api_caps: std::collections::HashMap<HwAccelApi, (bool, bool, bool)> =
        std::collections::HashMap::new();

    for line in output.lines() {
        let trimmed = line.trim();
        for &(encoder, api, av1, vp9, hevc) in KNOWN_HW_ENCODERS {
            if trimmed.contains(encoder) {
                let entry = api_caps.entry(api).or_insert((false, false, false));
                entry.0 |= av1;
                entry.1 |= vp9;
                entry.2 |= hevc;
            }
        }
    }

    for (api, (av1, vp9, hevc)) in api_caps {
        capabilities.push(HardwareCapability {
            device_name: format!("FFmpeg {api:?}"),
            supports_av1: av1,
            supports_vp9: vp9,
            supports_hevc: hevc,
            api,
        });
    }

    Ok(capabilities)
}
