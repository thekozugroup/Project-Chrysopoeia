//! Registry of ffmpeg video encoders Chrysopoeia knows how to drive.

use serde::{Deserialize, Serialize};

use crate::codec::VideoCodec;

/// Acceleration API an encoder runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HwApi {
    /// CPU encoding.
    Software,
    /// NVIDIA NVENC (CUDA).
    Nvenc,
    /// Intel Quick Sync Video.
    Qsv,
    /// VA-API (Intel and AMD on Linux).
    Vaapi,
    /// Apple VideoToolbox.
    VideoToolbox,
    /// AMD Advanced Media Framework.
    Amf,
    /// Rockchip MPP (ARM boards).
    Rkmpp,
    /// V4L2 memory-to-memory (Raspberry Pi and other ARM boards).
    V4l2m2m,
}

impl HwApi {
    pub fn is_hardware(self) -> bool {
        !matches!(self, Self::Software)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Software => "CPU",
            Self::Nvenc => "NVIDIA NVENC",
            Self::Qsv => "Intel Quick Sync",
            Self::Vaapi => "VA-API",
            Self::VideoToolbox => "Apple VideoToolbox",
            Self::Amf => "AMD AMF",
            Self::Rkmpp => "Rockchip MPP",
            Self::V4l2m2m => "V4L2",
        }
    }
}

/// Static description of one ffmpeg video encoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EncoderInfo {
    /// ffmpeg encoder name, e.g. `hevc_nvenc`.
    pub name: &'static str,
    pub codec: VideoCodec,
    pub api: HwApi,
}

const fn enc(name: &'static str, codec: VideoCodec, api: HwApi) -> EncoderInfo {
    EncoderInfo { name, codec, api }
}

/// All video encoders, grouped by codec. Within a codec, software encoders are
/// listed in order of preference.
pub const VIDEO_ENCODERS: &[EncoderInfo] = &[
    // AV1
    enc("libsvtav1", VideoCodec::Av1, HwApi::Software),
    enc("libaom-av1", VideoCodec::Av1, HwApi::Software),
    enc("av1_nvenc", VideoCodec::Av1, HwApi::Nvenc),
    enc("av1_qsv", VideoCodec::Av1, HwApi::Qsv),
    enc("av1_vaapi", VideoCodec::Av1, HwApi::Vaapi),
    enc("av1_amf", VideoCodec::Av1, HwApi::Amf),
    // HEVC
    enc("libx265", VideoCodec::Hevc, HwApi::Software),
    enc("hevc_nvenc", VideoCodec::Hevc, HwApi::Nvenc),
    enc("hevc_qsv", VideoCodec::Hevc, HwApi::Qsv),
    enc("hevc_vaapi", VideoCodec::Hevc, HwApi::Vaapi),
    enc("hevc_videotoolbox", VideoCodec::Hevc, HwApi::VideoToolbox),
    enc("hevc_amf", VideoCodec::Hevc, HwApi::Amf),
    enc("hevc_rkmpp", VideoCodec::Hevc, HwApi::Rkmpp),
    enc("hevc_v4l2m2m", VideoCodec::Hevc, HwApi::V4l2m2m),
    // H.264
    enc("libx264", VideoCodec::H264, HwApi::Software),
    enc("h264_nvenc", VideoCodec::H264, HwApi::Nvenc),
    enc("h264_qsv", VideoCodec::H264, HwApi::Qsv),
    enc("h264_vaapi", VideoCodec::H264, HwApi::Vaapi),
    enc("h264_videotoolbox", VideoCodec::H264, HwApi::VideoToolbox),
    enc("h264_amf", VideoCodec::H264, HwApi::Amf),
    enc("h264_rkmpp", VideoCodec::H264, HwApi::Rkmpp),
    enc("h264_v4l2m2m", VideoCodec::H264, HwApi::V4l2m2m),
    // VP9
    enc("libvpx-vp9", VideoCodec::Vp9, HwApi::Software),
    enc("vp9_qsv", VideoCodec::Vp9, HwApi::Qsv),
    enc("vp9_vaapi", VideoCodec::Vp9, HwApi::Vaapi),
];

/// Look up an encoder by ffmpeg name.
pub fn find_encoder(name: &str) -> Option<&'static EncoderInfo> {
    VIDEO_ENCODERS.iter().find(|e| e.name == name)
}

/// Encoders for a codec, in registry order.
pub fn encoders_for(codec: VideoCodec) -> impl Iterator<Item = &'static EncoderInfo> {
    VIDEO_ENCODERS.iter().filter(move |e| e.codec == codec)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_codec_has_a_software_encoder() {
        for codec in VideoCodec::ALL {
            assert!(
                encoders_for(codec).any(|e| e.api == HwApi::Software),
                "{codec:?} has no software encoder"
            );
        }
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<_> = VIDEO_ENCODERS.iter().map(|e| e.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), VIDEO_ENCODERS.len());
    }
}
