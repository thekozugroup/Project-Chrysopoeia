//! Codec and container knowledge: what can be produced, what fits where.
//!
//! Source files may use any codec ffmpeg can decode. Targets are limited to the
//! codecs below, which is what the UI offers.

use serde::{Deserialize, Serialize};

/// Target video codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    Av1,
    Hevc,
    H264,
    Vp9,
}

impl VideoCodec {
    pub const ALL: [VideoCodec; 4] = [Self::Av1, Self::Hevc, Self::H264, Self::Vp9];

    /// Name ffprobe reports for this codec (`codec_name`).
    pub fn ffprobe_name(self) -> &'static str {
        match self {
            Self::Av1 => "av1",
            Self::Hevc => "hevc",
            Self::H264 => "h264",
            Self::Vp9 => "vp9",
        }
    }

    /// Human-readable label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Av1 => "AV1",
            Self::Hevc => "HEVC (H.265)",
            Self::H264 => "H.264",
            Self::Vp9 => "VP9",
        }
    }

    pub fn from_probe_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "av1" | "libdav1d" | "libaom-av1" => Some(Self::Av1),
            "hevc" | "h265" => Some(Self::Hevc),
            "h264" | "avc" | "avc1" => Some(Self::H264),
            "vp9" => Some(Self::Vp9),
            _ => None,
        }
    }

    /// Royalty-free codecs (AV1, VP9).
    pub fn royalty_free(self) -> bool {
        matches!(self, Self::Av1 | Self::Vp9)
    }

    /// Whether 10-bit output is safe to produce for broad playback.
    /// H.264 High 10 exists but almost nothing hardware-decodes it.
    pub fn supports_10bit(self) -> bool {
        !matches!(self, Self::H264)
    }

    /// Compression efficiency rank of this target (higher = more efficient).
    pub fn efficiency_rank(self) -> u8 {
        match self {
            Self::Av1 => 4,
            Self::Hevc | Self::Vp9 => 3,
            Self::H264 => 2,
        }
    }
}

/// Efficiency rank of an arbitrary source codec name as reported by ffprobe.
/// Used for the "skip files that are already as efficient as the target" rule.
pub fn source_efficiency_rank(codec_name: &str) -> u8 {
    match codec_name.to_ascii_lowercase().as_str() {
        "av1" => 4,
        "hevc" | "h265" | "vp9" | "vvc" | "h266" => 3,
        "h264" | "vp8" => 2,
        _ => 1,
    }
}

/// Target audio codecs. `Copy` keeps each source track as-is where the
/// container allows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioCodec {
    Copy,
    Opus,
    Aac,
    Flac,
    Ac3,
    Eac3,
    Mp3,
    Vorbis,
}

impl AudioCodec {
    pub const ALL: [AudioCodec; 8] = [
        Self::Copy,
        Self::Opus,
        Self::Aac,
        Self::Flac,
        Self::Ac3,
        Self::Eac3,
        Self::Mp3,
        Self::Vorbis,
    ];

    /// ffmpeg encoder name.
    pub fn ffmpeg_encoder(self) -> &'static str {
        match self {
            Self::Copy => "copy",
            Self::Opus => "libopus",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Ac3 => "ac3",
            Self::Eac3 => "eac3",
            Self::Mp3 => "libmp3lame",
            Self::Vorbis => "libvorbis",
        }
    }

    /// Name ffprobe reports for this codec. `None` for `Copy`.
    pub fn ffprobe_name(self) -> Option<&'static str> {
        match self {
            Self::Copy => None,
            Self::Opus => Some("opus"),
            Self::Aac => Some("aac"),
            Self::Flac => Some("flac"),
            Self::Ac3 => Some("ac3"),
            Self::Eac3 => Some("eac3"),
            Self::Mp3 => Some("mp3"),
            Self::Vorbis => Some("vorbis"),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Copy => "Keep original",
            Self::Opus => "Opus",
            Self::Aac => "AAC",
            Self::Flac => "FLAC (lossless)",
            Self::Ac3 => "Dolby Digital (AC-3)",
            Self::Eac3 => "Dolby Digital Plus (E-AC-3)",
            Self::Mp3 => "MP3",
            Self::Vorbis => "Vorbis",
        }
    }

    /// Maximum channels the encoder accepts; sources with more are downmixed.
    /// ffmpeg's E-AC-3 encoder stops at 5.1 even though the format allows 7.1.
    pub fn max_channels(self) -> u32 {
        match self {
            Self::Mp3 => 2,
            Self::Ac3 | Self::Eac3 => 6,
            _ => 8,
        }
    }

    /// Default bitrate in kbit/s for a given channel count. `None` for
    /// lossless codecs and `Copy`.
    pub fn default_bitrate_kbps(self, channels: u32) -> Option<u32> {
        let ch = channels.clamp(1, self.max_channels());
        let kbps = match self {
            Self::Copy | Self::Flac => return None,
            Self::Opus => match ch {
                1 => 64,
                2 => 128,
                3..=6 => 320,
                _ => 448,
            },
            Self::Aac => match ch {
                1 => 96,
                2 => 192,
                3..=6 => 384,
                _ => 512,
            },
            Self::Ac3 => match ch {
                1 | 2 => 192,
                _ => 640,
            },
            Self::Eac3 => match ch {
                1 | 2 => 224,
                _ => 640,
            },
            Self::Mp3 => 192,
            Self::Vorbis => match ch {
                1 => 96,
                2 => 160,
                _ => 320,
            },
        };
        Some(kbps)
    }
}

/// Target containers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Container {
    Mkv,
    Mp4,
    Webm,
}

/// What to do with a subtitle stream when writing a given container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleAction {
    Copy,
    /// Re-encode to the given ffmpeg subtitle encoder (e.g. `mov_text`).
    Convert(&'static str),
    Drop,
}

/// Text-based subtitle codecs (as reported by ffprobe).
pub const TEXT_SUBTITLE_CODECS: &[&str] = &[
    "subrip",
    "srt",
    "ass",
    "ssa",
    "webvtt",
    "mov_text",
    "text",
    "microdvd",
    "subviewer",
    "subviewer1",
    "jacosub",
    "realtext",
    "sami",
    "stl",
    "mpl2",
    "pjs",
    "vplayer",
];

/// Bitmap subtitle codecs; these cannot be converted to text formats.
pub const IMAGE_SUBTITLE_CODECS: &[&str] = &[
    "hdmv_pgs_subtitle",
    "pgssub",
    "dvd_subtitle",
    "dvdsub",
    "dvb_subtitle",
    "dvbsub",
    "xsub",
];

/// Subtitle codecs the Matroska muxer can store as they are (its codec tag
/// table in ffmpeg 6.1). Other text formats are converted, other picture
/// formats dropped.
pub const MATROSKA_SUBTITLES: &[&str] = &[
    "subrip",
    "ass",
    "webvtt",
    "text",
    "dvd_subtitle",
    "dvb_subtitle",
    "hdmv_pgs_subtitle",
    "hdmv_text_subtitle",
    "arib_caption",
];

pub fn is_text_subtitle(codec_name: &str) -> bool {
    TEXT_SUBTITLE_CODECS.contains(&codec_name.to_ascii_lowercase().as_str())
}

pub fn is_image_subtitle(codec_name: &str) -> bool {
    IMAGE_SUBTITLE_CODECS.contains(&codec_name.to_ascii_lowercase().as_str())
}

impl Container {
    pub const ALL: [Container; 3] = [Self::Mkv, Self::Mp4, Self::Webm];

    /// File extension (no dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Mkv => "mkv",
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
        }
    }

    /// ffmpeg muxer name for `-f`.
    pub fn ffmpeg_format(self) -> &'static str {
        match self {
            Self::Mkv => "matroska",
            Self::Mp4 => "mp4",
            Self::Webm => "webm",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mkv => "MKV",
            Self::Mp4 => "MP4",
            Self::Webm => "WebM",
        }
    }

    pub fn supports_video(self, codec: VideoCodec) -> bool {
        match self {
            Self::Mkv | Self::Mp4 => true,
            Self::Webm => matches!(codec, VideoCodec::Av1 | VideoCodec::Vp9),
        }
    }

    /// Whether a target audio codec may be written into this container.
    pub fn supports_audio(self, codec: AudioCodec) -> bool {
        match (self, codec) {
            (_, AudioCodec::Copy) => true,
            (Self::Mkv, _) => true,
            (
                Self::Mp4,
                AudioCodec::Aac
                | AudioCodec::Ac3
                | AudioCodec::Eac3
                | AudioCodec::Opus
                | AudioCodec::Mp3,
            ) => true,
            (Self::Mp4, _) => false,
            (Self::Webm, AudioCodec::Opus | AudioCodec::Vorbis) => true,
            (Self::Webm, _) => false,
        }
    }

    /// Whether a *source* audio stream (ffprobe codec name) can be stream-copied
    /// into this container.
    ///
    /// Matroska refuses Blu-ray/DVD PCM and SMPTE 302M. MP2 is excluded from
    /// MP4: it is stored as generic MPEG audio, which Apple players (Layer III
    /// only) play silently.
    pub fn can_copy_audio(self, codec_name: &str) -> bool {
        let c = codec_name.to_ascii_lowercase();
        match self {
            Self::Mkv => !matches!(
                c.as_str(),
                "pcm_bluray" | "pcm_dvd" | "s302m" | "pcm_s24daud"
            ),
            Self::Mp4 => matches!(c.as_str(), "aac" | "ac3" | "eac3" | "mp3" | "opus" | "alac"),
            Self::Webm => matches!(c.as_str(), "opus" | "vorbis"),
        }
    }

    /// Audio codec used when a source track must be re-encoded because it
    /// cannot be copied into this container.
    pub fn fallback_audio(self) -> AudioCodec {
        match self {
            Self::Mkv => AudioCodec::Opus,
            Self::Mp4 => AudioCodec::Aac,
            Self::Webm => AudioCodec::Opus,
        }
    }

    pub fn subtitle_action(self, codec_name: &str) -> SubtitleAction {
        let c = codec_name.to_ascii_lowercase();
        let text = is_text_subtitle(&c);
        match self {
            Self::Mkv => {
                if MATROSKA_SUBTITLES.contains(&c.as_str()) {
                    SubtitleAction::Copy
                } else if c == "ssa" {
                    // Old SSA becomes ASS, which keeps the styling.
                    SubtitleAction::Convert("ass")
                } else if text {
                    SubtitleAction::Convert("srt")
                } else {
                    // Includes picture formats the muxer can't store (DivX XSUB).
                    SubtitleAction::Drop
                }
            }
            Self::Mp4 => {
                if c == "mov_text" {
                    SubtitleAction::Copy
                } else if text {
                    SubtitleAction::Convert("mov_text")
                } else {
                    SubtitleAction::Drop
                }
            }
            Self::Webm => {
                if c == "webvtt" {
                    SubtitleAction::Copy
                } else if text {
                    SubtitleAction::Convert("webvtt")
                } else {
                    SubtitleAction::Drop
                }
            }
        }
    }

    /// Only Matroska carries attachments (fonts for styled subtitles).
    pub fn supports_attachments(self) -> bool {
        matches!(self, Self::Mkv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webm_rejects_patent_encumbered_codecs() {
        assert!(!Container::Webm.supports_video(VideoCodec::Hevc));
        assert!(!Container::Webm.supports_audio(AudioCodec::Aac));
        assert!(Container::Webm.supports_audio(AudioCodec::Opus));
    }

    #[test]
    fn subtitle_actions() {
        assert_eq!(
            Container::Mkv.subtitle_action("subrip"),
            SubtitleAction::Copy
        );
        assert_eq!(
            Container::Mkv.subtitle_action("mov_text"),
            SubtitleAction::Convert("srt")
        );
        assert_eq!(
            Container::Mp4.subtitle_action("ass"),
            SubtitleAction::Convert("mov_text")
        );
        assert_eq!(
            Container::Mp4.subtitle_action("hdmv_pgs_subtitle"),
            SubtitleAction::Drop
        );
        assert_eq!(
            Container::Webm.subtitle_action("subrip"),
            SubtitleAction::Convert("webvtt")
        );
    }

    #[test]
    fn efficiency_ranks() {
        assert!(source_efficiency_rank("av1") >= VideoCodec::Hevc.efficiency_rank());
        assert!(source_efficiency_rank("h264") < VideoCodec::Hevc.efficiency_rank());
        assert_eq!(source_efficiency_rank("mpeg2video"), 1);
    }

    #[test]
    fn audio_bitrates() {
        assert_eq!(AudioCodec::Opus.default_bitrate_kbps(2), Some(128));
        assert_eq!(AudioCodec::Flac.default_bitrate_kbps(6), None);
        // AC-3 caps at 6 channels, so 8 channels is treated as 6.
        assert_eq!(AudioCodec::Ac3.default_bitrate_kbps(8), Some(640));
    }

    #[test]
    fn serde_names() {
        assert_eq!(
            serde_json::to_string(&VideoCodec::Hevc).unwrap(),
            "\"hevc\""
        );
        assert_eq!(
            serde_json::to_string(&AudioCodec::Copy).unwrap(),
            "\"copy\""
        );
        assert_eq!(serde_json::to_string(&Container::Webm).unwrap(), "\"webm\"");
    }
}
