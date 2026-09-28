//! `decide`: every skip rule and the cases that must still be converted.

mod common;

use chrysopoeia_core::{AudioCodec, Container, HdrFormat, StreamInfo, StreamKind, VideoCodec};
use chrysopoeia_worker::{Decision, decide};
use common::*;

fn reason(d: Decision) -> String {
    match d {
        Decision::Skip { reason } => reason,
        Decision::Transcode => panic!("expected a skip, got Transcode"),
    }
}

fn efficient(target: VideoCodec) -> chrysopoeia_core::TranscodeProfile {
    let mut p = profile(target, AudioCodec::Copy, Container::Mkv);
    p.skip_efficient = true;
    p
}

#[test]
fn audio_only_files_are_skipped() {
    let p = probe_of("flac", vec![audio(0, "flac", 2, 44_100, None)]);
    assert_eq!(
        reason(decide(&p, &efficient(VideoCodec::Hevc))),
        "Audio-only file — nothing to convert"
    );
}

#[test]
fn cover_art_is_not_video() {
    let cover = StreamInfo {
        index: 1,
        kind: Some(StreamKind::Video),
        codec: "mjpeg".into(),
        is_attached_pic: true,
        width: Some(600),
        height: Some(600),
        ..Default::default()
    };
    let p = probe_of("mp3", vec![audio(0, "mp3", 2, 44_100, None), cover]);
    assert_eq!(
        reason(decide(&p, &efficient(VideoCodec::Av1))),
        "Audio-only file — nothing to convert"
    );
}

#[test]
fn files_without_any_stream_are_skipped() {
    let p = probe_of("mov", vec![]);
    assert!(reason(decide(&p, &efficient(VideoCodec::Av1))).contains("No video"));
}

#[test]
fn more_efficient_sources_are_skipped() {
    let p = probe_of("matroska", vec![video(0, "av1", 1920, 1080)]);
    assert_eq!(
        reason(decide(&p, &efficient(VideoCodec::Hevc))),
        "Already AV1, which is more efficient than HEVC"
    );
}

#[test]
fn same_codec_is_skipped_with_a_short_reason() {
    let p = probe_of("matroska", vec![video(0, "hevc", 1920, 1080)]);
    assert_eq!(
        reason(decide(&p, &efficient(VideoCodec::Hevc))),
        "Already HEVC"
    );
}

#[test]
fn equally_efficient_sources_are_skipped() {
    let p = probe_of("matroska", vec![video(0, "vp9", 1920, 1080)]);
    assert_eq!(
        reason(decide(&p, &efficient(VideoCodec::Hevc))),
        "Already VP9, which is as efficient as HEVC"
    );
}

#[test]
fn less_efficient_sources_are_converted() {
    for codec in ["h264", "mpeg2video", "mpeg4", "vc1", "msmpeg4v3"] {
        let p = probe_of("matroska", vec![video(0, codec, 1920, 1080)]);
        assert_eq!(
            decide(&p, &efficient(VideoCodec::Hevc)),
            Decision::Transcode,
            "{codec}"
        );
    }
    let p = probe_of("matroska", vec![video(0, "hevc", 1920, 1080)]);
    assert_eq!(decide(&p, &efficient(VideoCodec::Av1)), Decision::Transcode);
}

#[test]
fn exact_format_is_skipped_when_not_skipping_efficient() {
    // ffprobe reports "mov" for every MP4-family file.
    let p = probe_of("mov", vec![video(0, "h264", 1920, 1080)]);
    let prof = profile(VideoCodec::H264, AudioCodec::Aac, Container::Mp4);
    assert_eq!(reason(decide(&p, &prof)), "Already H.264 in MP4");

    // Same codec, other container: convert.
    let p = probe_of("matroska", vec![video(0, "h264", 1920, 1080)]);
    assert_eq!(decide(&p, &prof), Decision::Transcode);

    // MKV and WebM share the "matroska" demuxer name.
    let p = probe_of("matroska", vec![video(0, "vp9", 1280, 720)]);
    let webm = profile(VideoCodec::Vp9, AudioCodec::Opus, Container::Webm);
    assert_eq!(reason(decide(&p, &webm)), "Already VP9 in WebM");
}

#[test]
fn without_skip_efficient_better_codecs_are_still_converted() {
    let p = probe_of("matroska", vec![video(0, "av1", 1920, 1080)]);
    let prof = profile(VideoCodec::H264, AudioCodec::Aac, Container::Mp4);
    assert_eq!(decide(&p, &prof), Decision::Transcode);
}

#[test]
fn hdr_to_h264_is_skipped() {
    let prof = profile(VideoCodec::H264, AudioCodec::Aac, Container::Mp4);
    let p = probe_of("matroska", vec![hdr10(video_10bit(0, "hevc", 3840, 2160))]);
    assert_eq!(
        reason(decide(&p, &prof)),
        "HDR video would lose its colours as H.264 — left unchanged"
    );

    // HLG detected from the transfer alone.
    let mut hlg = video_10bit(0, "hevc", 3840, 2160);
    hlg.color_transfer = Some("arib-std-b67".into());
    let p = probe_of("matroska", vec![hlg]);
    assert!(matches!(decide(&p, &prof), Decision::Skip { .. }));

    // Dolby Vision flagged by the scanner.
    let mut dv = video_10bit(0, "hevc", 3840, 2160);
    dv.hdr = Some(HdrFormat::DolbyVision);
    let p = probe_of("matroska", vec![dv]);
    assert!(matches!(decide(&p, &prof), Decision::Skip { .. }));
}

#[test]
fn sdr_10bit_to_h264_and_hdr_to_hevc_are_converted() {
    let h264 = profile(VideoCodec::H264, AudioCodec::Aac, Container::Mp4);
    let p = probe_of("matroska", vec![video_10bit(0, "hevc", 1920, 1080)]);
    assert_eq!(decide(&p, &h264), Decision::Transcode);

    let av1 = profile(VideoCodec::Av1, AudioCodec::Opus, Container::Mkv);
    let p = probe_of("matroska", vec![hdr10(video_10bit(0, "hevc", 3840, 2160))]);
    assert_eq!(decide(&p, &av1), Decision::Transcode);
}

#[test]
fn short_or_unknown_duration_is_skipped() {
    let prof = efficient(VideoCodec::Av1);
    let mut p = probe_of("matroska", vec![video(0, "h264", 1920, 1080)]);
    p.duration_secs = Some(0.4);
    assert_eq!(
        reason(decide(&p, &prof)),
        "Shorter than a second — nothing worth converting"
    );
    p.duration_secs = None;
    assert!(reason(decide(&p, &prof)).contains("how long"));
    p.duration_secs = Some(f64::NAN);
    assert!(reason(decide(&p, &prof)).contains("how long"));
    p.duration_secs = Some(1.0);
    assert_eq!(decide(&p, &prof), Decision::Transcode);
}

#[test]
fn unknown_dimensions_are_skipped() {
    let prof = efficient(VideoCodec::Av1);
    let mut v = video(0, "h264", 0, 0);
    v.width = None;
    let p = probe_of("matroska", vec![v]);
    assert!(reason(decide(&p, &prof)).contains("picture size"));
    let p = probe_of("matroska", vec![video(0, "h264", 1920, 0)]);
    assert!(reason(decide(&p, &prof)).contains("picture size"));
}

#[test]
fn too_tall_efficient_sources_are_converted() {
    let mut prof = efficient(VideoCodec::Hevc);
    prof.max_height = Some(1080);
    let p = probe_of("matroska", vec![video(0, "hevc", 3840, 2160)]);
    assert_eq!(decide(&p, &prof), Decision::Transcode);
    // At or under the limit the usual rule applies.
    let p = probe_of("matroska", vec![video(0, "hevc", 1920, 1080)]);
    assert_eq!(reason(decide(&p, &prof)), "Already HEVC");
}

#[test]
fn default_goals_behave_as_documented() {
    use chrysopoeia_core::{Goal, TranscodeProfile};
    let h264 = probe_of(
        "mov",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "aac", 2, 48_000, None),
        ],
    );
    let hevc = probe_of("matroska", vec![video(0, "hevc", 1920, 1080)]);
    let save = TranscodeProfile::from_goal(Goal::SaveSpace);
    let balanced = TranscodeProfile::from_goal(Goal::Balanced);
    let compatible = TranscodeProfile::from_goal(Goal::Compatible);
    assert_eq!(decide(&h264, &save), Decision::Transcode);
    assert_eq!(decide(&hevc, &save), Decision::Transcode);
    assert_eq!(reason(decide(&hevc, &balanced)), "Already HEVC");
    assert_eq!(reason(decide(&h264, &compatible)), "Already H.264 in MP4");
    assert_eq!(decide(&hevc, &compatible), Decision::Transcode);
}
