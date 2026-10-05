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

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail};
use chrysopoeia_core::codec::{is_image_subtitle, source_efficiency_rank};
use chrysopoeia_core::tags::is_statistics_tag;
use chrysopoeia_core::{
    AudioCodec, Container, EncoderCandidate, HdrFormat, HwApi, ProbeInfo, StreamInfo, StreamKind,
    SubtitleAction, SubtitlePolicy, TranscodeProfile, VideoCodec,
};

use crate::quality::{VideoQuality, override_ignored, quality_label, video_quality_args};

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
/// 2. Unknown or zero picture size, or a known duration under one second:
///    skip (such files cannot be converted and verified reliably). An
///    unknown duration is not a reason to skip: Matroska written to a pipe
///    or by a live capture has none, and the converted file gets one.
/// 3. With `skip_efficient`: skip when the source codec's efficiency rank is
///    at least the target's ("Already AV1, which is more efficient than
///    HEVC"). Without it: skip only when the file is already what the
///    profile would write: the target codec in the target container, with a
///    picture every player of that codec handles (4:2:0 chroma; 8-bit for
///    H.264, at most 10-bit otherwise) and audio the container can hold
///    without conversion. Camera footage (4:2:2 10-bit H.264, PCM audio in
///    MOV) is therefore converted under "Plays everywhere".
///
///    Container matching uses ffprobe's demuxer name, which cannot tell MKV
///    from WebM (both report `matroska`) or MP4 from MOV/M4V (all report
///    `mov`). So an MKV or WebM target matches any Matroska-family file, and
///    an MP4 target matches any QuickTime-family file; the skip reason names
///    the family rather than claiming one exact container.
///
///    Neither rule applies when the picture is larger than the profile's
///    `max_height` allows (see [`SizeLimit`]): the user asked for smaller
///    pictures, so the file is converted even if its codec is efficient.
/// 4. Dolby Vision without a standard base layer (profile 5), or without
///    any colour description: skip. Without Dolby Vision reshaping such a
///    picture decodes with wrong colours, and verification would compare
///    two equally wrong pictures.
/// 5. H.264 target with an HDR source: skip. There is no tone mapping, and
///    8-bit H.264 cannot carry HDR, so the colours would be wrong.
/// 6. Audio tracks exist but none of them can be read: skip rather than
///    write a silent file.
pub fn decide(probe: &ProbeInfo, profile: &TranscodeProfile) -> Decision {
    decide_with(probe, profile, false)
}

/// [`decide`] for a file the user asked to convert anyway ("Convert
/// anyway"): rule 3 (already efficient, already in the target format) does
/// not apply. The other rules still do: they skip files that can't be
/// converted without losing something.
pub fn decide_forced(probe: &ProbeInfo, profile: &TranscodeProfile) -> Decision {
    decide_with(probe, profile, true)
}

fn decide_with(probe: &ProbeInfo, profile: &TranscodeProfile, force: bool) -> Decision {
    let Some(video) = probe.primary_video() else {
        return if probe.audio_streams().next().is_some() {
            skip("Audio-only file — nothing to convert")
        } else {
            skip("No video in this file — nothing to convert")
        };
    };
    let (width, height) = match (video.width, video.height) {
        (Some(w), Some(h)) if w > 0 && h > 0 => (w, h),
        _ => return skip("Couldn't read this video's picture size — left unchanged"),
    };
    if let Some(d) = probe.duration_secs
        && d.is_finite()
        && (0.0..MIN_DURATION_SECS).contains(&d)
    {
        return skip("Shorter than a second — nothing worth converting");
    }

    let target = profile.video_codec;
    let too_big = size_limit(profile).is_some_and(|limit| !limit.fits(width, height));
    if !too_big && !force {
        if profile.skip_efficient {
            let rank = source_efficiency_rank(&video.codec);
            if rank >= target.efficiency_rank() {
                return skip(efficient_reason(&video.codec, target, rank));
            }
        } else if VideoCodec::from_probe_name(&video.codec) == Some(target)
            && container_matches(&probe.container, profile.container)
            && plays_as_target(video, target)
            && audio_fits_container(probe, profile)
        {
            return skip(format!(
                "Already {} in {}",
                codec_short_name(target),
                container_family_label(profile.container)
            ));
        }
    }

    if video.hdr == Some(HdrFormat::DolbyVision) && !has_standard_base_layer(video) {
        return skip(if video.dolby_vision_without_base_layer {
            DV_PROFILE_5_REASON
        } else {
            DV_UNKNOWN_COLOURS_REASON
        });
    }
    if target == VideoCodec::H264 && is_hdr(video) {
        return skip("HDR video would lose its colours as H.264 — left unchanged");
    }
    if select_audio(probe, profile).all_unreadable {
        return skip("Couldn't read this file's audio — left unchanged");
    }
    Decision::Transcode
}

/// Whether the source picture is one every decoder of `target` plays and
/// the planner would write as-is: 4:2:0, 8-bit for H.264 (see
/// [`VideoCodec::supports_10bit`]) and at most 10-bit for the others.
fn plays_as_target(video: &StreamInfo, target: VideoCodec) -> bool {
    let max_depth = if target.supports_10bit() { 10 } else { 8 };
    is_420(video) && source_bit_depth(video) <= max_depth
}

/// Whether every audio track the plan would keep can be copied into the
/// profile's container (so a same-format file needs no audio conversion).
fn audio_fits_container(probe: &ProbeInfo, profile: &TranscodeProfile) -> bool {
    select_audio(probe, profile)
        .kept
        .iter()
        .all(|s| can_copy_audio(profile.container, &s.codec.to_ascii_lowercase()))
}

/// The container family a same-format skip matched (see [`decide`]).
fn container_family_label(target: Container) -> &'static str {
    match target {
        Container::Mp4 => "an MP4 or MOV file",
        Container::Mkv => "an MKV file",
        Container::Webm => "a WebM or MKV file",
    }
}

/// Skip reason for Dolby Vision without a standard base layer.
const DV_PROFILE_5_REASON: &str =
    "Dolby Vision profile 5 can't be converted without losing its colours — left unchanged";

/// Skip reason for Dolby Vision whose picture has no colour description: it
/// may be profile 5 (older probes and some files don't say).
const DV_UNKNOWN_COLOURS_REASON: &str = "This Dolby Vision video doesn't say which colours its \
    picture uses, so converting it could ruin them — left unchanged";

/// Dolby Vision streams carry a base layer other players can show when the
/// stream is tagged with a standard transfer (PQ for profiles 7 and 8.1, HLG
/// for 8.4, SDR for 8.2 and 9). Profile 5 has no such layer: the scanner
/// flags it and clears its colour description (see
/// `chrysopoeia_scanner::probe`). A Dolby Vision stream without a transfer
/// tag can't be told apart from it, so it is treated the same way.
fn has_standard_base_layer(video: &StreamInfo) -> bool {
    !video.dolby_vision_without_base_layer
        && video
            .color_transfer
            .as_deref()
            .is_some_and(|t| !is_placeholder_colour(t))
}

/// Colour description values that mean "not specified".
fn is_placeholder_colour(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.is_empty() || matches!(lower.as_str(), "unknown" | "reserved" | "unspecified")
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

/// PQ (SMPTE 2084) transfer, i.e. HDR10-style video. Dolby Vision alone does
/// not imply PQ: its base layer may be HLG, SDR or (profile 5) none at all.
fn is_pq(video: &StreamInfo) -> bool {
    matches!(video.hdr, Some(HdrFormat::Hdr10 | HdrFormat::Hdr10Plus))
        || video.color_transfer.as_deref() == Some("smpte2084")
}

/// HLG (ARIB STD-B67) transfer.
fn is_hlg(video: &StreamInfo) -> bool {
    video.hdr == Some(HdrFormat::Hlg) || video.color_transfer.as_deref() == Some("arib-std-b67")
}

/// The picture size a profile's `max_height` allows.
///
/// `max_height` names a resolution class ("1080p"), so like
/// [`chrysopoeia_core::media::resolution_label`] it looks at both sides: the
/// short side may be at most `max_height`, and the long side at most the
/// 16:9 width of that class (1080 → 1920) plus a fifth. The allowance keeps
/// near-16:9 pictures such as DCI 2K (2048×1080) or 1998×1080 flat, which
/// `resolution_label` also calls 1080p, from being re-encoded for a few
/// pixels. Pictures over the limit are scaled into the exact 16:9 frame: a
/// 3840×1600 scope film at 1080 becomes 1920×800 (not 2592×1080), and a
/// portrait 4K phone clip becomes 1080×1920 (not 608×1080).
///
/// The rule depends only on the short and long side, so a rotated file
/// (whose stored and displayed sizes are swapped) is judged the same before
/// and after conversion, and a converted file is never selected again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeLimit {
    /// Most lines on the short side (even).
    pub short: u32,
    /// Long side of the frame pictures are scaled into (even): the 16:9
    /// width for `short`.
    pub long: u32,
}

impl SizeLimit {
    /// Whether a `width`×`height` picture is within the limit, in either
    /// orientation (see the type docs for the long-side allowance).
    pub fn fits(self, width: u32, height: u32) -> bool {
        let long_allowed = u64::from(self.long) * 6 / 5;
        width.min(height) <= self.short && u64::from(width.max(height)) <= long_allowed
    }

    /// Output size for a `width`×`height` picture scaled down to fit, in the
    /// same orientation, with even sides. Matches [`Self::scale_filter`]
    /// (ffmpeg's `-2` rounding) for unrotated frames.
    pub fn fit(self, width: u32, height: u32) -> (u32, u32) {
        let (box_w, box_h) = if width >= height {
            (self.long, self.short)
        } else {
            (self.short, self.long)
        };
        if u64::from(width) * u64::from(box_h) >= u64::from(height) * u64::from(box_w) {
            (box_w, scaled_even(height, width, box_w))
        } else {
            (scaled_even(width, height, box_h), box_h)
        }
    }

    /// Software `scale` filter that fits frames of either orientation into
    /// the limit. ffmpeg rotates phone video upright before user filters and
    /// `StreamInfo` carries no rotation, so the orientation is decided from
    /// the frames themselves (`iw`/`ih`) rather than the probed size. The
    /// expressions are quoted because they contain commas, which would
    /// otherwise split the filter chain.
    pub fn scale_filter(self) -> String {
        let (l, s) = (self.long, self.short);
        let w =
            format!("if(gte(iw,ih),if(gte(iw*{s},ih*{l}),{l},-2),if(gte(ih*{s},iw*{l}),-2,{s}))");
        let h =
            format!("if(gte(iw,ih),if(gte(iw*{s},ih*{l}),-2,{s}),if(gte(ih*{s},iw*{l}),{l},-2))");
        format!("scale=w='{w}':h='{h}':flags=lanczos")
    }
}

/// The profile's size limit. `max_height` is rounded down to an even
/// number; values below 2 mean "no limit".
pub fn size_limit(profile: &TranscodeProfile) -> Option<SizeLimit> {
    let short = profile.max_height.filter(|&h| h >= 2).map(|h| h & !1)?;
    let long = (u64::from(short) * 16).div_ceil(9);
    let long = u32::try_from(long + (long & 1)).unwrap_or(u32::MAX - 1);
    Some(SizeLimit { short, long })
}

/// A planning failure caused by the original itself (for example, none of
/// its audio can be read): converting it again won't help until the file
/// is replaced. Carried inside the `anyhow::Error` [`build_plan`] returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceProblem(pub String);

impl std::fmt::Display for SourceProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SourceProblem {}

/// How a conversion runs, for messages: "on the CPU", "on the NVIDIA GPU".
pub(crate) fn where_encoded(api: HwApi) -> &'static str {
    match api {
        HwApi::Software => "on the CPU",
        HwApi::Nvenc => "on the NVIDIA GPU",
        HwApi::Qsv => "with Intel Quick Sync",
        HwApi::Vaapi => "on the GPU (VA-API)",
        HwApi::VideoToolbox => "with Apple VideoToolbox",
        HwApi::Amf => "on the AMD GPU",
        HwApi::Rkmpp => "on the Rockchip video engine",
        HwApi::V4l2m2m => "on the board's video engine",
    }
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
    /// Cover images to copy out of the original (with
    /// [`cover_extract_args`]) before `args` run: `args` attach them.
    pub covers: Vec<CoverFile>,
}

/// A cover image copied out of the original before the conversion runs, so
/// a new MKV can carry it as an attachment (`-attach`), as MKV stores cover
/// images. (ffmpeg's MKV writer would store a copied picture stream as a
/// second video track instead.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverFile {
    /// The picture's stream in the original (`-map 0:<index>`).
    pub index: u32,
    /// Where it is written: next to the work file and named after it, so
    /// crash recovery deletes one left behind.
    pub path: PathBuf,
}

/// Build the ffmpeg command for one attempt.
///
/// Argument order: global options, hardware devices and decode options,
/// input options, `-i <input>`, `-map`s, `-attach`es, metadata, video,
/// audio, subtitle, cover and attachment options, muxer options,
/// `-f <muxer> <output>`.
///
/// Cover images are kept where the container can hold them: MP4 gets them
/// as copied picture streams marked as the cover (the video's options then
/// name `v:0` alone), MKV as attachments copied out of the original first
/// ([`FfmpegPlan::covers`]). WebM holds none, and MP4 none but JPEG, PNG
/// and BMP; those are left out with a note.
///
/// Fails (with a plain sentence) when the file has no video, when the
/// encoder does not produce the profile's codec, when the container cannot
/// hold that codec, when a path cannot be passed to ffmpeg, and for files
/// [`decide`] skips because converting them would damage them (Dolby Vision
/// without a standard layer, audio that can't be read). Those checks repeat
/// here because a user can queue a skipped file by hand.
pub fn build_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let profile = req.profile;
    let encoder = req.encoder;
    let container = profile.container;

    let video = req
        .probe
        .primary_video()
        .ok_or_else(|| anyhow!("This file has no video stream to convert."))?;
    if video.hdr == Some(HdrFormat::DolbyVision) && !has_standard_base_layer(video) {
        if video.dolby_vision_without_base_layer {
            bail!(
                "This is Dolby Vision profile 5 video, which has no standard picture layer, so \
                 converting it would ruin its colours."
            );
        }
        bail!(
            "This Dolby Vision video doesn't say which colours its picture uses, so converting \
             it could ruin them."
        );
    }
    if encoder.codec != profile.video_codec {
        bail!(
            "The encoder Chrysopoeia picked makes {} video, but this library is set to {}. \
             Check the hardware settings.",
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
    let audio = plan_audio(req.probe, profile, &mut notes)?;
    let subtitles = plan_subtitles(req.probe, profile, &mut notes);
    let attachments: Vec<u32> = if container.supports_attachments() {
        req.probe
            .streams
            .iter()
            .filter(|s| s.kind == Some(StreamKind::Attachment))
            .map(|s| s.index)
            .collect()
    } else {
        let lost: Vec<&StreamInfo> = lost_attachments(req.probe, profile).collect();
        if !lost.is_empty() {
            let what = if lost.iter().all(|a| is_font_attachment(a)) {
                plural(lost.len(), "subtitle font", "subtitle fonts")
            } else {
                plural(lost.len(), "attached file", "attached files")
            };
            notes.push(format!(
                "Left out {what} because {} can't hold attachments",
                container.label()
            ));
        }
        Vec::new()
    };
    let covers = plan_covers(req.probe, container, req.output, &mut notes);
    let mut video_input_args = video_plan.input_args;
    let mut video_output_args = video_plan.output_args;
    if !covers.copied.is_empty() {
        // The cover is a second (copied) picture stream: every option meant
        // for the video must name the video alone, or ffmpeg would also try
        // to filter or tag the cover, or (a decoder named for the input)
        // read the cover as video and fail to copy it.
        video_input_args = pin_input_decoder(video_input_args, video.index);
        video_output_args = pin_to_main_video(video_output_args);
    }

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
    args.extend(video_input_args);
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
        .chain(attachments.iter().copied())
        .chain(covers.copied.iter().copied());
    for index in mapped {
        args.push("-map".into());
        args.push(format!("0:{index}"));
    }
    // Attached files come after the mapped attachments in the output.
    for cover in &covers.attached {
        args.push("-attach".into());
        args.push(path_arg(&cover.file.path)?);
    }
    push(&mut args, &["-map_metadata", "0", "-map_chapters", "0"]);
    if is_matroska(container) {
        // Out-of-date statistics would follow every kept track into the
        // new file (MP4 keeps no such tags).
        let stream = |index: u32| req.probe.streams.iter().find(|s| s.index == index);
        let kept = std::iter::once(("v:0".to_string(), Some(video)))
            .chain(
                audio
                    .tracks
                    .iter()
                    .enumerate()
                    .map(|(n, t)| (format!("a:{n}"), stream(t.index))),
            )
            .chain(
                subtitles
                    .iter()
                    .enumerate()
                    .map(|(n, t)| (format!("s:{n}"), stream(t.index))),
            )
            .chain(
                attachments
                    .iter()
                    .enumerate()
                    .map(|(n, index)| (format!("t:{n}"), stream(*index))),
            );
        for (spec, source) in kept {
            if let Some(source) = source {
                args.extend(clear_statistics_tags(&spec, source));
            }
        }
    }

    args.extend(video_output_args);
    args.extend(audio.args);
    for (n, sub) in subtitles.iter().enumerate() {
        args.push(format!("-c:s:{n}"));
        args.push(sub.codec.to_string());
    }
    for n in 1..=covers.copied.len() {
        args.push(format!("-c:v:{n}"));
        args.push("copy".into());
        args.push(format!("-disposition:v:{n}"));
        args.push("attached_pic".into());
    }
    if !attachments.is_empty() || !covers.attached.is_empty() {
        push(&mut args, &["-c:t", "copy"]);
    }
    for (i, cover) in covers.attached.iter().enumerate() {
        let n = attachments.len() + i;
        args.push(format!("-metadata:s:t:{n}"));
        args.push(format!("mimetype={}", cover.mimetype));
        args.push(format!("-metadata:s:t:{n}"));
        args.push(format!("filename={}", cover.filename));
    }
    push(&mut args, &["-max_muxing_queue_size", "9999"]);
    if container == Container::Mp4 {
        // faststart puts the index first so playback can start while the
        // file downloads. write_tmcd 0 stops the muxer from turning a copied
        // `timecode` tag (camera footage) into an extra timecode data track.
        push(&mut args, &["-movflags", "+faststart", "-write_tmcd", "0"]);
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
        covers: covers.attached.into_iter().map(|c| c.file).collect(),
    })
}

/// The ffmpeg arguments that copy one cover image out of `input` into
/// `cover.path`, byte for byte (the picture is stored whole in one packet).
pub fn cover_extract_args(input: &Path, cover: &CoverFile) -> anyhow::Result<Vec<String>> {
    let mut args = strings(&[
        "-hide_banner",
        "-nostdin",
        "-y",
        "-loglevel",
        "error",
        "-nostats",
        "-i",
    ]);
    args.push(path_arg(input)?);
    args.push("-map".into());
    args.push(format!("0:{}", cover.index));
    push(
        &mut args,
        &["-c", "copy", "-frames:v", "1", "-f", "rawvideo"],
    );
    args.push(path_arg(&cover.path)?);
    Ok(args)
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
        Some(HdrFormat::DolbyVision) => {
            // Only reached with a standard base layer (see `decide`); name
            // the picture that layer actually carries.
            let base = if is_pq(video) {
                "standard HDR10 picture"
            } else if is_hlg(video) {
                "standard HLG picture"
            } else {
                "standard (non-HDR) picture"
            };
            notes.push(format!(
                "Dolby Vision layers can't be kept, so the video keeps its {base}"
            ));
        }
        Some(HdrFormat::Hdr10Plus) => notes.push(
            "HDR10+ scene data can't be kept, so the video keeps its standard HDR10 picture".into(),
        ),
        _ => {}
    }

    let width = video.width.unwrap_or(0);
    let height = video.height.unwrap_or(0);
    let odd = width % 2 == 1 || height % 2 == 1;
    let limit = size_limit(profile).filter(|limit| !limit.fits(width, height));
    // Output size in the stored orientation (GPU scalers and bitrate-driven
    // encoders need numbers; CPU frames use the orientation-free filter).
    let target_size = limit.map(|limit| limit.fit(width, height));

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
            if let Some(limit) = limit {
                filters.push(limit.scale_filter());
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
            if target_size.is_some() || (depth > 8 && !ten_bit) {
                filters.push(gpu_scale_filter(gpu, target_size, hw_format));
            }
        }
    }
    if let Some((w, h)) = target_size {
        (out_width, out_height) = (w, h);
    }

    let quality = VideoQuality {
        encoder: &encoder.name,
        api,
        codec,
        quality: profile.quality,
        speed: profile.speed,
        quality_override: profile.quality_override,
        ten_bit,
        width: out_width,
        height: out_height,
    };
    if let Some(value) = profile.quality_override
        && override_ignored(&quality)
    {
        notes.push(format!(
            "Your custom quality value ({value}) doesn't fit the quality scale used when converting {}, so the “{}” quality setting was used",
            where_encoded(api),
            quality_label(profile.quality)
        ));
    }
    let mut out = strings(&["-c:v", &encoder.name]);
    out.extend(video_quality_args(&quality));
    if let Some(fmt) = pix_fmt {
        push(&mut out, &["-pix_fmt", fmt]);
    }
    if !filters.is_empty() {
        out.push("-vf".into());
        out.push(filters.join(","));
    }
    out.extend(color_args(video));
    // HDR10 stays HDR10: the CPU encoders don't copy the mastering display
    // and light levels from the decoded frames by themselves, so they are
    // passed explicitly.
    let keeps_hdr10 = ten_bit && is_pq(video);
    if encoder.name == "libx265" {
        let mut params = String::from("log-level=error");
        if keeps_hdr10 {
            // Write HDR10 SEI/VUI on every keyframe and optimise for PQ.
            params.push_str(":hdr-opt=1:repeat-headers=1");
            for p in x265_hdr10_params(video) {
                params.push(':');
                params.push_str(&p);
            }
        }
        push(&mut out, &["-x265-params", &params]);
    }
    if encoder.name == "libsvtav1" && keeps_hdr10 {
        let params = svtav1_hdr10_params(video);
        if !params.is_empty() {
            push(&mut out, &["-svtav1-params", &params.join(":")]);
        }
    }
    if profile.container == Container::Mp4 && codec == VideoCodec::Hevc {
        // Apple devices only play HEVC in MP4 when tagged hvc1.
        push(&mut out, &["-tag:v", "hvc1"]);
    }

    VideoPlan {
        input_args,
        output_args: out,
    }
}

/// libx265 options for the source's HDR10 static metadata: chromaticities
/// in 0.00002 units and luminance in 0.0001 cd/m² (x265's notation).
fn x265_hdr10_params(video: &StreamInfo) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = video.mastering_display {
        let c = |v: f64| (v * 50_000.0).round().clamp(0.0, 50_000.0) as u32;
        let l = |v: f64| (v * 10_000.0).round().clamp(0.0, 4.0e9) as u64;
        out.push(format!(
            "master-display=G({},{})B({},{})R({},{})WP({},{})L({},{})",
            c(m.green[0]),
            c(m.green[1]),
            c(m.blue[0]),
            c(m.blue[1]),
            c(m.red[0]),
            c(m.red[1]),
            c(m.white_point[0]),
            c(m.white_point[1]),
            l(m.max_luminance),
            l(m.min_luminance)
        ));
    }
    if let Some(cl) = video.content_light {
        out.push(format!("max-cll={},{}", cl.max_cll, cl.max_fall));
    }
    out
}

/// SVT-AV1 options for the source's HDR10 static metadata (chromaticities
/// and cd/m² as decimals).
fn svtav1_hdr10_params(video: &StreamInfo) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = video.mastering_display {
        let xy = |p: [f64; 2]| format!("({:.4},{:.4})", p[0], p[1]);
        out.push(format!(
            "mastering-display=G{}B{}R{}WP{}L({:.4},{:.4})",
            xy(m.green),
            xy(m.blue),
            xy(m.red),
            xy(m.white_point),
            m.max_luminance,
            m.min_luminance
        ));
    }
    if let Some(cl) = video.content_light {
        out.push(format!("content-light={},{}", cl.max_cll, cl.max_fall));
    }
    out
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
///
/// The size is given explicitly (from [`SizeLimit::fit`]) rather than as
/// `-2`/`-1`: `scale_qsv` computes `-1` without even rounding (1920×804 at
/// 720 lines would be 1719 wide, which 4:2:0 encoders reject). GPU frames
/// are never auto-rotated (ffmpeg's rotation filters work on CPU frames), so
/// the stored orientation is the one the filter sees.
fn gpu_scale_filter(gpu: GpuFrames, size: Option<(u32, u32)>, format: &str) -> String {
    let name = match gpu {
        GpuFrames::Cuda => "scale_cuda",
        GpuFrames::Vaapi => "scale_vaapi",
        GpuFrames::Qsv => "scale_qsv",
    };
    match size {
        Some((w, h)) => format!("{name}=w={w}:h={h}:format={format}"),
        None => format!("{name}=format={format}"),
    }
}

/// Length of `side` after the picture is scaled so that `other` becomes
/// `target`, rounded to an even number exactly like ffmpeg's `-2`:
/// `round(target * side / (other * 2)) * 2`. Never less than 2.
fn scaled_even(side: u32, other: u32, target: u32) -> u32 {
    if side == 0 || other == 0 {
        return 2;
    }
    let (s, o, t) = (u64::from(side), u64::from(other), u64::from(target));
    let halves = (t * s + o) / (2 * o);
    u32::try_from(halves * 2).unwrap_or(u32::MAX - 1).max(2)
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
        let placeholder = is_placeholder_colour(&lower);
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

/// Channel layouts in the Vorbis channel order, by channel count − 1. Opus
/// (mapping family 1) and Vorbis only accept these; `5.0`/`5.1` are ffmpeg's
/// back-surround layouts and `quad` is FL+FR+BL+BR. The common `5.1(side)`
/// and `5.0(side)` from Blu-ray/DVD sources must be converted to them.
const VORBIS_ORDER_LAYOUTS: [Option<&str>; 8] = [
    Some("mono"),
    Some("stereo"),
    Some("3.0"),
    Some("quad"),
    Some("5.0"),
    Some("5.1"),
    Some("6.1"),
    Some("7.1"),
];

/// AAC's standard channel configurations, by channel count − 1. Anything
/// else is written with a program config element that many TVs and
/// streamers ignore. There is no standard 7-channel configuration, so 6.1
/// becomes 5.1 (which plays everywhere) rather than a padded 7.1.
const AAC_STANDARD_LAYOUTS: [Option<&str>; 8] = [
    Some("mono"),
    Some("stereo"),
    Some("3.0"),
    Some("4.0"),
    Some("5.0"),
    Some("5.1"),
    None,
    Some("7.1"),
];

/// Layout lists for tracks whose channel count the probe could not read
/// (kept only when no track could be read fully, see [`select_audio`]).
/// Format negotiation then picks a layout the encoder accepts. It may pad
/// with a silent channel, which beats failing on an unknown layout.
const VORBIS_ORDER_ANY: &str = "aformat=channel_layouts=7.1|6.1|5.1|5.0|quad|3.0|stereo|mono";
const AAC_ANY: &str = "aformat=channel_layouts=7.1|5.1|5.0|4.0|3.0|stereo|mono";

/// How well the probe could read an audio track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioHealth {
    /// Codec known, and channels and sample rate known or not reported.
    Readable,
    /// Codec known but zero channels or sample rate. A plain ffprobe reports
    /// this for empty PIDs in broadcast recordings, but also for tracks that
    /// start late in a transport stream, which ffmpeg's deeper probe during
    /// the conversion (`-analyzeduration 100M`) reads fine.
    Unconfirmed,
    /// No codec ffmpeg knows: the track can be neither decoded nor copied.
    Unreadable,
}

fn audio_health(stream: &StreamInfo) -> AudioHealth {
    let codec = stream.codec.trim().to_ascii_lowercase();
    if codec.is_empty() || codec == "none" || codec == "unknown" {
        AudioHealth::Unreadable
    } else if stream.channels == Some(0) || stream.sample_rate == Some(0) {
        AudioHealth::Unconfirmed
    } else {
        AudioHealth::Readable
    }
}

/// The audio tracks an output keeps, before any codec decisions.
struct AudioSelection<'a> {
    /// Kept tracks, in source (and output) order.
    kept: Vec<&'a StreamInfo>,
    /// Tracks removed because they hold no readable sound.
    dropped_broken: usize,
    /// The file has audio, but not one track can be read.
    all_unreadable: bool,
    /// When the language filter matched nothing: which track was kept
    /// instead ("default", "first" or "first main").
    fallback: Option<&'static str>,
}

/// Choose the audio tracks to keep.
///
/// 1. Readable tracks are the candidates. Only when there are none do
///    unconfirmed tracks become the candidates: dropping them would leave
///    the video silent, and verification (which trusts `expected`) would
///    accept that. If such a track really is empty, ffmpeg fails and the
///    original stays untouched. Unreadable tracks are never kept.
/// 2. The language filter applies to the candidates. Untagged and
///    undetermined tracks always pass.
/// 3. If nothing passes, one track is kept anyway: the default one, else the
///    first that is not commentary or audio description, else the first.
fn select_audio<'a>(probe: &'a ProbeInfo, profile: &TranscodeProfile) -> AudioSelection<'a> {
    let all: Vec<&StreamInfo> = probe.audio_streams().collect();
    let with_health = |health: AudioHealth| -> Vec<&'a StreamInfo> {
        all.iter()
            .copied()
            .filter(|s| audio_health(s) == health)
            .collect()
    };
    let readable = with_health(AudioHealth::Readable);
    let candidates = if readable.is_empty() {
        with_health(AudioHealth::Unconfirmed)
    } else {
        readable
    };

    let wanted = wanted_languages(&profile.audio_languages);
    let mut kept: Vec<&StreamInfo> = candidates
        .iter()
        .copied()
        .filter(|s| language_allowed(s.language.as_deref(), &wanted))
        .collect();
    let mut fallback = None;
    if kept.is_empty() {
        let default = candidates.iter().copied().find(|s| s.is_default);
        let main = candidates.iter().copied().find(|s| !is_secondary_audio(s));
        let (pick, which) = match (default, main) {
            (Some(track), _) => (Some(track), "default"),
            (None, Some(track)) if candidates.first().is_some_and(|f| f.index != track.index) => {
                (Some(track), "first main")
            }
            (None, _) => (candidates.first().copied(), "first"),
        };
        if let Some(track) = pick {
            kept.push(track);
            fallback = Some(which);
        }
    }

    AudioSelection {
        dropped_broken: all.len() - candidates.len(),
        all_unreadable: !all.is_empty() && candidates.is_empty(),
        kept,
        fallback,
    }
}

/// Commentary and audio-description tracks, recognised by their title
/// (`StreamInfo` carries no `comment` or `visual_impaired` disposition).
/// They are never promoted to the default track.
fn is_secondary_audio(stream: &StreamInfo) -> bool {
    stream.title.as_deref().is_some_and(|title| {
        let title = title.to_ascii_lowercase();
        title.contains("comment") || title.contains("descri")
    })
}

fn plan_audio(
    probe: &ProbeInfo,
    profile: &TranscodeProfile,
    notes: &mut Vec<String>,
) -> anyhow::Result<AudioPlan> {
    let container = profile.container;
    let selection = select_audio(probe, profile);
    if selection.all_unreadable {
        return Err(SourceProblem(
            "None of this file's audio tracks can be read, so converting it would leave the \
             video silent. The original was left unchanged."
                .to_string(),
        )
        .into());
    }
    if selection.dropped_broken > 0 {
        notes.push(format!(
            "Removed {} that contain{} no readable sound",
            plural(selection.dropped_broken, "audio track", "audio tracks"),
            if selection.dropped_broken == 1 {
                "s"
            } else {
                ""
            }
        ));
    }
    if let Some(which) = selection.fallback {
        notes.push(format!(
            "None of the audio tracks is in your chosen languages ({}), so the {which} track was kept",
            language_list(&profile.audio_languages)
        ));
    }
    let kept = selection.kept;

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
            Some(codec) => args.extend(encode_audio_args(n, stream, codec, notes)),
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

    // Keep a default track when the original default was filtered out.
    // `+default` adds the flag without wiping the track's other flags (such
    // as `original`), and commentary is never the one promoted.
    let had_default = probe.audio_streams().any(|s| s.is_default);
    if had_default && !kept.is_empty() && !kept.iter().any(|s| s.is_default) {
        let n = kept
            .iter()
            .position(|s| !is_secondary_audio(s))
            .unwrap_or(0);
        args.push(format!("-disposition:a:{n}"));
        args.push("+default".into());
    }

    Ok(AudioPlan {
        tracks: kept.iter().map(|s| AudioTrack { index: s.index }).collect(),
        args,
    })
}

/// `Container::can_copy_audio` (which follows the muxers' real limits).
fn can_copy_audio(container: Container, codec: &str) -> bool {
    container.can_copy_audio(codec)
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

/// Most channels ffmpeg's encoder for `codec` accepts.
fn max_output_channels(codec: AudioCodec) -> u32 {
    codec.max_channels()
}

/// The one layout an Opus, Vorbis or AAC encoder must receive for a source
/// with `channels` channels, and its channel count. `None` for other codecs
/// and for mono/stereo, which need no conversion.
///
/// Exactly one layout is passed to `aformat`: given a list, ffmpeg's format
/// negotiation prefers a layout containing every source channel and pads
/// with silent channels (DVD `5.0(side)` became 7.1 AAC or 6.1 Opus). The
/// count is kept wherever the format has a layout for it, with two
/// exceptions: sources with an LFE channel but fewer than six channels (2.1,
/// 3.1, 4.1) become 5.1 so the bass channel survives, and 7-channel AAC
/// becomes 5.1 (see [`AAC_STANDARD_LAYOUTS`]). ffmpeg's resampler maps side
/// to back surrounds one to one, so `5.1(side)` → `5.1` loses nothing.
fn standard_layout(
    codec: AudioCodec,
    channels: u32,
    source_layout: Option<&str>,
) -> Option<(&'static str, u32)> {
    let table = match codec {
        AudioCodec::Opus | AudioCodec::Vorbis => &VORBIS_ORDER_LAYOUTS,
        AudioCodec::Aac => &AAC_STANDARD_LAYOUTS,
        _ => return None,
    };
    let mut count = channels.min(max_output_channels(codec)).min(8);
    if count <= 2 {
        return None;
    }
    if count < 6 && source_layout.is_some_and(layout_has_lfe) {
        count = 6;
    }
    (1..=count).rev().find_map(|c| {
        let slot = usize::try_from(c - 1).ok()?;
        table.get(slot).copied().flatten().map(|name| (name, c))
    })
}

/// Whether an ffprobe channel layout name includes a low-frequency channel
/// (`2.1`, `5.1(side)`, `7.1(wide)`, `22.2`, or a custom `FL+FR+LFE`).
fn layout_has_lfe(layout: &str) -> bool {
    let lower = layout.to_ascii_lowercase();
    lower.contains(".1") || lower.contains(".2") || lower.contains("lfe")
}

/// Options for one encoded output audio stream `n`.
fn encode_audio_args(
    n: usize,
    stream: &StreamInfo,
    codec: AudioCodec,
    notes: &mut Vec<String>,
) -> Vec<String> {
    let mut args = vec![format!("-c:a:{n}"), codec.ffmpeg_encoder().to_string()];
    let name = audio_short_name(codec);
    let max = max_output_channels(codec);
    // Zero means the probe could not read the count (see `AudioHealth`).
    let channels_in = stream.channels.filter(|&c| c > 0);
    let layout =
        channels_in.and_then(|c| standard_layout(codec, c, stream.channel_layout.as_deref()));
    let channels_out = match (layout, channels_in) {
        (Some((_, count)), _) => Some(count),
        (None, Some(count)) => Some(count.min(max)),
        (None, None) => None,
    };

    // With an unknown channel count the encoder picks its own per-channel
    // default bitrate, which beats guessing.
    if let Some(kbps) = channels_out.and_then(|c| codec.default_bitrate_kbps(c)) {
        args.push(format!("-b:a:{n}"));
        args.push(format!("{kbps}k"));
    }
    if let (Some(cin), Some(cout)) = (channels_in, channels_out) {
        let from = source_channel_label(stream, cin);
        let to = layout.map_or_else(|| channel_label(cout), |(l, _)| friendly_layout(l));
        if cout < cin {
            let why = if cin > max {
                format!("{name} holds at most {max} channels")
            } else {
                format!("{name} has no standard {from} layout")
            };
            notes.push(format!("Downmixed {from} audio to {to} because {why}"));
        } else if cout > cin {
            notes.push(format!(
                "Wrote {from} audio as {to} so its bass channel is kept, because {name} has no {from} layout (the added channels are silent)"
            ));
        }
        if layout.is_none() && cin > max {
            args.push(format!("-ac:a:{n}"));
            args.push(max.to_string());
        }
    }
    if let Some(rate) = output_sample_rate(codec, stream.sample_rate.filter(|&r| r > 0)) {
        args.push(format!("-ar:a:{n}"));
        args.push(rate.to_string());
    }

    let layout_filter = match (codec, layout, channels_out) {
        (_, Some((name, _)), _) => Some(format!("aformat=channel_layouts={name}")),
        (AudioCodec::Opus | AudioCodec::Vorbis, None, None) => Some(VORBIS_ORDER_ANY.to_string()),
        (AudioCodec::Aac, None, None) => Some(AAC_ANY.to_string()),
        _ => None,
    };
    if codec == AudioCodec::Opus && channels_out.is_some_and(|c| c > 2) {
        // Surround Opus needs the Vorbis-order mapping family. With an
        // unknown count libopus chooses the family itself.
        args.push(format!("-mapping_family:a:{n}"));
        args.push("1".into());
    }
    if let Some(filter) = layout_filter {
        args.push(format!("-filter:a:{n}"));
        args.push(filter);
    }
    if codec == AudioCodec::Flac {
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
    args
}

/// Sample rate to force, or `None` to keep the source's.
///
/// - libopus only runs at 48/24/16/12/8 kHz; anything else becomes 48 kHz.
/// - AC-3 and E-AC-3 only run at 48, 44.1 and 32 kHz; MP3 at the MPEG-1/2/2.5
///   rates up to 48 kHz; AAC up to 96 kHz. Other rates move to 44.1 kHz when
///   they are a multiple of 11 025 Hz (so 88.2 kHz halves cleanly), else 48 kHz.
fn output_sample_rate(codec: AudioCodec, source: Option<u32>) -> Option<u32> {
    let family = |rate: u32| {
        if rate.is_multiple_of(11_025) {
            44_100
        } else {
            48_000
        }
    };
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

/// Plain name for a channel count, e.g. `5.1` for six channels.
fn channel_label(channels: u32) -> String {
    match channels {
        1 => "mono".into(),
        2 => "stereo".into(),
        5 => "5.0".into(),
        6 => "5.1".into(),
        7 => "6.1".into(),
        8 => "7.1".into(),
        n => format!("{n}-channel"),
    }
}

/// Plain name for a source track's layout: ffprobe's layout name without
/// its speaker-position detail (`5.1(side)` → `5.1`) when it is a familiar
/// one, otherwise the channel count's name.
fn source_channel_label(stream: &StreamInfo, channels: u32) -> String {
    stream
        .channel_layout
        .as_deref()
        .map(friendly_layout)
        .filter(|name| {
            matches!(
                name.as_str(),
                "mono"
                    | "stereo"
                    | "2.1"
                    | "3.0"
                    | "3.1"
                    | "4.0"
                    | "4.1"
                    | "quad"
                    | "5.0"
                    | "5.1"
                    | "6.0"
                    | "6.1"
                    | "7.0"
                    | "7.1"
            )
        })
        .unwrap_or_else(|| channel_label(channels))
}

/// An ffmpeg layout name without its parenthesised variant.
fn friendly_layout(layout: &str) -> String {
    layout
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string()
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

/// `Container::subtitle_action`, which follows the muxers' real limits: text
/// formats Matroska can't store are converted (old `ssa` to ASS, the rest to
/// SubRip) and picture formats it can't store (DivX XSUB) are dropped.
fn subtitle_action(container: Container, codec: &str) -> SubtitleAction {
    container.subtitle_action(codec)
}

/// Picture-based subtitle tracks the profile wants kept (its language list,
/// plus forced tracks) that the target container can't hold. (Other formats
/// it can't hold, such as teletext in TV recordings, are dropped with a
/// note as before: they are rarely the subtitles people watch.)
fn lost_picture_subtitles<'a>(
    probe: &'a ProbeInfo,
    profile: &TranscodeProfile,
) -> impl Iterator<Item = &'a StreamInfo> {
    let keep = profile.subtitles != SubtitlePolicy::Drop;
    let wanted = wanted_languages(&profile.subtitle_languages);
    let container = profile.container;
    probe.subtitle_streams().filter(move |s| {
        let codec = s.codec.to_ascii_lowercase();
        keep && (s.is_forced || language_allowed(s.language.as_deref(), &wanted))
            && is_image_subtitle(&codec)
            && subtitle_action(container, &codec) == SubtitleAction::Drop
    })
}

/// Whether an attachment is a font (for styled subtitles).
fn is_font_attachment(stream: &StreamInfo) -> bool {
    matches!(
        stream.codec.to_ascii_lowercase().as_str(),
        "ttf" | "otf" | "woff" | "woff2"
    )
}

/// Attachments the target container would leave out: every one when it
/// can't hold attachments (only MKV can), except fonts when no subtitles
/// are kept anyway.
fn lost_attachments<'a>(
    probe: &'a ProbeInfo,
    profile: &TranscodeProfile,
) -> impl Iterator<Item = &'a StreamInfo> {
    let lost = !profile.container.supports_attachments();
    let subtitles_kept = profile.subtitles != SubtitlePolicy::Drop;
    probe.streams.iter().filter(move |s| {
        lost && s.kind == Some(StreamKind::Attachment) && (subtitles_kept || !is_font_attachment(s))
    })
}

/// Whether a subtitle codec is ASS or its older form SSA, which carry
/// styling: positions, colours, fonts, and lines shown at the same time.
fn is_styled_subtitle_codec(codec: &str) -> bool {
    matches!(codec, "ass" | "ssa")
}

/// ASS/SSA subtitle tracks the profile keeps (its language list, plus
/// forced tracks) that the target container stores as plain text (MP4's
/// timed text, WebM's WebVTT): their positions, colours and fonts are lost,
/// and lines shown at the same time are cut short. ffprobe can't tell a
/// styled track from a plain one without reading the whole track, so every
/// such track counts.
fn restyled_subtitles<'a>(
    probe: &'a ProbeInfo,
    profile: &TranscodeProfile,
) -> impl Iterator<Item = &'a StreamInfo> {
    let keep = profile.subtitles != SubtitlePolicy::Drop;
    let wanted = wanted_languages(&profile.subtitle_languages);
    let container = profile.container;
    probe.subtitle_streams().filter(move |s| {
        let codec = s.codec.to_ascii_lowercase();
        keep && (s.is_forced || language_allowed(s.language.as_deref(), &wanted))
            && is_styled_subtitle_codec(&codec)
            && matches!(
                subtitle_action(container, &codec),
                SubtitleAction::Convert(encoder) if encoder != "ass"
            )
    })
}

/// When converting `probe` under `profile` would lose part of what the
/// original has, a plain sentence saying so: picture-based subtitles the
/// new container can't hold (MP4 and WebM hold only text), the styling of
/// ASS/SSA subtitles (MP4 and WebM keep only their text), attachments such
/// as the fonts of styled subtitles (only MKV keeps them), or cover images
/// (MP4 keeps JPEG, PNG and BMP ones, WebM none). Replacing the original
/// would lose them for good, so such a file is left unchanged when
/// originals are replaced, unless the user converts it anyway (see
/// [`crate::run::run_job`]). `None` when nothing would be lost.
///
/// The sentence starts "{container} can't hold this file's {losses}, so it
/// was left unchanged." (the web UI recognises it by that).
pub fn replace_loss(probe: &ProbeInfo, profile: &TranscodeProfile) -> Option<String> {
    let pictures = lost_picture_subtitles(probe, profile).count();
    let styled = restyled_subtitles(probe, profile).count();
    let attachments: Vec<&StreamInfo> = lost_attachments(probe, profile).collect();
    let fonts = attachments.iter().filter(|a| is_font_attachment(a)).count();
    let covers = lost_covers(probe, profile.container).count();
    let mut lost: Vec<String> = Vec::new();
    if pictures > 0 {
        lost.push(plural(
            pictures,
            "picture-based subtitle",
            "picture-based subtitles",
        ));
    }
    if styled > 0 {
        lost.push(plural(styled, "styled subtitle", "styled subtitles"));
    }
    if !attachments.is_empty() {
        lost.push(if fonts == attachments.len() {
            plural(fonts, "subtitle font", "subtitle fonts")
        } else {
            plural(attachments.len(), "attached file", "attached files")
        });
    }
    if covers > 0 {
        lost.push(plural(covers, "cover image", "cover images"));
    }
    if lost.is_empty() {
        return None;
    }
    let label = profile.container.label();
    Some(format!(
        "{label} can't hold this file's {}, so it was left unchanged. To convert it, choose an \
         MKV goal or save converted files to a separate folder; Convert anyway converts it \
         without them",
        join_list(&lost)
    ))
}

/// "a", "a and b", "a, b and c".
fn join_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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
        match subtitle_action(container, &codec) {
            SubtitleAction::Copy => kept.push(SubtitleTrack {
                index: stream.index,
                codec: "copy",
            }),
            SubtitleAction::Convert(encoder) => {
                if is_styled_subtitle_codec(&codec) && encoder != "ass" {
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
// Cover images

/// What happens to one cover image under a target container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoverFate {
    /// MP4: copied as a picture stream marked as the cover.
    Copy,
    /// MKV: copied out of the original first, then attached (see
    /// [`CoverFile`]).
    Attach,
    /// The container can't hold it.
    Lost,
}

fn cover_fate(container: Container, cover: &StreamInfo) -> CoverFate {
    match container {
        Container::Mkv => CoverFate::Attach,
        // MP4 stores cover art as JPEG, PNG or BMP only.
        Container::Mp4
            if matches!(
                cover.codec.to_ascii_lowercase().as_str(),
                "mjpeg" | "png" | "bmp"
            ) =>
        {
            CoverFate::Copy
        }
        Container::Mp4 | Container::Webm => CoverFate::Lost,
    }
}

/// Cover images: pictures stored with the file (an MKV attachment such as
/// `cover.jpg`, MP4 cover art), which ffprobe shows as one-picture video
/// streams.
fn cover_images(probe: &ProbeInfo) -> impl Iterator<Item = &StreamInfo> {
    probe
        .streams
        .iter()
        .filter(|s| s.kind == Some(StreamKind::Video) && s.is_attached_pic)
}

/// Cover images the target container can't hold.
fn lost_covers(probe: &ProbeInfo, container: Container) -> impl Iterator<Item = &StreamInfo> {
    cover_images(probe).filter(move |c| cover_fate(container, c) == CoverFate::Lost)
}

/// A cover image attached to a new MKV.
struct AttachedCover {
    file: CoverFile,
    mimetype: String,
    filename: String,
}

/// The cover images an output keeps.
#[derive(Default)]
struct CoverPlan {
    /// MP4: source stream indices, mapped after everything else.
    copied: Vec<u32>,
    /// MKV: pictures attached from files copied out beforehand.
    attached: Vec<AttachedCover>,
}

fn plan_covers(
    probe: &ProbeInfo,
    container: Container,
    output: &Path,
    notes: &mut Vec<String>,
) -> CoverPlan {
    let mut plan = CoverPlan::default();
    let mut lost = 0usize;
    let mut names: Vec<String> = Vec::new();
    for cover in cover_images(probe) {
        match cover_fate(container, cover) {
            CoverFate::Copy => plan.copied.push(cover.index),
            CoverFate::Attach => {
                let (extension, mimetype) = image_type(cover);
                let filename = cover_file_name(cover, extension, &names);
                names.push(filename.clone());
                plan.attached.push(AttachedCover {
                    file: CoverFile {
                        index: cover.index,
                        path: cover_path(output, plan.attached.len() + 1, extension),
                    },
                    mimetype,
                    filename,
                });
            }
            CoverFate::Lost => lost += 1,
        }
    }
    if lost > 0 {
        notes.push(format!(
            "Left out {} because {} can't hold {}",
            plural(lost, "cover image", "cover images"),
            container.label(),
            if lost == 1 { "it" } else { "them" }
        ));
    }
    plan
}

/// File extension and MIME type for a cover image: its own `mimetype` tag
/// when it names a picture type, else the one its codec implies.
fn image_type(cover: &StreamInfo) -> (&'static str, String) {
    let (extension, implied) = match cover.codec.to_ascii_lowercase().as_str() {
        "mjpeg" | "jpeg" | "jpg" => ("jpg", "image/jpeg"),
        "png" | "apng" => ("png", "image/png"),
        "gif" => ("gif", "image/gif"),
        "bmp" => ("bmp", "image/bmp"),
        "webp" => ("webp", "image/webp"),
        "tiff" => ("tif", "image/tiff"),
        "jpegxl" => ("jxl", "image/jxl"),
        _ => ("bin", "application/octet-stream"),
    };
    let mimetype = cover
        .mimetype
        .as_deref()
        .map(str::trim)
        .filter(|m| m.starts_with("image/") && m.chars().all(|c| c.is_ascii_graphic()))
        .unwrap_or(implied)
        .to_ascii_lowercase();
    (extension, mimetype)
}

/// The name a cover image gets in a new MKV: its own (MKV sources), else
/// `cover.<ext>` (the name media servers look for), then `cover-2.<ext>`
/// and so on. Names already used in this file are not used twice.
fn cover_file_name(cover: &StreamInfo, extension: &str, used: &[String]) -> String {
    let own = cover.filename.as_deref().map(str::trim).filter(|n| {
        !n.is_empty()
            && !n.contains(['/', '\\'])
            && !n.chars().any(char::is_control)
            && !used.iter().any(|u| u == n)
    });
    if let Some(name) = own {
        return name.to_string();
    }
    (1..)
        .map(|n| {
            if n == 1 {
                format!("cover.{extension}")
            } else {
                format!("cover-{n}.{extension}")
            }
        })
        .find(|name| !used.contains(name))
        .unwrap_or_else(|| format!("cover.{extension}"))
}

/// Where the `n`th cover image is copied out: next to the work file
/// `output`, named after it (`.Movie.chrysopoeia-1a2b3c4d.tmp.cover-1.jpg`
/// for `.Movie.chrysopoeia-1a2b3c4d.tmp.mkv`), so it is a leftover crash
/// recovery recognises.
fn cover_path(output: &Path, n: usize, extension: &str) -> PathBuf {
    let stem = output
        .file_stem()
        .map_or_else(|| "output".into(), |s| s.to_string_lossy().into_owned());
    output.with_file_name(format!("{stem}.cover-{n}.{extension}"))
}

/// Input options with a decoder named for every video stream (`-c:v
/// h264_qsv`, Quick Sync decoding) rewritten to name the video's own
/// stream (`-c:<index>`). ffmpeg would otherwise read a cover image as
/// that codec too, and copying it would fail.
fn pin_input_decoder(args: Vec<String>, video_index: u32) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "-c:v"
            && let Some(decoder) = args.next()
        {
            out.push(format!("-c:{video_index}"));
            out.push(decoder);
        } else {
            out.push(arg);
        }
    }
    out
}

/// Video options rewritten to name the main video (output stream `v:0`)
/// alone: `-c:v` → `-c:v:0`, `-vf` → `-filter:v:0`, `-crf` → `-crf:v:0`.
/// Used when the output also holds a copied cover image as a second
/// picture stream, which must be neither filtered nor re-encoded (ffmpeg
/// refuses to filter a copied stream). The options come in flag/value
/// pairs; anything else is returned unchanged.
fn pin_to_main_video(args: Vec<String>) -> Vec<String> {
    if !args.len().is_multiple_of(2) || !args.iter().step_by(2).all(|a| a.starts_with('-')) {
        return args;
    }
    args.into_iter()
        .enumerate()
        .map(|(i, arg)| {
            if i % 2 == 1 {
                arg
            } else if arg == "-vf" {
                "-filter:v:0".to_string()
            } else if arg.ends_with(":v") {
                format!("{arg}:0")
            } else if arg.contains(':') {
                // Already names a stream (`-metadata:s:v:0`, `-c:v:0`).
                arg
            } else {
                format!("{arg}:v:0")
            }
        })
        .collect()
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

/// Remove the Matroska statistics tags (`BPS`, `DURATION`,
/// `NUMBER_OF_FRAMES`, `NUMBER_OF_BYTES`, `_STATISTICS_*`, plain or
/// localized such as `DURATION-eng`) that `source`, a kept track of the
/// original, has. ffmpeg copies a track's tags into the new file whether it
/// copies or re-encodes the track, but they describe the old track: a clip
/// cut from a film keeps the film's `DURATION-eng`, and players such as
/// Jellyfin read a stale `BPS` as the new bitrate. ffmpeg's MKV writer adds
/// its own `DURATION` for the new track. `spec` is the output stream
/// specifier, like `v:0`, `a:1` or `s:0`; an empty value removes
/// the tag and leaves every other one (title, language, ...) alone.
fn clear_statistics_tags(spec: &str, source: &StreamInfo) -> Vec<String> {
    let mut args = Vec::new();
    for key in &source.statistics_tags {
        // A name with `=` or a line break can't be written as `KEY=`.
        if key.is_empty() || key.contains(['=', '\n', '\r']) || !is_statistics_tag(key) {
            continue;
        }
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
    fn even_scaling_matches_ffmpeg_minus_two() {
        assert_eq!(scaled_even(3840, 2160, 1080), 1920);
        assert_eq!(scaled_even(1920, 800, 720), 1728);
        assert_eq!(scaled_even(1998, 1080, 720), 1332);
        assert_eq!(scaled_even(639, 359, 240), 428);
        // scale_qsv's -1 would give 1719 here.
        assert_eq!(scaled_even(1920, 804, 720), 1720);
        assert_eq!(scaled_even(0, 0, 720), 2);
    }

    #[test]
    fn size_limits_are_resolution_classes() {
        let limit = |h| {
            size_limit(&TranscodeProfile {
                max_height: Some(h),
                ..TranscodeProfile::default()
            })
            .unwrap()
        };
        assert_eq!(
            limit(1080),
            SizeLimit {
                short: 1080,
                long: 1920
            }
        );
        assert_eq!(
            limit(720),
            SizeLimit {
                short: 720,
                long: 1280
            }
        );
        assert_eq!(
            limit(480),
            SizeLimit {
                short: 480,
                long: 854
            }
        );
        assert_eq!(
            limit(2160),
            SizeLimit {
                short: 2160,
                long: 3840
            }
        );
        assert_eq!(
            limit(719),
            SizeLimit {
                short: 718,
                long: 1278
            }
        );
        assert!(
            size_limit(&TranscodeProfile {
                max_height: Some(1),
                ..TranscodeProfile::default()
            })
            .is_none()
        );
        // Huge values do not overflow.
        assert!(limit(u32::MAX).long >= limit(u32::MAX).short);

        let hd = limit(1080);
        assert!(hd.fits(1920, 1080));
        assert!(hd.fits(1080, 1920), "portrait 1080p");
        assert!(hd.fits(1440, 1080));
        assert!(!hd.fits(3840, 2160));
        assert!(!hd.fits(2160, 3840));
        assert!(!hd.fits(3840, 1600), "scope 4K is a 4K file");
        assert!(
            !hd.fits(2560, 1080),
            "ultra-wide 1080 is a 1440p-class file"
        );
        assert!(hd.fits(1998, 1080), "flat 1080p is not worth a re-encode");
        assert!(hd.fits(2048, 1080), "DCI 2K");
        assert!(hd.fits(2304, 1080));
        assert!(!hd.fits(2306, 1080));
        assert_eq!(hd.fit(3840, 2160), (1920, 1080));
        assert_eq!(hd.fit(2160, 3840), (1080, 1920));
        assert_eq!(hd.fit(3840, 1600), (1920, 800));
        assert_eq!(hd.fit(1998, 1080), (1920, 1038));
        assert_eq!(hd.fit(2880, 2160), (1440, 1080));
        // A converted file always fits the limit it was made for.
        for (w, h) in [
            (3840, 2160),
            (2160, 3840),
            (3840, 1600),
            (4096, 2160),
            (1998, 1080),
        ] {
            let (ow, oh) = hd.fit(w, h);
            assert!(hd.fits(ow, oh), "{w}x{h} -> {ow}x{oh}");
            assert_eq!((ow % 2, oh % 2), (0, 0));
        }
        assert_eq!(limit(720).fit(1920, 804), (1280, 536));
        assert_eq!(limit(480).fit(1920, 1080), (854, 480));
    }

    fn surround(codec: &str, channels: u32, layout: &str) -> StreamInfo {
        StreamInfo {
            kind: Some(StreamKind::Audio),
            codec: codec.into(),
            channels: Some(channels),
            channel_layout: Some(layout.into()),
            sample_rate: Some(48_000),
            ..Default::default()
        }
    }

    #[test]
    fn one_layout_per_channel_count() {
        use AudioCodec::*;
        type Case = (AudioCodec, u32, &'static str, Option<(&'static str, u32)>);
        let cases: &[Case] = &[
            (Opus, 6, "5.1(side)", Some(("5.1", 6))),
            (Opus, 5, "5.0(side)", Some(("5.0", 5))),
            (Aac, 5, "5.0(side)", Some(("5.0", 5))),
            (Vorbis, 5, "5.0(side)", Some(("5.0", 5))),
            (Opus, 4, "4.0", Some(("quad", 4))),
            (Aac, 4, "quad", Some(("4.0", 4))),
            (Opus, 7, "6.1", Some(("6.1", 7))),
            (Aac, 7, "6.1", Some(("5.1", 6))),
            (Opus, 8, "7.1(wide)", Some(("7.1", 8))),
            (Aac, 8, "7.1(wide)", Some(("7.1", 8))),
            (Opus, 6, "hexagonal", Some(("5.1", 6))),
            (Opus, 3, "2.1", Some(("5.1", 6))),
            (Vorbis, 3, "2.1", Some(("5.1", 6))),
            (Opus, 3, "3.0(back)", Some(("3.0", 3))),
            (Opus, 10, "unknown", Some(("7.1", 8))),
            (Opus, 2, "stereo", None),
            (Aac, 1, "mono", None),
            (Ac3, 6, "5.1(side)", None),
            (Flac, 6, "5.1(side)", None),
        ];
        for &(codec, channels, layout, want) in cases {
            assert_eq!(
                standard_layout(codec, channels, Some(layout)),
                want,
                "{codec:?} {layout}"
            );
        }
    }

    #[test]
    fn dvd_five_channel_audio_stays_five_channels() {
        let mut notes = Vec::new();
        let args = encode_audio_args(
            0,
            &surround("ac3", 5, "5.0(side)"),
            AudioCodec::Aac,
            &mut notes,
        );
        let joined = args.join(" ");
        assert!(
            joined.contains("-filter:a:0 aformat=channel_layouts=5.0"),
            "{joined}"
        );
        assert!(!joined.contains('|'), "a single layout: {joined}");
        assert!(joined.contains("-b:a:0 384k"), "{joined}");
        assert!(notes.is_empty(), "{notes:?}");

        let args = encode_audio_args(1, &surround("dts", 7, "6.1"), AudioCodec::Aac, &mut notes);
        assert!(
            args.join(" ")
                .contains("-filter:a:1 aformat=channel_layouts=5.1")
        );
        assert_eq!(
            notes,
            ["Downmixed 6.1 audio to 5.1 because AAC has no standard 6.1 layout"]
        );

        notes.clear();
        let args = encode_audio_args(0, &surround("flac", 3, "2.1"), AudioCodec::Opus, &mut notes);
        let joined = args.join(" ");
        assert!(
            joined.contains("-b:a:0 320k"),
            "bitrate for 6 channels: {joined}"
        );
        assert!(joined.contains("-mapping_family:a:0 1"), "{joined}");
        assert!(notes[0].starts_with("Wrote 2.1 audio as 5.1"), "{notes:?}");
    }

    #[test]
    fn unknown_channel_counts_let_the_encoder_choose() {
        let mut notes = Vec::new();
        let mut late = surround("ac3", 0, "unknown");
        late.sample_rate = Some(0);
        let args = encode_audio_args(0, &late, AudioCodec::Opus, &mut notes);
        let joined = args.join(" ");
        assert!(!joined.contains("-b:a:0"), "{joined}");
        assert!(!joined.contains("-mapping_family"), "{joined}");
        assert!(joined.contains("-ar:a:0 48000"), "{joined}");
        assert!(
            joined.contains("-filter:a:0 aformat=channel_layouts=7.1|"),
            "{joined}"
        );
        let args = encode_audio_args(0, &late, AudioCodec::Ac3, &mut notes);
        assert!(!args.join(" ").contains("-ac:a:0"));
        assert!(notes.is_empty(), "{notes:?}");
    }

    #[test]
    fn matroska_subtitle_guard() {
        assert_eq!(
            subtitle_action(Container::Mkv, "subrip"),
            SubtitleAction::Copy
        );
        assert_eq!(
            subtitle_action(Container::Mkv, "hdmv_pgs_subtitle"),
            SubtitleAction::Copy
        );
        assert_eq!(
            subtitle_action(Container::Mkv, "xsub"),
            SubtitleAction::Drop
        );
        assert_eq!(
            subtitle_action(Container::Mkv, "microdvd"),
            SubtitleAction::Convert("srt")
        );
        assert_eq!(
            subtitle_action(Container::Mkv, "sami"),
            SubtitleAction::Convert("srt")
        );
        assert_eq!(
            subtitle_action(Container::Mkv, "ssa"),
            SubtitleAction::Convert("ass")
        );
        assert_eq!(
            subtitle_action(Container::Mkv, "mov_text"),
            SubtitleAction::Convert("srt")
        );
        // Other containers keep the core rules.
        assert_eq!(
            subtitle_action(Container::Mp4, "microdvd"),
            SubtitleAction::Convert("mov_text")
        );
        assert_eq!(
            subtitle_action(Container::Webm, "webvtt"),
            SubtitleAction::Copy
        );
        // Every text codec the core knows ends up somewhere Matroska can store.
        for codec in chrysopoeia_core::codec::TEXT_SUBTITLE_CODECS {
            match subtitle_action(Container::Mkv, codec) {
                SubtitleAction::Copy => assert!(
                    chrysopoeia_core::codec::MATROSKA_SUBTITLES.contains(codec),
                    "{codec}"
                ),
                SubtitleAction::Convert(to) => assert!(matches!(to, "srt" | "ass"), "{codec}"),
                SubtitleAction::Drop => panic!("{codec} dropped"),
            }
        }
    }

    #[test]
    fn lfe_detection() {
        assert!(layout_has_lfe("2.1"));
        assert!(layout_has_lfe("5.1(side)"));
        assert!(layout_has_lfe("FL+FR+LFE"));
        assert!(!layout_has_lfe("5.0(side)"));
        assert!(!layout_has_lfe("hexagonal"));
        assert!(!layout_has_lfe("quad"));
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
