//! Skip decisions and ffmpeg argument construction. Pure functions only.
//!
//! [`decide`] answers "does this file need work under this profile?" and
//! [`build_plan`] turns a probed file, a profile and one encoder candidate
//! into the exact ffmpeg arguments for one attempt. Neither touches the disk
//! or spawns processes, so the same inputs always produce the same command
//! (which is what the UI shows in a job's detail sheet).
//!
//! The rules follow "Worker behaviour (normative)" in `docs/ARCHITECTURE.md`.
//! Where ffmpeg needs more care than the contract spells out (audio channel
//! layouts, sample rates, hardware frame formats), the reason is written next
//! to the rule.

use std::path::Path;

use anyhow::{anyhow, bail};
use chrysopoeia_core::codec::{is_image_subtitle, source_efficiency_rank};
use chrysopoeia_core::{
    AudioCodec, Container, EncoderCandidate, HdrFormat, HwApi, ProbeInfo, StreamInfo, StreamKind,
    SubtitleAction, SubtitlePolicy, TranscodeProfile, VideoCodec,
};

use crate::quality::{VideoQuality, video_quality_args};

/// Files shorter than this are not worth converting (and too short to verify).
const MIN_DURATION_SECS: f64 = 1.0;

/// Render node used for VA-API and Quick Sync when detection named none.
pub const DEFAULT_RENDER_NODE: &str = "/dev/dri/renderD128";

/// Whether a file needs work under a profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The file should be converted.
    Transcode,
    /// No work needed. `reason` is a short sentence for the UI, e.g.
    /// "Already AV1, which is more efficient than HEVC".
    Skip {
        /// Why no work is needed, in plain language.
        reason: String,
    },
}

fn skip(reason: impl Into<String>) -> Decision {
    Decision::Skip {
        reason: reason.into(),
    }
}

/// Decide whether `probe` needs transcoding under `profile`.
///
/// Rules, in order:
/// 1. No real video stream (audio only, or cover art only): skip.
/// 2. Unknown or zero picture size, unknown duration, or shorter than one
///    second: skip (such files cannot be converted and verified reliably).
/// 3. With `skip_efficient`: skip when the source codec's efficiency rank is
///    at least the target's ("Already AV1, which is more efficient than
///    HEVC"). Without it: skip only when the video is already the target
///    codec in the target container.
///
///    Container matching uses ffprobe's demuxer name, which cannot tell MKV
///    from WebM (both report `matroska`) or MP4 from MOV/M4V (all report
///    `mov`). So an MKV or WebM target matches any Matroska-family file, and
///    an MP4 target matches any QuickTime-family file.
///
///    Neither rule applies when the video is taller than the profile's
///    `max_height`: the user asked for smaller pictures, so the file is
///    converted even if its codec is already efficient.
/// 4. H.264 target with an HDR source: skip. There is no tone mapping, and
///    8-bit H.264 cannot carry HDR, so the colours would be wrong.
pub fn decide(probe: &ProbeInfo, profile: &TranscodeProfile) -> Decision {
    let Some(video) = probe.primary_video() else {
        return if probe.audio_streams().next().is_some() {
            skip("Audio-only file — nothing to convert")
        } else {
            skip("No video in this file — nothing to convert")
        };
    };
    let height = match (video.width, video.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => h,
        _ => return skip("Couldn't read this video's picture size — left unchanged"),
    };
    match probe.duration_secs {
        Some(d) if d.is_finite() && d >= MIN_DURATION_SECS => {}
        Some(d) if d.is_finite() && d >= 0.0 => {
            return skip("Shorter than a second — nothing worth converting");
        }
        _ => return skip("Couldn't tell how long this video is — left unchanged"),
    }

    let target = profile.video_codec;
    let too_tall = max_output_height(profile).is_some_and(|max| height > max);
    if !too_tall {
        if profile.skip_efficient {
            let rank = source_efficiency_rank(&video.codec);
            if rank >= target.efficiency_rank() {
                return skip(efficient_reason(&video.codec, target, rank));
            }
        } else if VideoCodec::from_probe_name(&video.codec) == Some(target)
            && container_matches(&probe.container, profile.container)
        {
            return skip(format!(
                "Already {} in {}",
                codec_short_name(target),
                profile.container.label()
            ));
        }
    }

    if target == VideoCodec::H264 && is_hdr(video) {
        return skip("HDR video would lose its colours as H.264 — left unchanged");
    }
    Decision::Transcode
}

/// Skip reason for the `skip_efficient` rule.
fn efficient_reason(source: &str, target: VideoCodec, rank: u8) -> String {
    let name = source_codec_name(source);
    let target_name = codec_short_name(target);
    if VideoCodec::from_probe_name(source) == Some(target) {
        format!("Already {name}")
    } else if rank > target.efficiency_rank() {
        format!("Already {name}, which is more efficient than {target_name}")
    } else {
        format!("Already {name}, which is as efficient as {target_name}")
    }
}

/// Whether ffprobe's container name (first token of `format_name`) belongs
/// to the same family as `target`. See [`decide`] for why this is a family
/// match rather than an exact one.
fn container_matches(probe_container: &str, target: Container) -> bool {
    let name = probe_container
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    match target {
        Container::Mkv | Container::Webm => name == "matroska" || name == "webm",
        Container::Mp4 => name == "mov" || name == "mp4",
    }
}

/// HDR signalling: the scanner's classification, or a PQ/HLG transfer.
fn is_hdr(video: &StreamInfo) -> bool {
    video.hdr.is_some() || is_pq(video) || video.color_transfer.as_deref() == Some("arib-std-b67")
}

/// PQ (SMPTE 2084) transfer, i.e. HDR10-style video.
fn is_pq(video: &StreamInfo) -> bool {
    matches!(
        video.hdr,
        Some(HdrFormat::Hdr10 | HdrFormat::Hdr10Plus | HdrFormat::DolbyVision)
    ) || video.color_transfer.as_deref() == Some("smpte2084")
}

/// The profile's height limit, rounded down to an even number. Values below
/// 2 mean "no limit".
fn max_output_height(profile: &TranscodeProfile) -> Option<u32> {
    profile.max_height.filter(|&h| h >= 2).map(|h| h & !1)
}

/// Inputs for [`build_plan`].
#[derive(Debug, Clone, Copy)]
pub struct PlanRequest<'a> {
    /// The source file.
    pub input: &'a Path,
    /// Where ffmpeg writes (a temp path; the extension matches the container).
    pub output: &'a Path,
    /// ffprobe results for `input`.
    pub probe: &'a ProbeInfo,
    /// The library's (normalized) profile.
    pub profile: &'a TranscodeProfile,
    /// The encoder for this attempt; `hw_decode` asks for GPU decoding.
    pub encoder: &'a EncoderCandidate,
}

/// Number of streams of each kind the output will contain.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StreamSummary {
    /// Video streams (always 1: the primary video).
    pub video: u32,
    /// Audio streams kept.
    pub audio: u32,
    /// Subtitle streams kept.
    pub subtitle: u32,
}

/// A ready-to-run ffmpeg invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfmpegPlan {
    /// Arguments after the `ffmpeg` binary. Always includes `-y`,
    /// `-progress pipe:1 -nostats`, explicit `-map`s and `-f <muxer>`.
    pub args: Vec<String>,
    /// Plain-language notes about compromises, e.g. "Removed 2 picture-based
    /// subtitles because MP4 can't hold them".
    pub notes: Vec<String>,
    /// What the output should contain; used by verification.
    pub expected: StreamSummary,
}

/// Build the ffmpeg command for one attempt.
///
/// Argument order: global options, hardware devices and decode options,
/// input options, `-i <input>`, `-map`s, metadata, video, audio, subtitle
/// and attachment options, muxer options, `-f <muxer> <output>`.
///
/// Fails (with a plain sentence) when the file has no video, when the
/// encoder does not produce the profile's codec, when the container cannot
/// hold that codec, or when a path cannot be passed to ffmpeg.
pub fn build_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let profile = req.profile;
    let encoder = req.encoder;
    let container = profile.container;

    let video = req
        .probe
        .primary_video()
        .ok_or_else(|| anyhow!("This file has no video stream to convert."))?;
    if encoder.codec != profile.video_codec {
        bail!(
            "The {} encoder makes {} video, but this library is set to {}.",
            encoder.name,
            encoder.codec.label(),
            profile.video_codec.label()
        );
    }
    if !container.supports_video(encoder.codec) {
        bail!(
            "{} files can't hold {} video. Choose MKV or MP4 for this library.",
            container.label(),
            encoder.codec.label()
        );
    }
    let input = path_arg(req.input)?;
    let output = path_arg(req.output)?;

    let mut notes = Vec::new();
    let video_plan = plan_video(video, profile, encoder, &mut notes);
    let audio = plan_audio(req.probe, profile, &mut notes);
    let subtitles = plan_subtitles(req.probe, profile, &mut notes);
    let attachments: Vec<u32> = if container.supports_attachments() {
        req.probe
            .streams
            .iter()
            .filter(|s| s.kind == Some(StreamKind::Attachment))
            .map(|s| s.index)
            .collect()
    } else {
        Vec::new()
    };

    let mut args: Vec<String> = Vec::with_capacity(96);
    push(
        &mut args,
        &[
            "-hide_banner",
            "-nostdin",
            "-y",
            "-loglevel",
            "warning",
            "-nostats",
            "-progress",
            "pipe:1",
        ],
    );
    args.extend(video_plan.input_args);
    push(
        &mut args,
        &["-analyzeduration", "100M", "-probesize", "100M"],
    );
    if needs_genpts(&req.probe.container) {
        push(&mut args, &["-fflags", "+genpts"]);
    }
    args.push("-i".into());
    args.push(input);

    let mapped = std::iter::once(video.index)
        .chain(audio.tracks.iter().map(|t| t.index))
        .chain(subtitles.iter().map(|s| s.index))
        .chain(attachments.iter().copied());
    for index in mapped {
        args.push("-map".into());
        args.push(format!("0:{index}"));
    }
    push(&mut args, &["-map_metadata", "0", "-map_chapters", "0"]);

    args.extend(video_plan.output_args);
    args.extend(audio.args);
    for (n, sub) in subtitles.iter().enumerate() {
        args.push(format!("-c:s:{n}"));
        args.push(sub.codec.to_string());
    }
    if !attachments.is_empty() {
        push(&mut args, &["-c:t", "copy"]);
    }
    push(&mut args, &["-max_muxing_queue_size", "9999"]);
    if container == Container::Mp4 {
        push(&mut args, &["-movflags", "+faststart"]);
    }
    push(&mut args, &["-f", container.ffmpeg_format()]);
    args.push(output);

    // Several tracks can earn the same note (e.g. two downmixes).
    let mut unique_notes: Vec<String> = Vec::with_capacity(notes.len());
    for note in notes {
        if !unique_notes.contains(&note) {
            unique_notes.push(note);
        }
    }

    Ok(FfmpegPlan {
        args,
        notes: unique_notes,
        expected: StreamSummary {
            video: 1,
            audio: count(audio.tracks.len()),
            subtitle: count(subtitles.len()),
        },
    })
}

/// Options that create the hardware device an encoder (and its upload or
/// scaling filters) needs. Empty for APIs that take system-memory frames.
///
/// Exposed so hardware detection can test-encode with exactly the device
/// setup real jobs use.
pub fn hw_device_args(api: HwApi, device: Option<&str>) -> Vec<String> {
    let node = device
        .map(str::trim)
        .filter(|d| !d.is_empty())
        .unwrap_or(DEFAULT_RENDER_NODE);
    let va = format!("vaapi=va:{node}");
    match api {
        HwApi::Vaapi => strings(&["-init_hw_device", &va, "-filter_hw_device", "va"]),
        // QSV on Linux is initialised through VA-API and derived from it.
        HwApi::Qsv => strings(&[
            "-init_hw_device",
            &va,
            "-init_hw_device",
            "qsv=qs@va",
            "-filter_hw_device",
            "qs",
        ]),
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Video

/// Where decoded frames live when they reach the filter chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Frames {
    /// System memory: software decode, or VideoToolbox (which copies back).
    System,
    /// Stays on the GPU in this API's surface format.
    Gpu(GpuFrames),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GpuFrames {
    Cuda,
    Vaapi,
    Qsv,
}

struct VideoPlan {
    /// Options before `-i`: hardware devices, hwaccel and decoder choice.
    input_args: Vec<String>,
    /// Encoder, quality, pixel format, filters, colour and tag options.
    output_args: Vec<String>,
}

fn plan_video(
    video: &StreamInfo,
    profile: &TranscodeProfile,
    encoder: &EncoderCandidate,
    notes: &mut Vec<String>,
) -> VideoPlan {
    let api = encoder.api;
    let codec = encoder.codec;
    let depth = source_bit_depth(video);
    let ten_bit = depth > 8 && codec.supports_10bit() && writes_10bit(api, codec);
    if depth > 8 && !ten_bit {
        let why = if codec.supports_10bit() {
            format!(
                "{} can only write 8-bit {}",
                api.label(),
                codec_short_name(codec)
            )
        } else {
            format!(
                "{} is written as 8-bit so it plays everywhere",
                codec_short_name(codec)
            )
        };
        notes.push(format!(
            "Reduced the {depth}-bit picture to 8-bit because {why}"
        ));
    }
    match video.hdr {
        Some(HdrFormat::DolbyVision) => notes.push(
            "Dolby Vision layers can't be kept, so the video keeps its standard HDR10 picture"
                .into(),
        ),
        Some(HdrFormat::Hdr10Plus) => notes.push(
            "HDR10+ scene data can't be kept, so the video keeps its standard HDR10 picture".into(),
        ),
        _ => {}
    }

    let width = video.width.unwrap_or(0);
    let height = video.height.unwrap_or(0);
    let odd = width % 2 == 1 || height % 2 == 1;
    let target_height = max_output_height(profile).filter(|&max| height > max);

    // GPU decoding is kept only when the frames can stay on the GPU all the
    // way to the encoder; otherwise this attempt decodes on the CPU.
    let hw_decode = if encoder.hw_decode && has_hw_decode(api) {
        match cpu_decode_reason(video, api, depth, odd) {
            Some(reason) => {
                notes.push(format!("Decoded on the CPU because {reason}"));
                false
            }
            None => true,
        }
    } else {
        false
    };
    let frames = match (hw_decode, api) {
        (true, HwApi::Nvenc) => Frames::Gpu(GpuFrames::Cuda),
        (true, HwApi::Vaapi) => Frames::Gpu(GpuFrames::Vaapi),
        (true, HwApi::Qsv) => Frames::Gpu(GpuFrames::Qsv),
        _ => Frames::System,
    };

    let mut input_args = hw_device_args(api, encoder.device.as_deref());
    if hw_decode {
        input_args.extend(hw_decode_args(api, &video.codec));
    }

    let sw_format = if ten_bit { "yuv420p10le" } else { "yuv420p" };
    let hw_format = if ten_bit { "p010le" } else { "nv12" };
    let mut filters: Vec<String> = Vec::new();
    let mut pix_fmt = None;
    let (mut out_width, mut out_height) = (width & !1, height & !1);
    match frames {
        Frames::System => {
            if video.interlaced {
                // One frame per frame (not per field) keeps the frame rate.
                filters.push("bwdif=mode=send_frame".into());
            }
            if let Some(h) = target_height {
                filters.push(format!("scale=-2:{h}:flags=lanczos"));
            } else if odd {
                // 4:2:0 encoders need even sizes. Scaling by one pixel (rather
                // than cropping) keeps the picture aligned with the source,
                // which is what verification compares against.
                filters.push("scale=trunc(iw/2)*2:trunc(ih/2)*2:flags=lanczos".into());
                notes.push(format!(
                    "Resized from {width}×{height} to {out_width}×{out_height} because video encoders need even picture sizes"
                ));
            }
            match api {
                HwApi::Software => pix_fmt = Some(sw_format),
                HwApi::Vaapi => filters.push(format!("format={hw_format},hwupload")),
                HwApi::Qsv => filters.push(format!(
                    "format={hw_format},hwupload=extra_hw_frames=64,format=qsv"
                )),
                // NVENC, VideoToolbox, AMF, MPP and V4L2 take system frames.
                _ => pix_fmt = Some(hw_format),
            }
        }
        Frames::Gpu(gpu) => {
            // Frames arrive in the decoder's surface format. Convert on the
            // GPU when resizing or when the bit depth must drop (e.g. 10-bit
            // HEVC to 8-bit H.264); otherwise pass them straight through.
            if target_height.is_some() || (depth > 8 && !ten_bit) {
                filters.push(gpu_scale_filter(gpu, target_height, hw_format));
            }
        }
    }
    if let Some(h) = target_height {
        out_width = even_width_for(width, height, h);
        out_height = h;
    }

    let mut out = strings(&["-c:v", &encoder.name]);
    out.extend(video_quality_args(&VideoQuality {
        encoder: &encoder.name,
        api,
        codec,
        quality: profile.quality,
        speed: profile.speed,
        quality_override: profile.quality_override,
        ten_bit,
        width: out_width,
        height: out_height,
    }));
    if let Some(fmt) = pix_fmt {
        push(&mut out, &["-pix_fmt", fmt]);
    }
    if !filters.is_empty() {
        out.push("-vf".into());
        out.push(filters.join(","));
    }
    out.extend(color_args(video));
    if encoder.name == "libx265" {
        let mut params = String::from("log-level=error");
        if ten_bit && is_pq(video) {
            // Write HDR10 SEI/VUI on every keyframe and optimise for PQ.
            params.push_str(":hdr-opt=1:repeat-headers=1");
        }
        push(&mut out, &["-x265-params", &params]);
    }
    if profile.container == Container::Mp4 && codec == VideoCodec::Hevc {
        // Apple devices only play HEVC in MP4 when tagged hvc1.
        push(&mut out, &["-tag:v", "hvc1"]);
    }
    if is_matroska(profile.container) {
        out.extend(clear_statistics_tags("v:0"));
    }

    VideoPlan {
        input_args,
        output_args: out,
    }
}

/// Whether an API can write 10-bit output for a codec (H.264 never is).
/// Rockchip MPP and V4L2 encoders are 8-bit only.
fn writes_10bit(api: HwApi, codec: VideoCodec) -> bool {
    match api {
        HwApi::Software | HwApi::Nvenc | HwApi::Qsv | HwApi::Vaapi | HwApi::Amf => {
            codec != VideoCodec::H264
        }
        HwApi::VideoToolbox => codec == VideoCodec::Hevc,
        HwApi::Rkmpp | HwApi::V4l2m2m => false,
    }
}

/// APIs whose GPU decoding the planner knows how to set up. AMF, MPP and
/// V4L2 encoders are fed system-memory frames.
fn has_hw_decode(api: HwApi) -> bool {
    matches!(
        api,
        HwApi::Nvenc | HwApi::Vaapi | HwApi::Qsv | HwApi::VideoToolbox
    )
}

/// Why this attempt must decode on the CPU even though GPU decoding was
/// requested, or `None` when GPU decoding can be used. The reason completes
/// "Decoded on the CPU because …".
fn cpu_decode_reason(video: &StreamInfo, api: HwApi, depth: u8, odd: bool) -> Option<String> {
    if video.interlaced {
        return Some("interlaced video is deinterlaced on the CPU".into());
    }
    if odd {
        return Some("the picture has an odd size that is fixed on the CPU".into());
    }
    if api == HwApi::VideoToolbox {
        // VideoToolbox falls back to software decoding by itself and always
        // hands back system-memory frames.
        return None;
    }
    let codec = video.codec.to_ascii_lowercase();
    let name = source_codec_name(&codec);
    let known = match api {
        HwApi::Nvenc => matches!(
            codec.as_str(),
            "h264" | "hevc" | "av1" | "vp9" | "vp8" | "mpeg1video" | "mpeg2video" | "vc1"
        ),
        HwApi::Vaapi => matches!(
            codec.as_str(),
            "h264" | "hevc" | "av1" | "vp9" | "vp8" | "mpeg2video"
        ),
        HwApi::Qsv => qsv_decoder(&codec).is_some(),
        _ => false,
    };
    if !known {
        return Some(format!("the GPU can't decode {name} video"));
    }
    if depth > 10 || (codec == "h264" && depth > 8) {
        return Some(format!("the GPU can't decode {depth}-bit {name}"));
    }
    if !is_420(video) {
        let chroma = chroma_label(video.pix_fmt.as_deref().unwrap_or_default());
        return Some(format!("the GPU can't decode {chroma} {name}"));
    }
    None
}

/// GPU decode options (before `-i`). Only called when [`cpu_decode_reason`]
/// returned `None`.
fn hw_decode_args(api: HwApi, source_codec: &str) -> Vec<String> {
    match api {
        HwApi::Nvenc => strings(&["-hwaccel", "cuda", "-hwaccel_output_format", "cuda"]),
        HwApi::Vaapi => strings(&[
            "-hwaccel",
            "vaapi",
            "-hwaccel_output_format",
            "vaapi",
            "-hwaccel_device",
            "va",
        ]),
        HwApi::Qsv => {
            let mut args = strings(&["-hwaccel", "qsv", "-hwaccel_output_format", "qsv"]);
            // ffmpeg's built-in decoders have no QSV hwaccel; the dedicated
            // *_qsv decoder has to be named or ffmpeg silently decodes on the
            // CPU and the GPU-side filters then fail.
            if let Some(decoder) = qsv_decoder(&source_codec.to_ascii_lowercase()) {
                push(&mut args, &["-c:v", decoder]);
            }
            args
        }
        HwApi::VideoToolbox => strings(&["-hwaccel", "videotoolbox"]),
        _ => Vec::new(),
    }
}

/// The Quick Sync decoder for a source codec.
fn qsv_decoder(codec: &str) -> Option<&'static str> {
    match codec {
        "h264" => Some("h264_qsv"),
        "hevc" => Some("hevc_qsv"),
        "av1" => Some("av1_qsv"),
        "vp9" => Some("vp9_qsv"),
        "mpeg2video" => Some("mpeg2_qsv"),
        _ => None,
    }
}

/// Resize and/or convert frames that are already on the GPU.
fn gpu_scale_filter(gpu: GpuFrames, height: Option<u32>, format: &str) -> String {
    let (name, auto_width) = match gpu {
        GpuFrames::Cuda => ("scale_cuda", "-2"),
        GpuFrames::Vaapi => ("scale_vaapi", "-2"),
        // scale_qsv only understands -1 ("keep aspect"); QSV aligns the
        // surface itself.
        GpuFrames::Qsv => ("scale_qsv", "-1"),
    };
    match height {
        Some(h) => format!("{name}=w={auto_width}:h={h}:format={format}"),
        None => format!("{name}=format={format}"),
    }
}

/// Output width for a downscale to `target` lines, computed exactly like
/// ffmpeg's `scale=-2:H`: `round(H * width / (height * 2)) * 2`.
fn even_width_for(width: u32, height: u32, target: u32) -> u32 {
    if width == 0 || height == 0 {
        return 0;
    }
    let (w, h, t) = (u64::from(width), u64::from(height), u64::from(target));
    let halves = (t * w + h) / (2 * h);
    u32::try_from(halves * 2).unwrap_or(u32::MAX - 1)
}

/// Bit depth of the source picture: the larger of ffprobe's
/// `bits_per_raw_sample` and what the pixel format name says (ffprobe often
/// leaves the former empty, e.g. for 10-bit HEVC in Matroska).
fn source_bit_depth(video: &StreamInfo) -> u8 {
    let from_format = video.pix_fmt.as_deref().map_or(8, pix_fmt_depth);
    let from_probe = video
        .bit_depth
        .filter(|d| (1..=16).contains(d))
        .unwrap_or(8);
    from_format.max(from_probe).clamp(8, 16)
}

/// Bit depth encoded in a pixel format name: `yuv420p10le` → 10,
/// `p010le` → 10, `gray12le` → 12, `yuv420p`/`nv12` → 8.
fn pix_fmt_depth(fmt: &str) -> u8 {
    let lower = fmt.to_ascii_lowercase();
    let base = lower
        .strip_suffix("le")
        .or_else(|| lower.strip_suffix("be"))
        .unwrap_or(&lower);
    // Trimming ASCII digits keeps a valid char boundary whatever the input.
    let head = base.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &base[head.len()..];
    let depth: u32 = match digits.len() {
        // Semi-planar/packed names carry three digits: p010, p016, y210, p410.
        3 if head == "p" || head == "y" => digits[1..].parse().unwrap_or(8),
        1 | 2 if is_planar_family(head) => digits.parse().unwrap_or(8),
        _ => 8,
    };
    u8::try_from(depth.clamp(8, 16)).unwrap_or(8)
}

/// Pixel format families whose trailing number is the bit depth (as opposed
/// to `rgb24` or `nv12`, where it is not).
fn is_planar_family(head: &str) -> bool {
    let planar = ["yuv", "gbr"].iter().any(|p| head.starts_with(p)) && head.ends_with('p');
    planar || head == "gray" || head == "ya"
}

/// 4:2:0 chroma subsampling (what every consumer encoder writes and every
/// GPU decoder handles). Unknown formats are assumed to be 4:2:0.
fn is_420(video: &StreamInfo) -> bool {
    let Some(fmt) = video.pix_fmt.as_deref() else {
        return true;
    };
    let fmt = fmt.to_ascii_lowercase();
    fmt.is_empty()
        || fmt.contains("420")
        || ["nv12", "nv21", "p010", "p012", "p016"]
            .iter()
            .any(|p| fmt.starts_with(p))
}

/// Chroma layout for a note, e.g. `4:2:2`.
fn chroma_label(fmt: &str) -> &'static str {
    let fmt = fmt.to_ascii_lowercase();
    if fmt.contains("422") || fmt.starts_with("p2") || fmt.starts_with("y2") {
        "4:2:2"
    } else if fmt.contains("444") || fmt.starts_with("p4") || fmt.starts_with("gbr") {
        "4:4:4"
    } else {
        "this kind of"
    }
}

/// Colour description pass-through. `unknown`/`reserved` values are left
/// out, and so is the `gbr` matrix: the output is always YUV, and tagging
/// YUV as RGB would turn the picture green and purple.
fn color_args(video: &StreamInfo) -> Vec<String> {
    let mut args = Vec::new();
    let pairs = [
        ("-color_primaries", video.color_primaries.as_deref()),
        ("-color_trc", video.color_transfer.as_deref()),
        ("-colorspace", video.color_space.as_deref()),
    ];
    for (flag, value) in pairs {
        let Some(value) = value.map(str::trim) else {
            continue;
        };
        let lower = value.to_ascii_lowercase();
        let placeholder =
            lower.is_empty() || matches!(lower.as_str(), "unknown" | "reserved" | "unspecified");
        let rgb_matrix = flag == "-colorspace" && lower == "gbr";
        let well_formed = lower
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if placeholder || rgb_matrix || !well_formed {
            continue;
        }
        args.push(flag.to_string());
        args.push(lower);
    }
    args
}

// ---------------------------------------------------------------------------
// Audio

struct AudioPlan {
    /// Kept source streams, in output order.
    tracks: Vec<AudioTrack>,
    /// Per-output-stream options (`-c:a:N`, `-b:a:N`, ...).
    args: Vec<String>,
}

struct AudioTrack {
    /// Source stream index.
    index: u32,
}

/// Opus channel layouts libopus accepts with mapping family 1 (the Vorbis
/// channel order). `5.1` here is ffmpeg's back-surround 5.1; the common
/// `5.1(side)` from Blu-ray/AC-3 sources is rejected by libopus as-is.
const OPUS_LAYOUTS: &str = "aformat=channel_layouts=7.1|6.1|5.1|5.0|quad|3.0|stereo|mono";
/// AAC layouts with a standard channel configuration; anything else (such as
/// `5.1(side)`) would be written with a program config element that many
/// players ignore.
const AAC_LAYOUTS: &str = "aformat=channel_layouts=7.1|5.1|5.0|4.0|3.0|stereo|mono";

/// Matroska muxer cannot store these even though `Container::can_copy_audio`
/// allows everything for MKV (Blu-ray/DVD PCM and SMPTE 302M from TS).
const MKV_UNCOPYABLE_AUDIO: &[&str] = &["pcm_bluray", "pcm_dvd", "s302m", "pcm_s24daud"];
/// MPEG-1 Layer II is allowed by `Container::can_copy_audio` for MP4, but MP4
/// labels it as generic MPEG audio: ffprobe then reports it as MP3 and Apple
/// players, which only decode Layer III, play it silently. It is re-encoded.
const MP4_UNCOPYABLE_AUDIO: &[&str] = &["mp2"];

fn plan_audio(probe: &ProbeInfo, profile: &TranscodeProfile, notes: &mut Vec<String>) -> AudioPlan {
    let container = profile.container;
    let all: Vec<&StreamInfo> = probe.audio_streams().collect();
    let (usable, broken): (Vec<&StreamInfo>, Vec<&StreamInfo>) =
        all.iter().copied().partition(|s| is_usable_audio(s));
    if !broken.is_empty() {
        notes.push(format!(
            "Removed {} that contain{} no readable sound",
            plural(broken.len(), "audio track", "audio tracks"),
            if broken.len() == 1 { "s" } else { "" }
        ));
    }

    let wanted = wanted_languages(&profile.audio_languages);
    let mut kept: Vec<&StreamInfo> = usable
        .iter()
        .copied()
        .filter(|s| language_allowed(s.language.as_deref(), &wanted))
        .collect();
    if kept.is_empty() {
        let fallback = usable.iter().copied().find(|s| s.is_default);
        let which = if fallback.is_some() {
            "default"
        } else {
            "first"
        };
        if let Some(track) = fallback.or_else(|| usable.first().copied()) {
            kept.push(track);
            notes.push(format!(
                "None of the audio tracks is in your chosen languages ({}), so the {which} track was kept",
                language_list(&profile.audio_languages)
            ));
        }
    }

    let requested = profile.audio_codec;
    let encode_target = if requested == AudioCodec::Copy || container.supports_audio(requested) {
        requested
    } else {
        container.fallback_audio()
    };

    let mut args = Vec::new();
    let mut forced: Vec<(String, AudioCodec)> = Vec::new();
    let mut target_replaced = false;
    for (n, stream) in kept.iter().enumerate() {
        let source = stream.codec.to_ascii_lowercase();
        let target = if requested == AudioCodec::Copy {
            if can_copy_audio(container, &source) {
                None
            } else {
                let t = copy_fallback(container, &source);
                forced.push((audio_source_name(&source), t));
                Some(t)
            }
        } else if encode_target.ffprobe_name() == Some(source.as_str())
            && can_copy_audio(container, &source)
        {
            None
        } else {
            target_replaced |= encode_target != requested;
            Some(encode_target)
        };
        match target {
            None => {
                args.push(format!("-c:a:{n}"));
                args.push("copy".into());
            }
            Some(codec) => {
                args.extend(encode_audio_args(n, stream, codec, notes));
                if is_matroska(container) {
                    args.extend(clear_statistics_tags(&format!("a:{n}")));
                }
            }
        }
    }

    if target_replaced {
        notes.push(format!(
            "{} can't hold {} audio, so {} was used instead",
            container.label(),
            audio_short_name(requested),
            audio_short_name(encode_target)
        ));
    }
    push_forced_notes(&forced, container, notes);

    let had_default = all.iter().any(|s| s.is_default);
    if had_default && !kept.is_empty() && !kept.iter().any(|s| s.is_default) {
        push(&mut args, &["-disposition:a:0", "default"]);
    }

    AudioPlan {
        tracks: kept.iter().map(|s| AudioTrack { index: s.index }).collect(),
        args,
    }
}

/// A track ffmpeg can actually decode: it has a codec and, where known,
/// channels and a sample rate. Empty PIDs in broadcast recordings fail these.
fn is_usable_audio(stream: &StreamInfo) -> bool {
    let codec = stream.codec.trim().to_ascii_lowercase();
    !codec.is_empty()
        && codec != "none"
        && codec != "unknown"
        && stream.channels != Some(0)
        && stream.sample_rate != Some(0)
}

/// `Container::can_copy_audio` plus the muxers' real limits.
fn can_copy_audio(container: Container, codec: &str) -> bool {
    let refused = match container {
        Container::Mkv | Container::Webm => MKV_UNCOPYABLE_AUDIO,
        Container::Mp4 => MP4_UNCOPYABLE_AUDIO,
    };
    container.can_copy_audio(codec) && !refused.contains(&codec)
}

/// Codec for a track that "Keep original" cannot copy: FLAC for lossless
/// sources where the container holds it (so nothing is lost), otherwise the
/// container's fallback.
fn copy_fallback(container: Container, codec: &str) -> AudioCodec {
    if is_lossless_audio(codec) && container.supports_audio(AudioCodec::Flac) {
        AudioCodec::Flac
    } else {
        container.fallback_audio()
    }
}

fn is_lossless_audio(codec: &str) -> bool {
    codec.starts_with("pcm_")
        || matches!(
            codec,
            "s302m" | "truehd" | "mlp" | "flac" | "alac" | "wavpack" | "tta" | "ape" | "mp4als"
        )
}

/// Sources that deserve 24-bit FLAC; everything else (lossy codecs, 16-bit
/// PCM) fits in 16 bits.
fn is_high_resolution_audio(stream: &StreamInfo) -> bool {
    let codec = stream.codec.to_ascii_lowercase();
    let pcm_16 = matches!(
        codec.as_str(),
        "pcm_s16le"
            | "pcm_s16be"
            | "pcm_u16le"
            | "pcm_u16be"
            | "pcm_s8"
            | "pcm_u8"
            | "pcm_alaw"
            | "pcm_mulaw"
    );
    let dts_hd_ma = codec == "dts"
        && stream
            .profile
            .as_deref()
            .is_some_and(|p| p.to_ascii_uppercase().contains("MA"));
    (is_lossless_audio(&codec) && !pcm_16) || dts_hd_ma
}

/// Most channels ffmpeg's encoder for `codec` accepts. ffmpeg's E-AC-3
/// encoder stops at 5.1 even though the format allows 7.1.
fn max_output_channels(codec: AudioCodec) -> u32 {
    match codec {
        AudioCodec::Eac3 => 6,
        other => other.max_channels(),
    }
}

/// Options for one encoded output audio stream `n`.
fn encode_audio_args(
    n: usize,
    stream: &StreamInfo,
    codec: AudioCodec,
    notes: &mut Vec<String>,
) -> Vec<String> {
    let mut args = vec![format!("-c:a:{n}"), codec.ffmpeg_encoder().to_string()];
    let channels_in = stream.channels.filter(|&c| c > 0).unwrap_or(2);
    let max = max_output_channels(codec);
    let channels = channels_in.min(max);
    if let Some(kbps) = codec.default_bitrate_kbps(channels) {
        args.push(format!("-b:a:{n}"));
        args.push(format!("{kbps}k"));
    }
    if channels_in > max {
        args.push(format!("-ac:a:{n}"));
        args.push(max.to_string());
        notes.push(format!(
            "Downmixed {} audio to {} because {} holds at most {max} channels",
            channel_label(channels_in),
            channel_label(max),
            audio_short_name(codec)
        ));
    }
    if let Some(rate) = output_sample_rate(codec, stream.sample_rate) {
        args.push(format!("-ar:a:{n}"));
        args.push(rate.to_string());
    }
    match codec {
        AudioCodec::Opus if channels > 2 => {
            args.push(format!("-mapping_family:a:{n}"));
            args.push("1".into());
            args.push(format!("-filter:a:{n}"));
            args.push(OPUS_LAYOUTS.into());
        }
        AudioCodec::Aac if channels > 2 => {
            args.push(format!("-filter:a:{n}"));
            args.push(AAC_LAYOUTS.into());
        }
        AudioCodec::Flac => {
            args.push(format!("-sample_fmt:a:{n}"));
            args.push(
                if is_high_resolution_audio(stream) {
                    "s32"
                } else {
                    "s16"
                }
                .into(),
            );
        }
        _ => {}
    }
    args
}

/// Sample rate to force, or `None` to keep the source's.
///
/// - libopus only runs at 48/24/16/12/8 kHz; anything else becomes 48 kHz.
/// - AC-3 and E-AC-3 only run at 48, 44.1 and 32 kHz; MP3 at the MPEG-1/2/2.5
///   rates up to 48 kHz; AAC up to 96 kHz. Other rates move to 44.1 kHz when
///   they are a multiple of 11 025 Hz (so 88.2 kHz halves cleanly), else 48 kHz.
fn output_sample_rate(codec: AudioCodec, source: Option<u32>) -> Option<u32> {
    let family = |rate: u32| if rate % 11_025 == 0 { 44_100 } else { 48_000 };
    match codec {
        AudioCodec::Opus => match source {
            Some(48_000 | 24_000 | 16_000 | 12_000 | 8_000) => None,
            _ => Some(48_000),
        },
        AudioCodec::Ac3 | AudioCodec::Eac3 => match source {
            None | Some(48_000 | 44_100 | 32_000) => None,
            Some(rate) => Some(family(rate)),
        },
        AudioCodec::Mp3 => match source {
            None
            | Some(48_000 | 44_100 | 32_000 | 24_000 | 22_050 | 16_000 | 12_000 | 11_025 | 8_000) => {
                None
            }
            Some(rate) => Some(family(rate)),
        },
        AudioCodec::Aac => match source {
            Some(rate) if rate > 96_000 => Some(family(rate)),
            _ => None,
        },
        _ => None,
    }
}

/// Notes for "Keep original" tracks the container could not take.
fn push_forced_notes(
    forced: &[(String, AudioCodec)],
    container: Container,
    notes: &mut Vec<String>,
) {
    let mut targets: Vec<AudioCodec> = Vec::new();
    for (_, target) in forced {
        if !targets.contains(target) {
            targets.push(*target);
        }
    }
    for target in targets {
        let mut sources: Vec<&str> = Vec::new();
        let mut total = 0usize;
        for (source, _) in forced.iter().filter(|(_, t)| *t == target) {
            total += 1;
            if !sources.contains(&source.as_str()) {
                sources.push(source);
            }
        }
        let note = if total == 1 {
            format!(
                "Converted the {} audio track to {} because {} can't hold it",
                sources.join(", "),
                audio_short_name(target),
                container.label()
            )
        } else {
            format!(
                "Converted {total} audio tracks ({}) to {} because {} can't hold them",
                sources.join(", "),
                audio_short_name(target),
                container.label()
            )
        };
        notes.push(note);
    }
}

fn channel_label(channels: u32) -> String {
    match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        6 => "5.1".into(),
        7 => "6.1".into(),
        8 => "7.1".into(),
        n => format!("{n}-channel"),
    }
}

fn audio_short_name(codec: AudioCodec) -> &'static str {
    match codec {
        AudioCodec::Copy => "original",
        AudioCodec::Opus => "Opus",
        AudioCodec::Aac => "AAC",
        AudioCodec::Flac => "FLAC",
        AudioCodec::Ac3 => "AC-3",
        AudioCodec::Eac3 => "E-AC-3",
        AudioCodec::Mp3 => "MP3",
        AudioCodec::Vorbis => "Vorbis",
    }
}

/// Friendly name for a source audio codec as ffprobe reports it.
fn audio_source_name(codec: &str) -> String {
    let name = match codec {
        "aac" => "AAC",
        "ac3" => "AC-3",
        "eac3" => "E-AC-3",
        "dts" => "DTS",
        "truehd" | "mlp" => "TrueHD",
        "flac" => "FLAC",
        "alac" => "ALAC",
        "mp3" => "MP3",
        "mp2" => "MP2",
        "opus" => "Opus",
        "vorbis" => "Vorbis",
        "wmav1" | "wmav2" | "wmapro" | "wmalossless" => "WMA",
        "s302m" => "SMPTE 302M",
        c if c.starts_with("pcm_") => "PCM",
        other => return other.to_ascii_uppercase(),
    };
    name.to_string()
}

// ---------------------------------------------------------------------------
// Subtitles

struct SubtitleTrack {
    /// Source stream index.
    index: u32,
    /// `copy` or the ffmpeg subtitle encoder.
    codec: &'static str,
}

fn plan_subtitles(
    probe: &ProbeInfo,
    profile: &TranscodeProfile,
    notes: &mut Vec<String>,
) -> Vec<SubtitleTrack> {
    if profile.subtitles == SubtitlePolicy::Drop {
        return Vec::new();
    }
    let container = profile.container;
    let wanted = wanted_languages(&profile.subtitle_languages);
    let mut kept = Vec::new();
    let mut pictures_dropped = 0usize;
    let mut others_dropped: Vec<String> = Vec::new();
    let mut styled_converted = 0usize;

    for stream in probe.subtitle_streams() {
        // Forced subtitles translate foreign dialogue; they are always kept.
        if !stream.is_forced && !language_allowed(stream.language.as_deref(), &wanted) {
            continue;
        }
        let codec = stream.codec.to_ascii_lowercase();
        match container.subtitle_action(&codec) {
            SubtitleAction::Copy => kept.push(SubtitleTrack {
                index: stream.index,
                codec: "copy",
            }),
            SubtitleAction::Convert(encoder) => {
                if matches!(codec.as_str(), "ass" | "ssa") && encoder != "ass" {
                    styled_converted += 1;
                }
                kept.push(SubtitleTrack {
                    index: stream.index,
                    codec: encoder,
                });
            }
            SubtitleAction::Drop if is_image_subtitle(&codec) => pictures_dropped += 1,
            SubtitleAction::Drop => {
                let name = if codec.is_empty() {
                    "unknown".to_string()
                } else {
                    codec
                };
                if !others_dropped.contains(&name) {
                    others_dropped.push(name);
                }
            }
        }
    }

    let label = container.label();
    if pictures_dropped > 0 {
        let (them, n) = if pictures_dropped == 1 {
            ("it", "1 picture-based subtitle".to_string())
        } else {
            (
                "them",
                format!("{pictures_dropped} picture-based subtitles"),
            )
        };
        notes.push(format!("Removed {n} because {label} can't hold {them}"));
    }
    if !others_dropped.is_empty() {
        notes.push(format!(
            "Removed subtitles in a format {label} can't hold ({})",
            others_dropped.join(", ")
        ));
    }
    if styled_converted > 0 {
        notes.push(format!(
            "Converted {} to plain text because {label} can't keep {} styling",
            plural(styled_converted, "styled subtitle", "styled subtitles"),
            if styled_converted == 1 {
                "its"
            } else {
                "their"
            }
        ));
    }
    kept
}

// ---------------------------------------------------------------------------
// Languages

/// Tags that mean "no particular language"; such tracks are always kept.
const UNDETERMINED_LANGUAGES: &[&str] = &["und", "unk", "mul", "zxx", "xx", "none"];

/// Alternative spellings mapped to one ISO 639-2/T code: ISO 639-1 two-letter
/// codes, ISO 639-2/B bibliographic codes, deprecated codes and a few English
/// names that show up in the wild.
const LANGUAGE_ALIASES: &[(&str, &str)] = &[
    ("en", "eng"),
    ("english", "eng"),
    ("ja", "jpn"),
    ("jp", "jpn"),
    ("japanese", "jpn"),
    ("de", "deu"),
    ("ger", "deu"),
    ("german", "deu"),
    ("fr", "fra"),
    ("fre", "fra"),
    ("french", "fra"),
    ("es", "spa"),
    ("spanish", "spa"),
    ("it", "ita"),
    ("italian", "ita"),
    ("pt", "por"),
    ("portuguese", "por"),
    ("ru", "rus"),
    ("russian", "rus"),
    ("zh", "zho"),
    ("chi", "zho"),
    ("cmn", "zho"),
    ("chinese", "zho"),
    ("ko", "kor"),
    ("korean", "kor"),
    ("nl", "nld"),
    ("dut", "nld"),
    ("dutch", "nld"),
    ("sv", "swe"),
    ("swedish", "swe"),
    ("no", "nor"),
    ("nb", "nor"),
    ("nob", "nor"),
    ("nn", "nor"),
    ("nno", "nor"),
    ("norwegian", "nor"),
    ("da", "dan"),
    ("danish", "dan"),
    ("fi", "fin"),
    ("finnish", "fin"),
    ("pl", "pol"),
    ("polish", "pol"),
    ("cs", "ces"),
    ("cze", "ces"),
    ("sk", "slk"),
    ("slo", "slk"),
    ("hu", "hun"),
    ("ro", "ron"),
    ("rum", "ron"),
    ("el", "ell"),
    ("gre", "ell"),
    ("tr", "tur"),
    ("he", "heb"),
    ("iw", "heb"),
    ("ar", "ara"),
    ("hi", "hin"),
    ("th", "tha"),
    ("vi", "vie"),
    ("id", "ind"),
    ("in", "ind"),
    ("ms", "msa"),
    ("may", "msa"),
    ("uk", "ukr"),
    ("bg", "bul"),
    ("hr", "hrv"),
    ("sr", "srp"),
    ("sl", "slv"),
    ("et", "est"),
    ("lv", "lav"),
    ("lt", "lit"),
    ("is", "isl"),
    ("ice", "isl"),
    ("fa", "fas"),
    ("per", "fas"),
    ("ta", "tam"),
    ("te", "tel"),
    ("ml", "mal"),
    ("kn", "kan"),
    ("mr", "mar"),
    ("bn", "ben"),
    ("ur", "urd"),
    ("pa", "pan"),
    ("ca", "cat"),
    ("eu", "eus"),
    ("baq", "eus"),
    ("gl", "glg"),
    ("cy", "cym"),
    ("wel", "cym"),
    ("ga", "gle"),
    ("sq", "sqi"),
    ("alb", "sqi"),
    ("hy", "hye"),
    ("arm", "hye"),
    ("ka", "kat"),
    ("geo", "kat"),
    ("mk", "mkd"),
    ("mac", "mkd"),
    ("my", "mya"),
    ("bur", "mya"),
    ("bo", "bod"),
    ("tib", "bod"),
    ("mi", "mri"),
    ("mao", "mri"),
    ("tl", "tgl"),
    ("la", "lat"),
    ("af", "afr"),
    ("sw", "swa"),
    ("bs", "bos"),
    ("be", "bel"),
    ("kk", "kaz"),
    ("az", "aze"),
    ("uz", "uzb"),
    ("mn", "mon"),
    ("km", "khm"),
    ("lo", "lao"),
    ("si", "sin"),
    ("ne", "nep"),
    ("lb", "ltz"),
    ("mt", "mlt"),
    ("yi", "yid"),
    ("zu", "zul"),
    ("am", "amh"),
];

/// Canonical key for a language tag, or `None` for untagged/undetermined.
/// Region and script subtags are ignored (`en-US` → `eng`, `pt_BR` → `por`).
fn language_key(tag: &str) -> Option<String> {
    let lower = tag.trim().to_ascii_lowercase();
    let primary = lower.split(['-', '_']).next().unwrap_or_default();
    if primary.is_empty() || UNDETERMINED_LANGUAGES.contains(&primary) {
        return None;
    }
    let canonical = LANGUAGE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == primary)
        .map_or(primary, |(_, code)| *code);
    Some(canonical.to_string())
}

/// The profile's language list as canonical keys. Empty keeps everything.
fn wanted_languages(list: &[String]) -> Vec<String> {
    list.iter().filter_map(|l| language_key(l)).collect()
}

/// Whether a track passes a language filter. Untagged and undetermined
/// tracks always pass.
fn language_allowed(language: Option<&str>, wanted: &[String]) -> bool {
    if wanted.is_empty() {
        return true;
    }
    match language.and_then(language_key) {
        None => true,
        Some(key) => wanted.contains(&key),
    }
}

/// The user's languages as they typed them, for notes.
fn language_list(list: &[String]) -> String {
    let items: Vec<&str> = list
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    items.join(", ")
}

// ---------------------------------------------------------------------------
// Small helpers

/// ffmpeg argument for a path. Relative paths get the `file:` protocol
/// prefix so names starting with `-` or containing `:` are never mistaken
/// for options or protocols.
fn path_arg(path: &Path) -> anyhow::Result<String> {
    let Some(text) = path.to_str() else {
        bail!(
            "The path {} contains characters that can't be passed to ffmpeg. Renaming the file fixes this.",
            path.display()
        );
    };
    if text.is_empty() {
        bail!("No file path was given.");
    }
    Ok(if path.is_absolute() {
        text.to_string()
    } else {
        format!("file:{text}")
    })
}

/// AVI and MPEG program/transport streams often lack presentation timestamps
/// on some frames; `+genpts` regenerates them so muxing never fails with
/// "non monotonically increasing dts".
fn needs_genpts(container: &str) -> bool {
    let name = container
        .split(',')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    name == "avi" || name.starts_with("mpeg")
}

fn is_matroska(container: Container) -> bool {
    matches!(container, Container::Mkv | Container::Webm)
}

/// Remove the per-track statistics tags mkvmerge writes (bitrate, frame and
/// byte counts). They describe the old stream, and players such as Jellyfin
/// read the stale bitrate as the new one. `spec` is an output stream
/// specifier like `v:0` or `a:1`.
fn clear_statistics_tags(spec: &str) -> Vec<String> {
    let mut args = Vec::with_capacity(12);
    for key in [
        "BPS",
        "BPS-eng",
        "NUMBER_OF_BYTES",
        "NUMBER_OF_BYTES-eng",
        "NUMBER_OF_FRAMES",
        "NUMBER_OF_FRAMES-eng",
    ] {
        args.push(format!("-metadata:s:{spec}"));
        args.push(format!("{key}="));
    }
    args
}

fn codec_short_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Av1 => "AV1",
        VideoCodec::Hevc => "HEVC",
        VideoCodec::H264 => "H.264",
        VideoCodec::Vp9 => "VP9",
    }
}

/// Friendly name for a source video codec as ffprobe reports it.
fn source_codec_name(codec: &str) -> String {
    let lower = codec.to_ascii_lowercase();
    let name = match lower.as_str() {
        "av1" => "AV1",
        "hevc" | "h265" => "HEVC",
        "h264" | "avc" | "avc1" => "H.264",
        "vp9" => "VP9",
        "vp8" => "VP8",
        "vvc" | "h266" => "VVC",
        "mpeg4" => "MPEG-4",
        "mpeg2video" => "MPEG-2",
        "mpeg1video" => "MPEG-1",
        "vc1" => "VC-1",
        "" => "this",
        _ => return codec.to_ascii_uppercase(),
    };
    name.to_string()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {many}")
    }
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_string()).collect()
}

fn push(args: &mut Vec<String>, items: &[&str]) {
    args.extend(items.iter().map(|s| (*s).to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixel_format_depths() {
        assert_eq!(pix_fmt_depth("yuv420p"), 8);
        assert_eq!(pix_fmt_depth("yuvj420p"), 8);
        assert_eq!(pix_fmt_depth("nv12"), 8);
        assert_eq!(pix_fmt_depth("rgb24"), 8);
        assert_eq!(pix_fmt_depth("yuv420p10le"), 10);
        assert_eq!(pix_fmt_depth("yuv422p10be"), 10);
        assert_eq!(pix_fmt_depth("yuv444p12le"), 12);
        assert_eq!(pix_fmt_depth("p010le"), 10);
        assert_eq!(pix_fmt_depth("p016le"), 16);
        assert_eq!(pix_fmt_depth("gray10le"), 10);
        assert_eq!(pix_fmt_depth("gbrp12le"), 12);
        assert_eq!(pix_fmt_depth("yuva444p10le"), 10);
        assert_eq!(pix_fmt_depth(""), 8);
        assert_eq!(pix_fmt_depth("rgb565le"), 8);
        // Never panics on odd input.
        assert_eq!(pix_fmt_depth("yuvé10"), 8);
        assert_eq!(pix_fmt_depth("10"), 8);
    }

    #[test]
    fn bit_depth_prefers_the_larger_signal() {
        let v = StreamInfo {
            pix_fmt: Some("yuv420p10le".into()),
            bit_depth: None,
            ..Default::default()
        };
        assert_eq!(source_bit_depth(&v), 10);
        let v = StreamInfo {
            pix_fmt: Some("yuv420p".into()),
            bit_depth: Some(10),
            ..Default::default()
        };
        assert_eq!(source_bit_depth(&v), 10);
        assert_eq!(source_bit_depth(&StreamInfo::default()), 8);
    }

    #[test]
    fn language_keys() {
        assert_eq!(language_key("eng").as_deref(), Some("eng"));
        assert_eq!(language_key("en").as_deref(), Some("eng"));
        assert_eq!(language_key("en-US").as_deref(), Some("eng"));
        assert_eq!(language_key("ger").as_deref(), Some("deu"));
        assert_eq!(language_key("fre").as_deref(), Some("fra"));
        assert_eq!(language_key(" JPN ").as_deref(), Some("jpn"));
        assert_eq!(language_key("und"), None);
        assert_eq!(language_key(""), None);
        assert!(language_allowed(Some("en"), &["eng".to_string()]));
        assert!(language_allowed(None, &["eng".to_string()]));
        assert!(!language_allowed(Some("fre"), &["eng".to_string()]));
    }

    #[test]
    fn container_families() {
        assert!(container_matches("matroska", Container::Mkv));
        assert!(container_matches("matroska,webm", Container::Webm));
        assert!(container_matches("mov", Container::Mp4));
        assert!(!container_matches("mov", Container::Mkv));
        assert!(!container_matches("avi", Container::Mp4));
    }

    #[test]
    fn even_widths() {
        assert_eq!(even_width_for(3840, 2160, 1080), 1920);
        assert_eq!(even_width_for(1920, 800, 720), 1728);
        assert_eq!(even_width_for(1998, 1080, 720), 1332);
        assert_eq!(even_width_for(639, 359, 240), 428);
        assert_eq!(even_width_for(0, 0, 720), 0);
    }

    #[test]
    fn sample_rates() {
        assert_eq!(
            output_sample_rate(AudioCodec::Opus, Some(44_100)),
            Some(48_000)
        );
        assert_eq!(output_sample_rate(AudioCodec::Opus, Some(48_000)), None);
        assert_eq!(output_sample_rate(AudioCodec::Opus, None), Some(48_000));
        assert_eq!(output_sample_rate(AudioCodec::Ac3, Some(44_100)), None);
        assert_eq!(
            output_sample_rate(AudioCodec::Ac3, Some(96_000)),
            Some(48_000)
        );
        assert_eq!(
            output_sample_rate(AudioCodec::Eac3, Some(88_200)),
            Some(44_100)
        );
        assert_eq!(output_sample_rate(AudioCodec::Mp3, Some(22_050)), None);
        assert_eq!(
            output_sample_rate(AudioCodec::Mp3, Some(96_000)),
            Some(48_000)
        );
        assert_eq!(
            output_sample_rate(AudioCodec::Aac, Some(192_000)),
            Some(48_000)
        );
        assert_eq!(output_sample_rate(AudioCodec::Flac, Some(192_000)), None);
    }
}
