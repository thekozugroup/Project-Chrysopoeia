//! Per-encoder quality and speed mapping.
//!
//! Users pick a [`QualityLevel`] ("Smallest files" … "Best quality") and a
//! [`SpeedPreset`]. Every ffmpeg encoder expresses those in its own scale:
//! CRF for the CPU encoders, CQ for NVENC, ICQ for Quick Sync, a constant QP
//! (or AV1/VP9 q-index) for VA-API, AMF and Rockchip, a 0–100 quality for
//! VideoToolbox and a bitrate for V4L2. This module holds one table per
//! encoder family and turns the user's choice into encoder options.
//!
//! Mapping assumptions (all values are starting points tuned for "visually
//! transparent at Balanced" on typical 1080p film/TV content):
//! - CPU encoders follow the scales each project documents: x264/x265 CRF is
//!   0–51 (23/28 are their defaults), SVT-AV1/libaom/libvpx CRF is 0–63.
//!   The ladders are spaced so each step is roughly a 15–25 % size change.
//! - Hardware encoders are less efficient than the CPU encoders at the same
//!   number, so their ladders sit a little lower (higher quality) to land at a
//!   similar visual result.
//! - AV1 and VP9 on VA-API and AMD AMF take a q-index from 0 to 255 instead of
//!   a 0–51 QP. libaom maps its 0–63 CRF onto that range at roughly ×4, so the
//!   hardware ladder is the CPU CRF ladder (shifted two steps towards quality)
//!   multiplied by four.
//! - `quality_override` is a quantizer-style number (CRF, CQ, QP; lower is
//!   better), as the core profile documents it. It is used in the encoder's
//!   own scale when that scale is quantizer-style too. It is ignored (the
//!   level ladder is used, and the plan says so) when the scale is of another
//!   kind (VideoToolbox's 1–100 quality and V4L2's bitrate, where 24 would
//!   mean a terrible picture) or when the value is above the encoder's
//!   maximum. One profile drives a whole fallback chain (for example NVENC,
//!   then libx265), so an out-of-range value was almost certainly meant for
//!   another encoder, and clamping it would pick the worst possible picture.
//!   Values below the minimum are raised to it. Either way a bad value can
//!   never make ffmpeg refuse to start.
//!
//! Encoder-wide options that audio encoders also read (`global_quality`,
//! `b`, `profile`) always carry a `:v` stream specifier: an unscoped
//! `-global_quality` would, for example, silently change the native AAC
//! encoder's rate/distortion balance.

use szalinski_core::{HwApi, QualityLevel, SpeedPreset, VideoCodec};

/// How an encoder expresses its quality target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualityScale {
    /// Short name of the scale for the Advanced settings, e.g. `CRF`, `CQ`.
    pub name: &'static str,
    /// Lowest value the encoder accepts.
    pub min: u32,
    /// Highest value the encoder accepts.
    pub max: u32,
    /// True when a larger number means better quality (VideoToolbox, V4L2
    /// bitrate); false for quantizer-style scales where larger means smaller
    /// files.
    pub higher_is_better: bool,
    /// Value for each [`QualityLevel`], indexed by [`QualityLevel::step`]
    /// (Smallest first). For V4L2 these are kbit/s at 1080p; the argument
    /// builder scales them by picture area.
    pub levels: [u32; 5],
}

impl QualityScale {
    /// Whether a quantizer-style `quality_override` applies to this scale
    /// (see the module docs): the scale is quantizer-style and the value is
    /// not above its maximum.
    pub fn accepts_override(&self, value: u32) -> bool {
        !self.higher_is_better && value <= self.max
    }

    /// The value for `quality`, or `quality_override` (raised to `min` if
    /// needed) when [`Self::accepts_override`] allows it.
    pub fn value(&self, quality: QualityLevel, quality_override: Option<u32>) -> u32 {
        match quality_override {
            Some(v) if self.accepts_override(v) => v.max(self.min),
            _ => self.levels[usize::from(quality.step()).min(self.levels.len() - 1)],
        }
    }
}

/// Everything that decides an encoder's quality and speed options.
#[derive(Debug, Clone, Copy)]
pub struct VideoQuality<'a> {
    /// ffmpeg encoder name, e.g. `libsvtav1` or `hevc_nvenc`.
    pub encoder: &'a str,
    /// Acceleration API the encoder runs on.
    pub api: HwApi,
    /// Codec the encoder produces.
    pub codec: VideoCodec,
    /// The profile's quality target.
    pub quality: QualityLevel,
    /// The profile's effort setting.
    pub speed: SpeedPreset,
    /// Quantizer-style value in the encoder's own scale; wins over
    /// `quality` where [`QualityScale::accepts_override`] allows it.
    pub quality_override: Option<u32>,
    /// Whether the output is 10-bit (selects the Main 10 / Profile 2 profile).
    pub ten_bit: bool,
    /// Output picture width in pixels (only bitrate-driven encoders use it).
    pub width: u32,
    /// Output picture height in pixels (only bitrate-driven encoders use it).
    pub height: u32,
}

/// Encoder families that share one option vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    SvtAv1,
    Aom,
    X265,
    X264,
    Vpx9,
    Nvenc,
    Qsv,
    Vaapi,
    VideoToolbox,
    Amf,
    Rkmpp,
    V4l2m2m,
    /// A CPU encoder this module has no table for: encoder defaults are used.
    OtherSoftware,
}

fn family(encoder: &str, api: HwApi) -> Family {
    match api {
        HwApi::Software => match encoder {
            "libsvtav1" => Family::SvtAv1,
            "libaom-av1" => Family::Aom,
            "libx265" => Family::X265,
            "libx264" => Family::X264,
            "libvpx-vp9" => Family::Vpx9,
            _ => Family::OtherSoftware,
        },
        HwApi::Nvenc => Family::Nvenc,
        HwApi::Qsv => Family::Qsv,
        HwApi::Vaapi => Family::Vaapi,
        HwApi::VideoToolbox => Family::VideoToolbox,
        HwApi::Amf => Family::Amf,
        HwApi::Rkmpp => Family::Rkmpp,
        HwApi::V4l2m2m => Family::V4l2m2m,
    }
}

const fn scale(name: &'static str, min: u32, max: u32, levels: [u32; 5]) -> QualityScale {
    QualityScale {
        name,
        min,
        max,
        higher_is_better: false,
        levels,
    }
}

// CPU encoders.
const SVT_AV1: QualityScale = scale("CRF", 0, 63, [40, 36, 32, 28, 24]);
const AOM_AV1: QualityScale = scale("CRF", 0, 63, [40, 36, 32, 28, 24]);
const X265: QualityScale = scale("CRF", 0, 51, [28, 26, 24, 22, 20]);
const X264: QualityScale = scale("CRF", 0, 51, [26, 24, 22, 20, 18]);
const VPX_VP9: QualityScale = scale("CRF", 0, 63, [40, 36, 32, 28, 24]);

// NVENC `-cq` (0 means "automatic", so the floor is 1; ffmpeg caps it at 51
// for every NVENC codec, AV1 included).
const NVENC_H264: QualityScale = scale("CQ", 1, 51, [27, 25, 23, 21, 19]);
const NVENC_HEVC: QualityScale = scale("CQ", 1, 51, [30, 28, 26, 24, 22]);
const NVENC_AV1: QualityScale = scale("CQ", 1, 51, [38, 34, 31, 28, 25]);

// Quick Sync ICQ `-global_quality` (1–51 for every codec; the driver maps it
// onto AV1/VP9 q-indices itself).
const QSV_H264: QualityScale = scale("ICQ", 1, 51, [27, 25, 23, 21, 19]);
const QSV_HEVC: QualityScale = scale("ICQ", 1, 51, [28, 26, 24, 22, 20]);
const QSV_AV1_VP9: QualityScale = scale("ICQ", 1, 51, [30, 28, 26, 24, 22]);

// Constant QP for H.264/HEVC on VA-API, AMF and Rockchip MPP.
const QP_H264: QualityScale = scale("QP", 1, 51, [27, 25, 23, 21, 19]);
const QP_HEVC: QualityScale = scale("QP", 1, 51, [28, 26, 24, 22, 20]);
// AV1/VP9 q-index (0–255) on VA-API and AMF: the CRF ladder 38/34/30/26/22
// multiplied by four (see the module docs).
const QINDEX: QualityScale = scale("Q-index", 1, 255, [152, 136, 120, 104, 88]);

// VideoToolbox `-q:v`: 1–100, higher is better. H.264 needs a little more
// than HEVC for the same look.
const VT_HEVC: QualityScale = QualityScale {
    higher_is_better: true,
    ..scale("Quality", 1, 100, [50, 55, 60, 65, 70])
};
const VT_H264: QualityScale = QualityScale {
    higher_is_better: true,
    ..scale("Quality", 1, 100, [55, 60, 65, 70, 75])
};

// V4L2 mem2mem has no quality mode, only bitrate: kbit/s at 1080p.
const V4L2_H264: QualityScale = QualityScale {
    higher_is_better: true,
    ..scale("kbit/s", 100, 100_000, [4_000, 5_000, 6_500, 8_000, 10_000])
};
const V4L2_HEVC: QualityScale = QualityScale {
    higher_is_better: true,
    ..scale("kbit/s", 100, 100_000, [2_500, 3_200, 4_000, 5_000, 6_500])
};

/// The quality scale an encoder uses, or `None` for a CPU encoder this
/// module does not know (it then runs with its own defaults).
pub fn quality_scale(encoder: &str, api: HwApi, codec: VideoCodec) -> Option<QualityScale> {
    let scale = match family(encoder, api) {
        Family::SvtAv1 => SVT_AV1,
        Family::Aom => AOM_AV1,
        Family::X265 => X265,
        Family::X264 => X264,
        Family::Vpx9 => VPX_VP9,
        Family::Nvenc => match codec {
            VideoCodec::H264 => NVENC_H264,
            VideoCodec::Av1 => NVENC_AV1,
            VideoCodec::Hevc | VideoCodec::Vp9 => NVENC_HEVC,
        },
        Family::Qsv => match codec {
            VideoCodec::H264 => QSV_H264,
            VideoCodec::Hevc => QSV_HEVC,
            VideoCodec::Av1 | VideoCodec::Vp9 => QSV_AV1_VP9,
        },
        Family::Vaapi | Family::Amf => match codec {
            VideoCodec::H264 => QP_H264,
            VideoCodec::Hevc => QP_HEVC,
            VideoCodec::Av1 | VideoCodec::Vp9 => QINDEX,
        },
        Family::Rkmpp => match codec {
            VideoCodec::H264 => QP_H264,
            _ => QP_HEVC,
        },
        Family::VideoToolbox => match codec {
            VideoCodec::H264 => VT_H264,
            _ => VT_HEVC,
        },
        Family::V4l2m2m => match codec {
            VideoCodec::H264 => V4L2_H264,
            _ => V4L2_HEVC,
        },
        Family::OtherSoftware => return None,
    };
    Some(scale)
}

/// Whether `q.quality_override` is set but not used for this encoder (its
/// scale is of another kind, the value is out of range, or the encoder has
/// no quality table). The planner turns this into a note.
pub fn override_ignored(q: &VideoQuality<'_>) -> bool {
    match (q.quality_override, quality_scale(q.encoder, q.api, q.codec)) {
        (Some(value), Some(scale)) => !scale.accepts_override(value),
        (Some(_), None) => true,
        (None, _) => false,
    }
}

/// The name of a quality level as the UI shows it.
pub fn quality_label(level: QualityLevel) -> &'static str {
    match level {
        QualityLevel::Smallest => "Smallest files",
        QualityLevel::Small => "Small files",
        QualityLevel::Balanced => "Balanced",
        QualityLevel::High => "High quality",
        QualityLevel::Best => "Best quality",
    }
}

/// Encoder options for quality, speed and (for 10-bit output) profile.
///
/// Returns everything that goes after `-c:v <encoder>` except the pixel
/// format and filters, which depend on where frames are decoded and are
/// chosen by [`crate::plan::build_plan`].
pub fn video_quality_args(q: &VideoQuality<'_>) -> Vec<String> {
    let fam = family(q.encoder, q.api);
    let value =
        quality_scale(q.encoder, q.api, q.codec).map(|s| s.value(q.quality, q.quality_override));
    let mut args: Vec<String> = Vec::new();
    let v = value.unwrap_or_default().to_string();

    match fam {
        Family::SvtAv1 => {
            // SVT-AV1 presets run 0 (slowest) to 13; 8/6/4 are the usual
            // "fast", "default-ish" and "archival" points.
            let preset = match q.speed {
                SpeedPreset::Fast => "8",
                SpeedPreset::Balanced => "6",
                SpeedPreset::Thorough => "4",
            };
            push(&mut args, &["-crf", &v, "-preset", preset]);
        }
        Family::Aom => {
            // `-b:v 0` puts libaom in constant-quality mode; row-mt is needed
            // for it to use more than a couple of cores.
            let cpu_used = match q.speed {
                SpeedPreset::Fast => "8",
                SpeedPreset::Balanced => "6",
                SpeedPreset::Thorough => "4",
            };
            push(
                &mut args,
                &[
                    "-crf",
                    &v,
                    "-b:v",
                    "0",
                    "-cpu-used",
                    cpu_used,
                    "-row-mt",
                    "1",
                ],
            );
        }
        Family::X265 | Family::X264 => {
            let preset = match q.speed {
                SpeedPreset::Fast => "fast",
                SpeedPreset::Balanced => "medium",
                SpeedPreset::Thorough => "slow",
            };
            push(&mut args, &["-crf", &v, "-preset", preset]);
        }
        Family::Vpx9 => {
            // Constant quality needs `-b:v 0`; "good" deadline with cpu-used
            // 4/2/1 spans the useful speed range (0 is impractically slow).
            let cpu_used = match q.speed {
                SpeedPreset::Fast => "4",
                SpeedPreset::Balanced => "2",
                SpeedPreset::Thorough => "1",
            };
            push(
                &mut args,
                &[
                    "-crf",
                    &v,
                    "-b:v",
                    "0",
                    "-row-mt",
                    "1",
                    "-deadline",
                    "good",
                    "-cpu-used",
                    cpu_used,
                ],
            );
            if q.ten_bit {
                // 10-bit 4:2:0 VP9 is Profile 2.
                push(&mut args, &["-profile:v", "2"]);
            }
        }
        Family::Nvenc => {
            // VBR with a CQ target and no bitrate cap is NVENC's constant
            // quality mode. p4 is NVIDIA's default preset; p7 the slowest.
            let preset = match q.speed {
                SpeedPreset::Fast => "p4",
                SpeedPreset::Balanced => "p5",
                SpeedPreset::Thorough => "p7",
            };
            push(
                &mut args,
                &[
                    "-rc",
                    "vbr",
                    "-cq",
                    &v,
                    "-b:v",
                    "0",
                    "-preset",
                    preset,
                    "-tune",
                    "hq",
                    "-spatial-aq",
                    "1",
                ],
            );
            push_main10(&mut args, q);
        }
        Family::Qsv => {
            // ICQ (global_quality without a bitrate). Look-ahead is not
            // enabled: LA-ICQ exists only for H.264 on the older
            // (non low-power) encoder and fails outright on Arc and newer
            // iGPUs, which would waste the attempt.
            let preset = match q.speed {
                SpeedPreset::Fast => "veryfast",
                SpeedPreset::Balanced => "medium",
                SpeedPreset::Thorough => "slower",
            };
            push(&mut args, &["-global_quality:v", &v, "-preset", preset]);
            if q.ten_bit && q.codec == VideoCodec::Vp9 {
                push(&mut args, &["-profile:v", "profile2"]);
            } else {
                push_main10(&mut args, q);
            }
        }
        Family::Vaapi => {
            // Constant QP works on every VA-API driver (Intel iHD and AMD
            // Mesa); ICQ is Intel-only. The QP comes from global_quality,
            // which all four VA-API encoders read (`-qp` exists only for
            // H.264/HEVC). VA-API has no portable speed preset, so
            // `SpeedPreset` is not mapped.
            push(&mut args, &["-rc_mode", "CQP", "-global_quality:v", &v]);
            push_main10(&mut args, q);
        }
        Family::VideoToolbox => {
            // Constant quality (Apple silicon). allow_sw 0 refuses the slow
            // software fallback; realtime 0 lets the encoder take its time.
            // VideoToolbox has no speed preset.
            push(&mut args, &["-q:v", &v, "-allow_sw", "0", "-realtime", "0"]);
            push_main10(&mut args, q);
        }
        Family::Amf => {
            // Constant QP; `-quality` is AMF's speed/quality preset. The
            // 10-bit profile follows the p010 input automatically.
            let quality = match q.speed {
                SpeedPreset::Fast => "speed",
                SpeedPreset::Balanced => "balanced",
                SpeedPreset::Thorough => "quality",
            };
            push(
                &mut args,
                &["-rc", "cqp", "-qp_i", &v, "-qp_p", &v, "-quality", quality],
            );
        }
        Family::Rkmpp => {
            // Constant QP. MPP has no speed preset.
            push(&mut args, &["-rc_mode", "CQP", "-qp_init", &v]);
        }
        Family::V4l2m2m => {
            // The ladder is kbit/s at 1080p, scaled to the output size. A
            // quantizer-style override never applies here (see module docs).
            let kbps = scaled_bitrate(value.unwrap_or(4_000), q.width, q.height);
            push(&mut args, &["-b:v", &format!("{kbps}k")]);
        }
        Family::OtherSoftware => {}
    }
    args
}

/// Append string slices as owned arguments.
fn push(args: &mut Vec<String>, items: &[&str]) {
    args.extend(items.iter().map(|s| (*s).to_string()));
}

/// `-profile:v main10` for 10-bit HEVC on hardware encoders that do not pick
/// it from the input format by themselves.
fn push_main10(args: &mut Vec<String>, q: &VideoQuality<'_>) {
    if q.ten_bit && q.codec == VideoCodec::Hevc {
        push(args, &["-profile:v", "main10"]);
    }
}

/// Scale a 1080p bitrate to another picture size. Bitrate grows slower than
/// pixel count (larger pictures compress better), so the area ratio is raised
/// to the power 0.75.
fn scaled_bitrate(kbps_1080p: u32, width: u32, height: u32) -> u32 {
    const REFERENCE_AREA: f64 = 1920.0 * 1080.0;
    let area = f64::from(width.max(16)) * f64::from(height.max(16));
    let factor = (area / REFERENCE_AREA).powf(0.75);
    let scaled = (f64::from(kbps_1080p) * factor).round();
    // The float is finite and bounded by the clamp, so the cast is exact.
    scaled.clamp(300.0, 60_000.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q<'a>(encoder: &'a str, api: HwApi, codec: VideoCodec) -> VideoQuality<'a> {
        VideoQuality {
            encoder,
            api,
            codec,
            quality: QualityLevel::Balanced,
            speed: SpeedPreset::Balanced,
            quality_override: None,
            ten_bit: false,
            width: 1920,
            height: 1080,
        }
    }

    fn value_after(args: &[String], flag: &str) -> Option<String> {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    }

    #[test]
    fn every_registered_encoder_has_a_scale_and_args() {
        for enc in szalinski_core::VIDEO_ENCODERS {
            let scale = quality_scale(enc.name, enc.api, enc.codec)
                .unwrap_or_else(|| panic!("{} has no quality scale", enc.name));
            for level in scale.levels {
                assert!((scale.min..=scale.max).contains(&level), "{}", enc.name);
            }
            // Ladders move monotonically towards quality.
            let w = scale.levels.windows(2);
            if scale.higher_is_better {
                assert!(w.clone().all(|p| p[0] < p[1]), "{}", enc.name);
            } else {
                assert!(w.clone().all(|p| p[0] > p[1]), "{}", enc.name);
            }
            let args = video_quality_args(&q(enc.name, enc.api, enc.codec));
            assert!(!args.is_empty(), "{} produced no args", enc.name);
            assert!(
                args.len().is_multiple_of(2),
                "{} args are not flag/value pairs: {args:?}",
                enc.name
            );
        }
    }

    #[test]
    fn software_ladders_match_the_documented_values() {
        let crf = |enc, codec, level| {
            let mut req = q(enc, HwApi::Software, codec);
            req.quality = level;
            value_after(&video_quality_args(&req), "-crf").unwrap()
        };
        assert_eq!(
            crf("libsvtav1", VideoCodec::Av1, QualityLevel::Smallest),
            "40"
        );
        assert_eq!(crf("libsvtav1", VideoCodec::Av1, QualityLevel::Best), "24");
        assert_eq!(
            crf("libx265", VideoCodec::Hevc, QualityLevel::Balanced),
            "24"
        );
        assert_eq!(crf("libx264", VideoCodec::H264, QualityLevel::High), "20");
        assert_eq!(
            crf("libvpx-vp9", VideoCodec::Vp9, QualityLevel::Small),
            "36"
        );
        assert_eq!(
            crf("libaom-av1", VideoCodec::Av1, QualityLevel::Balanced),
            "32"
        );
    }

    #[test]
    fn speed_presets_map_per_encoder() {
        let arg = |enc, api, codec, speed, flag| {
            let mut req = q(enc, api, codec);
            req.speed = speed;
            value_after(&video_quality_args(&req), flag).unwrap()
        };
        use HwApi::*;
        use SpeedPreset::*;
        assert_eq!(
            arg("libsvtav1", Software, VideoCodec::Av1, Fast, "-preset"),
            "8"
        );
        assert_eq!(
            arg("libsvtav1", Software, VideoCodec::Av1, Thorough, "-preset"),
            "4"
        );
        assert_eq!(
            arg(
                "libaom-av1",
                Software,
                VideoCodec::Av1,
                Balanced,
                "-cpu-used"
            ),
            "6"
        );
        assert_eq!(
            arg("libx265", Software, VideoCodec::Hevc, Thorough, "-preset"),
            "slow"
        );
        assert_eq!(
            arg("libx264", Software, VideoCodec::H264, Fast, "-preset"),
            "fast"
        );
        assert_eq!(
            arg("libvpx-vp9", Software, VideoCodec::Vp9, Fast, "-cpu-used"),
            "4"
        );
        assert_eq!(
            arg("hevc_nvenc", Nvenc, VideoCodec::Hevc, Thorough, "-preset"),
            "p7"
        );
        assert_eq!(
            arg("av1_qsv", Qsv, VideoCodec::Av1, Fast, "-preset"),
            "veryfast"
        );
        assert_eq!(
            arg("h264_amf", Amf, VideoCodec::H264, Thorough, "-quality"),
            "quality"
        );
    }

    #[test]
    fn override_replaces_the_value_within_the_scale() {
        let mut req = q("libx265", HwApi::Software, VideoCodec::Hevc);
        req.quality_override = Some(19);
        assert_eq!(
            value_after(&video_quality_args(&req), "-crf").unwrap(),
            "19"
        );
        assert!(!override_ignored(&req));
        // Above the maximum: meant for another encoder, so the ladder is
        // used instead of the worst possible CRF.
        req.quality_override = Some(60);
        assert_eq!(
            value_after(&video_quality_args(&req), "-crf").unwrap(),
            "24"
        );
        assert!(override_ignored(&req));
        // The same value is fine for a 0-63 encoder.
        let mut svt = q("libsvtav1", HwApi::Software, VideoCodec::Av1);
        svt.quality_override = Some(60);
        assert_eq!(
            value_after(&video_quality_args(&svt), "-crf").unwrap(),
            "60"
        );

        // Below the minimum: raised to it.
        let mut nv = q("hevc_nvenc", HwApi::Nvenc, VideoCodec::Hevc);
        nv.quality_override = Some(0);
        assert_eq!(value_after(&video_quality_args(&nv), "-cq").unwrap(), "1");
        assert!(!override_ignored(&nv));
    }

    #[test]
    fn override_is_ignored_by_scales_of_another_kind() {
        // A CRF-style 24 would be a terrible VideoToolbox quality (higher is
        // better there) and a 24 kbit/s V4L2 bitrate.
        let mut vt = q("hevc_videotoolbox", HwApi::VideoToolbox, VideoCodec::Hevc);
        vt.quality_override = Some(24);
        assert_eq!(value_after(&video_quality_args(&vt), "-q:v").unwrap(), "60");
        assert!(override_ignored(&vt));

        let mut v4l2 = q("h264_v4l2m2m", HwApi::V4l2m2m, VideoCodec::H264);
        v4l2.quality_override = Some(24);
        assert_eq!(
            value_after(&video_quality_args(&v4l2), "-b:v").as_deref(),
            Some("6500k")
        );
        assert!(override_ignored(&v4l2));

        let plain = q("libx264", HwApi::Software, VideoCodec::H264);
        assert!(!override_ignored(&plain));
        let mut unknown = q("librav1e", HwApi::Software, VideoCodec::Av1);
        unknown.quality_override = Some(30);
        assert!(override_ignored(&unknown));
    }

    #[test]
    fn hardware_option_vocabularies() {
        let nv = video_quality_args(&q("av1_nvenc", HwApi::Nvenc, VideoCodec::Av1));
        assert_eq!(value_after(&nv, "-rc").as_deref(), Some("vbr"));
        assert_eq!(value_after(&nv, "-b:v").as_deref(), Some("0"));
        assert_eq!(value_after(&nv, "-tune").as_deref(), Some("hq"));
        assert_eq!(value_after(&nv, "-spatial-aq").as_deref(), Some("1"));

        let qsv = video_quality_args(&q("hevc_qsv", HwApi::Qsv, VideoCodec::Hevc));
        assert_eq!(
            value_after(&qsv, "-global_quality:v").as_deref(),
            Some("24")
        );
        assert!(
            !qsv.iter().any(|a| a == "-global_quality"),
            "must be scoped to video"
        );

        let va = video_quality_args(&q("hevc_vaapi", HwApi::Vaapi, VideoCodec::Hevc));
        assert_eq!(value_after(&va, "-rc_mode").as_deref(), Some("CQP"));
        assert_eq!(value_after(&va, "-global_quality:v").as_deref(), Some("24"));

        let va_av1 = video_quality_args(&q("av1_vaapi", HwApi::Vaapi, VideoCodec::Av1));
        assert_eq!(
            value_after(&va_av1, "-global_quality:v").as_deref(),
            Some("120")
        );

        let vt = video_quality_args(&q(
            "h264_videotoolbox",
            HwApi::VideoToolbox,
            VideoCodec::H264,
        ));
        assert_eq!(value_after(&vt, "-q:v").as_deref(), Some("65"));
        assert_eq!(value_after(&vt, "-allow_sw").as_deref(), Some("0"));
        assert_eq!(value_after(&vt, "-realtime").as_deref(), Some("0"));

        let amf = video_quality_args(&q("av1_amf", HwApi::Amf, VideoCodec::Av1));
        assert_eq!(value_after(&amf, "-rc").as_deref(), Some("cqp"));
        assert_eq!(value_after(&amf, "-qp_i").as_deref(), Some("120"));
        assert_eq!(value_after(&amf, "-qp_p").as_deref(), Some("120"));

        let rk = video_quality_args(&q("hevc_rkmpp", HwApi::Rkmpp, VideoCodec::Hevc));
        assert_eq!(value_after(&rk, "-rc_mode").as_deref(), Some("CQP"));
        assert_eq!(value_after(&rk, "-qp_init").as_deref(), Some("24"));
    }

    #[test]
    fn v4l2_bitrate_follows_resolution() {
        let mut req = q("h264_v4l2m2m", HwApi::V4l2m2m, VideoCodec::H264);
        assert_eq!(
            value_after(&video_quality_args(&req), "-b:v").as_deref(),
            Some("6500k")
        );
        req.width = 1280;
        req.height = 720;
        let small: u32 = value_after(&video_quality_args(&req), "-b:v")
            .unwrap()
            .trim_end_matches('k')
            .parse()
            .unwrap();
        assert!((3_000..4_000).contains(&small), "720p bitrate {small}");
    }

    #[test]
    fn ten_bit_profiles() {
        let mut req = q("libvpx-vp9", HwApi::Software, VideoCodec::Vp9);
        req.ten_bit = true;
        assert_eq!(
            value_after(&video_quality_args(&req), "-profile:v").as_deref(),
            Some("2")
        );

        for (enc, api) in [
            ("hevc_nvenc", HwApi::Nvenc),
            ("hevc_qsv", HwApi::Qsv),
            ("hevc_vaapi", HwApi::Vaapi),
            ("hevc_videotoolbox", HwApi::VideoToolbox),
        ] {
            let mut req = q(enc, api, VideoCodec::Hevc);
            req.ten_bit = true;
            assert_eq!(
                value_after(&video_quality_args(&req), "-profile:v").as_deref(),
                Some("main10"),
                "{enc}"
            );
            req.ten_bit = false;
            assert!(
                !video_quality_args(&req).iter().any(|a| a == "-profile:v"),
                "{enc}"
            );
        }

        let mut vp9 = q("vp9_qsv", HwApi::Qsv, VideoCodec::Vp9);
        vp9.ten_bit = true;
        assert_eq!(
            value_after(&video_quality_args(&vp9), "-profile:v").as_deref(),
            Some("profile2")
        );
    }

    #[test]
    fn unknown_software_encoder_uses_its_defaults() {
        assert!(quality_scale("librav1e", HwApi::Software, VideoCodec::Av1).is_none());
        assert!(video_quality_args(&q("librav1e", HwApi::Software, VideoCodec::Av1)).is_empty());
    }
}
