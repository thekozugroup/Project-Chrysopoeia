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
    run_ffmpeg(&plan.args).map_err(|e| format!("{e}\nargs: {:?}", plan.args))?;

    let out = probe_file(&output);
    let fail = |what: String| {
        Err(format!(
            "{what}\nargs: {:?}\nnotes: {:?}",
            plan.args, plan.notes
        ))
    };

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
        ],
    );
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
    ] {
        let probe = probe_file(&media.join(input));
        assert_eq!(decide(&probe, &save), Decision::Transcode, "{input}");
    }
    // The 10-bit HEVC sample has no bits_per_raw_sample; the pixel format
    // alone must mark it as 10-bit.
    let v = hevc.primary_video().expect("video");
    assert_eq!(v.pix_fmt.as_deref(), Some("yuv420p10le"));
    let interlaced = probe_file(&media.join(TS_INTERLACED));
    assert!(interlaced.primary_video().is_some_and(|v| v.interlaced));
}
