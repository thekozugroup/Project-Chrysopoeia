//! Probe results: what a media file contains.

use serde::{Deserialize, Serialize};

/// Stream type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Attachment,
    Data,
}

/// High dynamic range signalling detected on a video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HdrFormat {
    /// PQ transfer (SMPTE 2084), static metadata.
    Hdr10,
    /// HDR10 with dynamic metadata.
    Hdr10Plus,
    /// Hybrid log-gamma.
    Hlg,
    /// Dolby Vision (RPU side data present). Re-encoding keeps the HDR10
    /// base layer only.
    DolbyVision,
}

/// One stream inside a media file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamInfo {
    /// ffprobe stream index (absolute, used for `-map 0:<index>`).
    pub index: u32,
    pub kind: Option<StreamKind>,
    /// ffprobe `codec_name`, e.g. `h264`, `truehd`, `hdmv_pgs_subtitle`.
    pub codec: String,
    pub profile: Option<String>,
    /// ISO 639 language tag, if present.
    pub language: Option<String>,
    pub title: Option<String>,
    pub is_default: bool,
    pub is_forced: bool,
    /// Cover art stored as a video stream.
    pub is_attached_pic: bool,
    /// File name of an attachment or of an MKV cover image (its `filename`
    /// tag), e.g. `cover.jpg`. Kept so a cover image copied into a new MKV
    /// keeps its name, which media servers look for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// MIME type of an attachment or of an MKV cover image (its `mimetype`
    /// tag), e.g. `image/jpeg`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mimetype: Option<String>,
    pub bit_rate: Option<u64>,
    /// The track's Matroska statistics tags (`DURATION-eng`, `BPS`,
    /// `_STATISTICS_WRITING_APP`, … see [`crate::tags`]), named as the file
    /// spells them. They describe the track as it was when they were
    /// written, so a conversion removes them (the MKV writer adds a fresh
    /// `DURATION`). Probes stored by older versions have none listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statistics_tags: Vec<String>,

    // Video
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pix_fmt: Option<String>,
    pub bit_depth: Option<u8>,
    pub frame_rate: Option<f64>,
    pub color_primaries: Option<String>,
    pub color_transfer: Option<String>,
    pub color_space: Option<String>,
    pub color_range: Option<String>,
    pub hdr: Option<HdrFormat>,
    /// Dolby Vision whose configuration record says the picture has no
    /// standard base layer (profile 5, or compatibility id 0): it is in
    /// Dolby's own colour space, so re-encoding it would ruin its colours.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dolby_vision_without_base_layer: bool,
    pub interlaced: bool,
    /// HDR10 static metadata: the mastering display (SMPTE ST 2086).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mastering_display: Option<MasteringDisplay>,
    /// HDR10 static metadata: content light levels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_light: Option<ContentLight>,

    // Audio
    pub channels: Option<u32>,
    pub channel_layout: Option<String>,
    pub sample_rate: Option<u32>,
}

/// The colour volume of the display an HDR video was mastered on (SMPTE ST
/// 2086), carried with HDR10 video so TVs can map it to their own range.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MasteringDisplay {
    /// CIE 1931 xy chromaticity of the red primary.
    pub red: [f64; 2],
    pub green: [f64; 2],
    pub blue: [f64; 2],
    pub white_point: [f64; 2],
    /// Peak luminance in cd/m² (nits).
    pub max_luminance: f64,
    /// Black level in cd/m².
    pub min_luminance: f64,
}

/// Content light levels of an HDR10 video (CTA-861.3), in cd/m².
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentLight {
    /// Brightest pixel of the whole video (MaxCLL).
    pub max_cll: u32,
    /// Brightest frame on average (MaxFALL).
    pub max_fall: u32,
}

/// Everything ffprobe told us about a file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeInfo {
    /// First token of ffprobe `format_name` (e.g. `matroska`, `mov`, `mpegts`).
    pub container: String,
    pub format_long_name: Option<String>,
    pub duration_secs: Option<f64>,
    /// Overall bitrate in bit/s.
    pub bit_rate: Option<u64>,
    pub size_bytes: u64,
    pub start_time: Option<f64>,
    pub chapters: u32,
    /// Matroska statistics tags stored for the whole file rather than a
    /// track (`DURATION-eng` carried over from an MP4's metadata, say; see
    /// [`crate::tags`]), named as the file spells them. A conversion to
    /// MKV or WebM removes them like the tracks' own. Probes stored by
    /// older versions have none listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statistics_tags: Vec<String>,
    pub streams: Vec<StreamInfo>,
}

impl ProbeInfo {
    /// The main video stream: the first video stream that is not cover art.
    pub fn primary_video(&self) -> Option<&StreamInfo> {
        self.streams
            .iter()
            .find(|s| s.kind == Some(StreamKind::Video) && !s.is_attached_pic)
    }

    pub fn audio_streams(&self) -> impl Iterator<Item = &StreamInfo> {
        self.streams
            .iter()
            .filter(|s| s.kind == Some(StreamKind::Audio))
    }

    pub fn subtitle_streams(&self) -> impl Iterator<Item = &StreamInfo> {
        self.streams
            .iter()
            .filter(|s| s.kind == Some(StreamKind::Subtitle))
    }

    /// True when there is no real video (music, podcasts, audio with cover art).
    pub fn is_audio_only(&self) -> bool {
        self.primary_video().is_none()
    }

    pub fn video_codec(&self) -> Option<&str> {
        self.primary_video().map(|s| s.codec.as_str())
    }

    pub fn audio_codec(&self) -> Option<&str> {
        self.audio_streams().next().map(|s| s.codec.as_str())
    }

    /// Short resolution label: `8K`, `4K`, `1440p`, `1080p`, `720p`, `576p`,
    /// `480p` or `SD`. Uses the larger of width-based and height-based classes
    /// so cropped widescreen (e.g. 1920x800) still reads as 1080p, and reads
    /// a portrait picture as if it were turned sideways (1080x1920 is
    /// 1080p, like the size limit that converts it).
    pub fn resolution_label(&self) -> Option<&'static str> {
        let v = self.primary_video()?;
        let (w, h) = (v.width?, v.height?);
        Some(resolution_label(w, h))
    }

    pub fn hdr(&self) -> Option<HdrFormat> {
        self.primary_video().and_then(|v| v.hdr)
    }
}

/// See [`ProbeInfo::resolution_label`].
pub fn resolution_label(width: u32, height: u32) -> &'static str {
    // Classes describe landscape pictures: a phone's portrait 1080x1920 is
    // a 1080p picture on its side, not 1440p.
    let (width, height) = (width.max(height), width.min(height));
    let by_width = match width {
        w if w >= 7000 => 6,
        w if w >= 3200 => 5,
        w if w >= 2300 => 4,
        w if w >= 1700 => 3,
        w if w >= 1200 => 2,
        w if w >= 1000 => 1,
        _ => 0,
    };
    let by_height = match height {
        h if h >= 4000 => 6,
        h if h >= 2000 => 5,
        h if h >= 1400 => 4,
        h if h >= 1000 => 3,
        h if h >= 700 => 2,
        h if h >= 560 => 1,
        _ => 0,
    };
    match by_width.max(by_height) {
        6 => "8K",
        5 => "4K",
        4 => "1440p",
        3 => "1080p",
        2 => "720p",
        1 => "576p",
        _ if height >= 470 => "480p",
        _ => "SD",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(resolution_label(3840, 2160), "4K");
        assert_eq!(resolution_label(1920, 800), "1080p");
        assert_eq!(resolution_label(1280, 720), "720p");
        assert_eq!(resolution_label(720, 576), "576p");
        assert_eq!(resolution_label(720, 480), "480p");
        assert_eq!(resolution_label(320, 240), "SD");
    }

    /// A portrait picture has the class of the same picture turned
    /// sideways, as the size limit judges it.
    #[test]
    fn portrait_labels_match_landscape() {
        assert_eq!(resolution_label(1080, 1920), "1080p");
        assert_eq!(resolution_label(720, 1280), "720p");
        assert_eq!(resolution_label(2160, 3840), "4K");
        assert_eq!(resolution_label(1440, 2560), "1440p");
        assert_eq!(resolution_label(480, 854), "480p");
        assert_eq!(resolution_label(800, 1920), "1080p");
    }

    #[test]
    fn cover_art_is_not_video() {
        let probe = ProbeInfo {
            streams: vec![
                StreamInfo {
                    index: 0,
                    kind: Some(StreamKind::Audio),
                    codec: "mp3".into(),
                    ..Default::default()
                },
                StreamInfo {
                    index: 1,
                    kind: Some(StreamKind::Video),
                    codec: "mjpeg".into(),
                    is_attached_pic: true,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        assert!(probe.is_audio_only());
        assert_eq!(probe.audio_codec(), Some("mp3"));
    }
}
