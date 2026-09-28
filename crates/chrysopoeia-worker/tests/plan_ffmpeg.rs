//! Run real ffmpeg with `build_plan`'s arguments over a synthetic library and
//! check the results with ffprobe.
//!
//! Covers every software encoder, every container and every audio target at
//! least once, and every kind of test input at least once. Hardware paths are
//! covered by the argument tests in `plan_args.rs` (there is no GPU here).
//! Each test skips (with a message) when ffmpeg, ffprobe or the encoder is
//! missing.

mod common;

use std::path::Path;
use std::process::Command;

use chrysopoeia_core::{AudioCodec, Container, TranscodeProfile, VideoCodec};
use chrysopoeia_worker::{PlanRequest, build_plan};
use common::*;

const MP4_TWO_AAC: &str = "Movies/Big Test (2020)/Big Test (2020).mp4";
const MKV_SURROUND_SRT: &str = "TV/Show/Season 01/Show - S01E01.mkv";
const MKV_HEVC_10BIT: &str = "TV/Show/Season 01/Show - S01E02.mkv";
const TS_INTERLACED: &str = "TV/Show/Season 01/Show - S01E03.ts";
const AVI_MPEG4: &str = "Movies/Old Home Video.avi";
const WEBM_ODD: &str = "Odd Size.webm";
const MKV_HDR10: &str = "HDR10.mkv";
const MKV_STYLED_FONT: &str = "Styled.mkv";
const MP4_COVER_ART: &str = "Cover.mp4";
const TS_LATE_AUDIO: &str = "Late Audio.ts";
const MOV_LAYOUTS: &str = "Layouts.mov";
const MOV_CAMERA: &str = "Camera.mov";
const MP4_ROTATED: &str = "Phone.mp4";
const MKV_PIPED: &str = "Piped.mkv";
const MKV_DISPOSITIONS: &str = "Dispositions.mkv";

/// Channels each `Layouts.mov` track must keep, per target codec (see
/// `LAYOUT_SAMPLE`): Opus and Vorbis have a layout for every count, 2.1
/// becomes 5.1 to keep its bass channel, and AAC turns 6.1 into 5.1.
const LAYOUT_CHANNELS_VORBIS_ORDER: &[u32] = &[5, 4, 4, 7, 8, 6];
const LAYOUT_CHANNELS_AAC: &[u32] = &[5, 4, 4, 6, 8, 6];

/// One conversion and what its output must look like.
struct Case {
    input: &'static str,
    encoder: &'static str,
    container: Container,
    audio: AudioCodec,
    max_height: Option<u32>,
    audio_languages: &'static [&'static str],
    /// ffprobe codec names of the output audio tracks, in order.
    expect_audio: &'static [&'static str],
    /// ffprobe codec names of the output subtitle tracks, in order.
    expect_subs: &'static [&'static str],
    expect_pix_fmt: Option<&'static str>,
    expect_size: Option<(u32, u32)>,
    /// Font/other attachments the output must carry.
    expect_attachments: usize,
    /// Colour primaries, transfer and matrix the output must be tagged with.
    expect_colour: Option<(&'static str, &'static str, &'static str)>,
    /// Channel count of each output audio track, when it matters.
    expect_channels: Option<&'static [u32]>,
}

impl Case {
    const fn new(
        input: &'static str,
        encoder: &'static str,
        container: Container,
        audio: AudioCodec,
    ) -> Self {
        Self {
            input,
            encoder,
            container,
            audio,
            max_height: None,
            audio_languages: &[],
            expect_audio: &[],
            expect_subs: &[],
            expect_pix_fmt: None,
            expect_size: None,
            expect_attachments: 0,
            expect_colour: None,
            expect_channels: None,
        }
    }
}

fn run_cases(encoder: &str, cases: &[Case]) {
    if !media_tools_available() {
        return;
    }
    if !has_encoder(encoder) {
        eprintln!("skipping: this ffmpeg has no {encoder}");
        return;
    }
    let Some(media) = media_dir() else {
        return;
    };
    let out_dir = tempfile::tempdir().expect("temp dir");
    let mut failures = Vec::new();
    for (i, case) in cases.iter().enumerate() {
        if let Err(e) = run_case(media, out_dir.path(), i, case) {
            failures.push(format!(
                "{} -> {} {} {:?}: {e}",
                case.input,
                case.encoder,
                case.container.label(),
                case.audio
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) failed:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

fn run_case(media: &Path, out_dir: &Path, i: usize, case: &Case) -> Result<(), String> {
    let input = media.join(case.input);
    let source = probe_file(&input);
    let encoder = candidate(case.encoder, false);
    let mut prof: TranscodeProfile = profile(encoder.codec, case.audio, case.container);
    prof.max_height = case.max_height;
    prof.audio_languages = case.audio_languages.iter().map(|s| s.to_string()).collect();
    let output = out_dir.join(format!("case{i}.{}", case.container.extension()));

    let plan = build_plan(&PlanRequest {
        input: &input,
        output: &output,
        probe: &source,
        profile: &prof,
        encoder: &encoder,
    })
    .map_err(|e| format!("build_plan: {e:#}"))?;
    let warnings = run_ffmpeg(&plan.args).map_err(|e| format!("{e}\nargs: {:?}", plan.args))?;

    let out = probe_file(&output);
    let fail = |what: String| {
        Err(format!(
            "{what}\nargs: {:?}\nnotes: {:?}",
            plan.args, plan.notes
        ))
    };
    // Encoders relabel layouts they don't support ("output stream will have
    // incorrect channel layout"); the plan must never rely on that.
    if warnings.contains("channel layout") {
        return fail(format!("ffmpeg warned about channel layouts:\n{warnings}"));
    }

    // Stream counts match what the plan promised verification.
    let videos: Vec<_> = out
        .streams
        .iter()
        .filter(|s| s.kind == Some(chrysopoeia_core::StreamKind::Video) && !s.is_attached_pic)
        .collect();
    let audio: Vec<&str> = out.audio_streams().map(|s| s.codec.as_str()).collect();
    let subs: Vec<&str> = out.subtitle_streams().map(|s| s.codec.as_str()).collect();
    let counts = (videos.len(), audio.len(), subs.len());
    let expected = (
        plan.expected.video as usize,
        plan.expected.audio as usize,
        plan.expected.subtitle as usize,
    );
    if counts != expected {
        return fail(format!(
            "stream counts {counts:?}, plan expected {expected:?}"
        ));
    }
    if audio != case.expect_audio {
        return fail(format!(
            "audio codecs {audio:?}, wanted {:?}",
            case.expect_audio
        ));
    }
    if subs != case.expect_subs {
        return fail(format!(
            "subtitle codecs {subs:?}, wanted {:?}",
            case.expect_subs
        ));
    }

    if let Some(wanted) = case.expect_channels {
        let got: Vec<u32> = out
            .audio_streams()
            .map(|s| s.channels.unwrap_or(0))
            .collect();
        if got != wanted {
            return fail(format!("audio channels {got:?}, wanted {wanted:?}"));
        }
    }

    // Cover art and data streams are never carried over.
    let all_video = out
        .streams
        .iter()
        .filter(|s| s.kind == Some(chrysopoeia_core::StreamKind::Video))
        .count();
    if all_video != 1 {
        return fail(format!(
            "{all_video} video streams (cover art must be dropped)"
        ));
    }
    // No data or timecode streams (MP4 turns a copied timecode tag into one
    // unless told not to).
    let others: Vec<&str> = out
        .streams
        .iter()
        .filter(|s| {
            !matches!(
                s.kind,
                Some(
                    chrysopoeia_core::StreamKind::Video
                        | chrysopoeia_core::StreamKind::Audio
                        | chrysopoeia_core::StreamKind::Subtitle
                        | chrysopoeia_core::StreamKind::Attachment
                )
            )
        })
        .map(|s| s.codec.as_str())
        .collect();
    if !others.is_empty() {
        return fail(format!("unexpected extra streams: {others:?}"));
    }
    let attachments = out
        .streams
        .iter()
        .filter(|s| s.kind == Some(chrysopoeia_core::StreamKind::Attachment))
        .count();
    if attachments != case.expect_attachments {
        return fail(format!(
            "{attachments} attachments, wanted {}",
            case.expect_attachments
        ));
    }

    let v = videos[0];
    if let Some((primaries, transfer, matrix)) = case.expect_colour {
        let got = (
            v.color_primaries.as_deref(),
            v.color_transfer.as_deref(),
            v.color_space.as_deref(),
        );
        if got != (Some(primaries), Some(transfer), Some(matrix)) {
            return fail(format!("colour tags {got:?}"));
        }
    }
    if v.codec != encoder.codec.ffprobe_name() {
        return fail(format!(
            "video codec {}, wanted {}",
            v.codec,
            encoder.codec.ffprobe_name()
        ));
    }
    if let Some(fmt) = case.expect_pix_fmt
        && v.pix_fmt.as_deref() != Some(fmt)
    {
        return fail(format!("pixel format {:?}, wanted {fmt}", v.pix_fmt));
    }
    if let Some((w, h)) = case.expect_size
        && (v.width, v.height) != (Some(w), Some(h))
    {
        return fail(format!("size {:?}x{:?}, wanted {w}x{h}", v.width, v.height));
    }
    if v.interlaced {
        return fail("output is still interlaced".into());
    }
    // Compare video lengths (container durations include subtitle tracks
    // in MKV but not in MP4, so they differ legitimately).
    let (src_len, out_len) = (video_length(&input), video_length(&output));
    if src_len < 2.0 {
        return fail(format!("the sample itself is only {src_len:.2} s long"));
    }
    if (src_len - out_len).abs() > 0.25 {
        return fail(format!("video lasts {out_len:.2} s, source {src_len:.2} s"));
    }
    if case.container == Container::Mp4 && encoder.codec == VideoCodec::Hevc {
        let tag = codec_tag(&output);
        if tag != "hvc1" {
            return fail(format!("HEVC in MP4 tagged {tag:?}, wanted hvc1"));
        }
    }
    // The file decodes end to end without errors.
    // (Subtitles are stream-copied: the null muxer has no subtitle encoder.)
    let decode = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-v", "error", "-i"])
        .arg(&output)
        .args(["-map", "0", "-c:s", "copy", "-f", "null", "-"])
        .output()
        .map_err(|e| e.to_string())?;
    let errors = String::from_utf8_lossy(&decode.stderr);
    if !decode.status.success() || !errors.trim().is_empty() {
        return fail(format!("decode errors: {errors}"));
    }
    Ok(())
}

/// Length of the first video stream from its packet timestamps.
fn video_length(path: &Path) -> f64 {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "packet=pts_time,duration_time",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    let (mut first, mut end) = (f64::MAX, f64::MIN);
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let mut parts = line.split(',');
        let pts: f64 = parts
            .next()
            .and_then(|p| p.parse().ok())
            .unwrap_or(f64::NAN);
        let dur: f64 = parts.next().and_then(|d| d.parse().ok()).unwrap_or(0.0);
        if pts.is_finite() {
            first = first.min(pts);
            end = end.max(pts + dur);
        }
    }
    if end > first { end - first } else { 0.0 }
}

fn codec_tag(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_tag_string",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    out.unwrap_or_default()
}

#[test]
fn svt_av1() {
    run_cases(
        "libsvtav1",
        &[
            // 1080p H.264 + 5.1(side) AC-3 + SRT: Opus surround, downscale.
            Case {
                max_height: Some(720),
                expect_audio: &["opus"],
                expect_channels: Some(&[6]),
                expect_subs: &["subrip"],
                expect_size: Some((1280, 720)),
                ..Case::new(
                    MKV_SURROUND_SRT,
                    "libsvtav1",
                    Container::Mkv,
                    AudioCodec::Opus,
                )
            },
            // 10-bit HEVC keeps 10 bits; AAC is copied.
            Case {
                expect_audio: &["aac"],
                expect_pix_fmt: Some("yuv420p10le"),
                ..Case::new(
                    MKV_HEVC_10BIT,
                    "libsvtav1",
                    Container::Mkv,
                    AudioCodec::Copy,
                )
            },
            // Two AAC tracks, Japanese only, into MP4.
            Case {
                audio_languages: &["jpn"],
                expect_audio: &["aac"],
                expect_pix_fmt: Some("yuv420p"),
                ..Case::new(MP4_TWO_AAC, "libsvtav1", Container::Mp4, AudioCodec::Aac)
            },
            // HDR10 keeps 10 bits and its colour description.
            Case {
                expect_pix_fmt: Some("yuv420p10le"),
                expect_colour: Some(("bt2020", "smpte2084", "bt2020nc")),
                ..Case::new(MKV_HDR10, "libsvtav1", Container::Mkv, AudioCodec::Copy)
            },
            // Every awkward layout to Opus keeps its channels (no padding).
            Case {
                expect_audio: &["opus"; 6],
                expect_channels: Some(LAYOUT_CHANNELS_VORBIS_ORDER),
                ..Case::new(MOV_LAYOUTS, "libsvtav1", Container::Mkv, AudioCodec::Opus)
            },
            // Matroska without a duration converts normally.
            Case {
                expect_audio: &["aac"],
                ..Case::new(MKV_PIPED, "libsvtav1", Container::Mkv, AudioCodec::Copy)
            },
        ],
    );
}

#[test]
fn aom_av1() {
    run_cases(
        "libaom-av1",
        &[
            // AVI (genpts) with MP3 to FLAC.
            Case {
                expect_audio: &["flac"],
                expect_size: Some((638, 358)),
                ..Case::new(AVI_MPEG4, "libaom-av1", Container::Mkv, AudioCodec::Flac)
            },
            // Odd-sized VP9 made even; Vorbis copied into WebM.
            Case {
                expect_audio: &["vorbis"],
                expect_size: Some((638, 358)),
                ..Case::new(WEBM_ODD, "libaom-av1", Container::Webm, AudioCodec::Copy)
            },
        ],
    );
}

#[test]
fn x265() {
    run_cases(
        "libx265",
        &[
            // Surround AC-3 to AAC in MP4, SRT to mov_text, hvc1 tag, downscale.
            Case {
                max_height: Some(720),
                expect_audio: &["aac"],
                expect_subs: &["mov_text"],
                expect_size: Some((1280, 720)),
                ..Case::new(MKV_SURROUND_SRT, "libx265", Container::Mp4, AudioCodec::Aac)
            },
            // Interlaced MPEG-2 in TS: deinterlaced; 44.1 kHz MP2 to AC-3.
            Case {
                expect_audio: &["ac3"],
                expect_size: Some((720, 576)),
                ..Case::new(TS_INTERLACED, "libx265", Container::Mkv, AudioCodec::Ac3)
            },
            // 10-bit stays 10-bit; E-AC-3 audio.
            Case {
                expect_audio: &["eac3"],
                expect_pix_fmt: Some("yuv420p10le"),
                ..Case::new(MKV_HEVC_10BIT, "libx265", Container::Mkv, AudioCodec::Eac3)
            },
            // HDR10 with x265's HDR options, into MP4.
            Case {
                expect_pix_fmt: Some("yuv420p10le"),
                expect_colour: Some(("bt2020", "smpte2084", "bt2020nc")),
                ..Case::new(MKV_HDR10, "libx265", Container::Mp4, AudioCodec::Copy)
            },
        ],
    );
}

#[test]
fn x264() {
    run_cases(
        "libx264",
        &[
            // Interlaced TS into MP4: MP2 can't be kept there, so it becomes AAC.
            Case {
                expect_audio: &["aac"],
                ..Case::new(TS_INTERLACED, "libx264", Container::Mp4, AudioCodec::Copy)
            },
            // Both AAC tracks to MP3.
            Case {
                expect_audio: &["mp3", "mp3"],
                ..Case::new(MP4_TWO_AAC, "libx264", Container::Mkv, AudioCodec::Mp3)
            },
            // 10-bit HEVC to 8-bit H.264 in MP4.
            Case {
                expect_audio: &["aac"],
                expect_pix_fmt: Some("yuv420p"),
                ..Case::new(MKV_HEVC_10BIT, "libx264", Container::Mp4, AudioCodec::Aac)
            },
            // Surround AC-3 + SRT, everything kept, into MKV.
            Case {
                expect_audio: &["ac3"],
                expect_subs: &["subrip"],
                max_height: Some(480),
                expect_size: Some((854, 480)),
                ..Case::new(
                    MKV_SURROUND_SRT,
                    "libx264",
                    Container::Mkv,
                    AudioCodec::Copy,
                )
            },
            // Styled ASS and its font stay together in MKV.
            Case {
                expect_audio: &["aac"],
                expect_subs: &["ass"],
                expect_attachments: 1,
                ..Case::new(MKV_STYLED_FONT, "libx264", Container::Mkv, AudioCodec::Copy)
            },
            // In MP4 the ASS becomes plain mov_text and the font is dropped.
            Case {
                expect_audio: &["aac"],
                expect_subs: &["mov_text"],
                ..Case::new(MKV_STYLED_FONT, "libx264", Container::Mp4, AudioCodec::Copy)
            },
            // 5.1 AC-3 downmixed to stereo MP3 (and the SRT kept).
            Case {
                max_height: Some(360),
                expect_audio: &["mp3"],
                expect_channels: Some(&[2]),
                expect_subs: &["subrip"],
                ..Case::new(MKV_SURROUND_SRT, "libx264", Container::Mkv, AudioCodec::Mp3)
            },
            // Cover art is not a second video stream in the output.
            Case {
                expect_audio: &["opus"],
                ..Case::new(MP4_COVER_ART, "libx264", Container::Mkv, AudioCodec::Opus)
            },
            // The only audio starts late: the quick probe can't read it, but
            // it must not be dropped (a silent file would pass verification).
            Case {
                expect_audio: &["opus"],
                expect_channels: Some(&[1]),
                ..Case::new(TS_LATE_AUDIO, "libx264", Container::Mkv, AudioCodec::Opus)
            },
            // Camera MOV to MP4: PCM becomes AAC and no timecode track appears.
            Case {
                expect_audio: &["aac"],
                ..Case::new(MOV_CAMERA, "libx264", Container::Mp4, AudioCodec::Aac)
            },
            // A rotated phone clip is limited as the portrait picture it is.
            Case {
                max_height: Some(480),
                expect_audio: &["aac"],
                expect_size: Some((480, 854)),
                ..Case::new(MP4_ROTATED, "libx264", Container::Mkv, AudioCodec::Copy)
            },
            // Awkward layouts to AAC: standard configurations only.
            Case {
                expect_audio: &["aac"; 6],
                expect_channels: Some(LAYOUT_CHANNELS_AAC),
                ..Case::new(MOV_LAYOUTS, "libx264", Container::Mp4, AudioCodec::Aac)
            },
        ],
    );
}

#[test]
fn vpx_vp9() {
    run_cases(
        "libvpx-vp9",
        &[
            // AAC can't be copied into WebM: both tracks become Opus.
            Case {
                expect_audio: &["opus", "opus"],
                ..Case::new(MP4_TWO_AAC, "libvpx-vp9", Container::Webm, AudioCodec::Copy)
            },
            // AVI to MKV with Vorbis.
            Case {
                expect_audio: &["vorbis"],
                ..Case::new(AVI_MPEG4, "libvpx-vp9", Container::Mkv, AudioCodec::Vorbis)
            },
            // 10-bit VP9 (Profile 2) with Opus in WebM.
            Case {
                expect_audio: &["opus"],
                expect_pix_fmt: Some("yuv420p10le"),
                ..Case::new(
                    MKV_HEVC_10BIT,
                    "libvpx-vp9",
                    Container::Webm,
                    AudioCodec::Opus,
                )
            },
            // Surround to Opus and SRT to WebVTT in WebM.
            Case {
                max_height: Some(360),
                expect_audio: &["opus"],
                expect_subs: &["webvtt"],
                expect_size: Some((640, 360)),
                ..Case::new(
                    MKV_SURROUND_SRT,
                    "libvpx-vp9",
                    Container::Webm,
                    AudioCodec::Opus,
                )
            },
            // Every awkward layout to Vorbis (which relabels what it doesn't
            // know instead of failing).
            Case {
                expect_audio: &["vorbis"; 6],
                expect_channels: Some(LAYOUT_CHANNELS_VORBIS_ORDER),
                ..Case::new(
                    MOV_LAYOUTS,
                    "libvpx-vp9",
                    Container::Webm,
                    AudioCodec::Vorbis,
                )
            },
        ],
    );
}

/// Disposition flags of each audio stream, e.g. `["default", "original"]`.
fn audio_dispositions(path: &Path) -> Vec<Vec<String>> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a",
            "-show_entries",
            "stream_disposition",
            "-of",
            "json",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("ffprobe JSON");
    json["streams"]
        .as_array()
        .map(|streams| {
            streams
                .iter()
                .map(|s| {
                    s["disposition"]
                        .as_object()
                        .map(|d| {
                            d.iter()
                                .filter(|(_, v)| v.as_i64() == Some(1))
                                .map(|(k, _)| k.clone())
                                .collect()
                        })
                        .unwrap_or_default()
                })
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_promoted_default_track_keeps_its_other_flags() {
    if !media_tools_available() || !has_encoder("libx264") {
        return;
    }
    let Some(media) = media_dir() else {
        return;
    };
    let input = media.join(MKV_DISPOSITIONS);
    let out_dir = tempfile::tempdir().expect("temp dir");
    let output = out_dir.path().join("out.mkv");
    let mut prof = profile(VideoCodec::H264, AudioCodec::Copy, Container::Mkv);
    prof.audio_languages = vec!["eng".into()];
    let source = probe_file(&input);
    let plan = build_plan(&PlanRequest {
        input: &input,
        output: &output,
        probe: &source,
        profile: &prof,
        encoder: &software(VideoCodec::H264),
    })
    .expect("plan");
    run_ffmpeg(&plan.args).expect("ffmpeg runs");
    let flags = audio_dispositions(&output);
    assert_eq!(flags.len(), 1, "{flags:?}");
    for flag in ["default", "original", "visual_impaired"] {
        assert!(
            flags[0].iter().any(|f| f == flag),
            "{flag} missing: {flags:?}"
        );
    }
}

#[test]
fn the_late_audio_sample_really_fools_a_quick_probe() {
    if !media_tools_available() {
        return;
    }
    let Some(media) = media_dir() else {
        return;
    };
    // If ffprobe ever reads this track fully, the x264 case above no longer
    // covers the "unconfirmed only" path; keep the sample honest.
    let probe = probe_file(&media.join(TS_LATE_AUDIO));
    let audio: Vec<_> = probe.audio_streams().collect();
    assert_eq!(audio.len(), 1);
    assert_eq!(audio[0].channels, Some(0), "{:?}", audio[0]);
}

#[test]
fn decide_agrees_with_the_sample_library() {
    use chrysopoeia_core::{Goal, TranscodeProfile};
    use chrysopoeia_worker::{Decision, decide};
    if !media_tools_available() {
        return;
    }
    let Some(media) = media_dir() else {
        return;
    };
    let balanced = TranscodeProfile::from_goal(Goal::Balanced);
    let save = TranscodeProfile::from_goal(Goal::SaveSpace);
    let hevc = probe_file(&media.join(MKV_HEVC_10BIT));
    assert_eq!(
        decide(&hevc, &balanced),
        Decision::Skip {
            reason: "Already HEVC".into()
        }
    );
    assert_eq!(decide(&hevc, &save), Decision::Transcode);
    let flac = probe_file(&media.join("Music/Tone.flac"));
    assert!(matches!(decide(&flac, &save), Decision::Skip { .. }));
    for input in [
        MP4_TWO_AAC,
        MKV_SURROUND_SRT,
        TS_INTERLACED,
        AVI_MPEG4,
        WEBM_ODD,
        MKV_HDR10,
        MKV_STYLED_FONT,
        MP4_COVER_ART,
        TS_LATE_AUDIO,
        MOV_CAMERA,
        MP4_ROTATED,
        MKV_PIPED,
    ] {
        let probe = probe_file(&media.join(input));
        assert_eq!(decide(&probe, &save), Decision::Transcode, "{input}");
    }
    // Matroska from a pipe has no duration, and is converted anyway.
    assert_eq!(probe_file(&media.join(MKV_PIPED)).duration_secs, None);
    // "Plays everywhere": a camera MOV with PCM sound is not already done.
    let compatible = TranscodeProfile::from_goal(Goal::Compatible);
    let camera = probe_file(&media.join(MOV_CAMERA));
    assert_eq!(decide(&camera, &compatible), Decision::Transcode);
    // The rotated 720p clip is within a 720 limit in either orientation.
    let mut hd = balanced.clone();
    hd.max_height = Some(720);
    hd.skip_efficient = true;
    hd.video_codec = VideoCodec::H264;
    let phone = probe_file(&media.join(MP4_ROTATED));
    assert!(matches!(decide(&phone, &hd), Decision::Skip { .. }));
    // The 10-bit HEVC sample has no bits_per_raw_sample; the pixel format
    // alone must mark it as 10-bit.
    let v = hevc.primary_video().expect("video");
    assert_eq!(v.pix_fmt.as_deref(), Some("yuv420p10le"));
    let interlaced = probe_file(&media.join(TS_INTERLACED));
    assert!(interlaced.primary_video().is_some_and(|v| v.interlaced));
}
