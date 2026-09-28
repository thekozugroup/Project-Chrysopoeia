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
    // ffprobe can't tell .mp4 from .mov, so the reason names the family.
    assert_eq!(
        reason(decide(&p, &prof)),
        "Already H.264 in an MP4 or MOV file"
    );

    // Same codec, other container: convert.
    let p = probe_of("matroska", vec![video(0, "h264", 1920, 1080)]);
    assert_eq!(decide(&p, &prof), Decision::Transcode);

    // MKV and WebM share the "matroska" demuxer name.
    let p = probe_of("matroska", vec![video(0, "vp9", 1280, 720)]);
    let webm = profile(VideoCodec::Vp9, AudioCodec::Opus, Container::Webm);
    assert_eq!(
        reason(decide(&p, &webm)),
        "Already VP9 in a WebM or MKV file"
    );
    let mkv = profile(VideoCodec::Vp9, AudioCodec::Opus, Container::Mkv);
    assert_eq!(reason(decide(&p, &mkv)), "Already VP9 in an MKV file");
}

#[test]
fn same_format_files_that_do_not_play_everywhere_are_converted() {
    let compatible =
        chrysopoeia_core::TranscodeProfile::from_goal(chrysopoeia_core::Goal::Compatible);
    // Camera footage: 4:2:2 10-bit H.264.
    let mut camera = video(0, "h264", 1920, 1080);
    camera.pix_fmt = Some("yuv422p10le".into());
    camera.profile = Some("High 4:2:2".into());
    let p = probe_of("mov", vec![camera, audio(1, "aac", 2, 48_000, None)]);
    assert_eq!(decide(&p, &compatible), Decision::Transcode);

    // 10-bit 4:2:0 H.264 (High 10) is no better.
    let p = probe_of(
        "mov",
        vec![
            video_10bit(0, "h264", 1920, 1080),
            audio(1, "aac", 2, 48_000, None),
        ],
    );
    assert_eq!(decide(&p, &compatible), Decision::Transcode);

    // A camera .MOV with PCM sound: MP4 can't hold the audio as it is.
    let p = probe_of(
        "mov",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "pcm_s16le", 2, 48_000, None),
        ],
    );
    assert_eq!(decide(&p, &compatible), Decision::Transcode);

    // 10-bit 4:2:0 HEVC is a normal HEVC file.
    let hevc = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let p = probe_of("matroska", vec![video_10bit(0, "hevc", 3840, 2160)]);
    assert!(matches!(decide(&p, &hevc), Decision::Skip { .. }));
    // A track the language filter removes does not force a conversion.
    let mut filtered = profile(VideoCodec::H264, AudioCodec::Aac, Container::Mp4);
    filtered.audio_languages = vec!["eng".into()];
    let p = probe_of(
        "mov",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "aac", 2, 48_000, Some("eng")),
            audio(2, "pcm_s16le", 2, 48_000, Some("ger")),
        ],
    );
    assert!(matches!(decide(&p, &filtered), Decision::Skip { .. }));
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

    // Dolby Vision (profile 8.1: an HDR10 base layer) flagged by the scanner.
    let mut dv = hdr10(video_10bit(0, "hevc", 3840, 2160));
    dv.hdr = Some(HdrFormat::DolbyVision);
    let p = probe_of("matroska", vec![dv]);
    assert!(matches!(decide(&p, &prof), Decision::Skip { .. }));
}

#[test]
fn dolby_vision_without_a_standard_layer_is_skipped() {
    // Profile 5: no HDR10/HLG/SDR base layer, colour description unset.
    let mut dv5 = video_10bit(0, "hevc", 3840, 2160);
    dv5.hdr = Some(HdrFormat::DolbyVision);
    let p = probe_of("matroska", vec![dv5.clone()]);
    let save = chrysopoeia_core::TranscodeProfile::from_goal(chrysopoeia_core::Goal::SaveSpace);
    let reason_text = "Dolby Vision video without a standard HDR layer — left unchanged";
    assert_eq!(reason(decide(&p, &save)), reason_text);
    let hevc_mp4 = profile(VideoCodec::Hevc, AudioCodec::Aac, Container::Mp4);
    assert_eq!(reason(decide(&p, &hevc_mp4)), reason_text);
    // "unknown" is no better than no tag.
    dv5.color_transfer = Some("unknown".into());
    let p = probe_of("matroska", vec![dv5]);
    assert_eq!(reason(decide(&p, &save)), reason_text);
    // A downscale request does not override it.
    let mut small = save.clone();
    small.max_height = Some(1080);
    assert_eq!(reason(decide(&p, &small)), reason_text);

    // Profile 8.1 (HDR10 base layer) and 8.4 (HLG) are converted.
    let mut dv81 = hdr10(video_10bit(0, "hevc", 3840, 2160));
    dv81.hdr = Some(HdrFormat::DolbyVision);
    let p = probe_of("matroska", vec![dv81]);
    assert_eq!(decide(&p, &save), Decision::Transcode);
    let mut dv84 = video_10bit(0, "hevc", 3840, 2160);
    dv84.hdr = Some(HdrFormat::DolbyVision);
    dv84.color_transfer = Some("arib-std-b67".into());
    let p = probe_of("matroska", vec![dv84]);
    assert_eq!(decide(&p, &save), Decision::Transcode);
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
fn short_files_are_skipped_and_unknown_durations_converted() {
    let prof = efficient(VideoCodec::Av1);
    let mut p = probe_of("matroska", vec![video(0, "h264", 1920, 1080)]);
    p.duration_secs = Some(0.4);
    assert_eq!(
        reason(decide(&p, &prof)),
        "Shorter than a second — nothing worth converting"
    );
    p.duration_secs = Some(0.0);
    assert!(matches!(decide(&p, &prof), Decision::Skip { .. }));
    // Matroska written to a pipe has no duration; converting it fixes that.
    p.duration_secs = None;
    assert_eq!(decide(&p, &prof), Decision::Transcode);
    p.duration_secs = Some(f64::NAN);
    assert_eq!(decide(&p, &prof), Decision::Transcode);
    p.duration_secs = Some(1.0);
    assert_eq!(decide(&p, &prof), Decision::Transcode);
}

#[test]
fn files_whose_audio_cannot_be_read_are_skipped() {
    let prof = efficient(VideoCodec::Av1);
    let mut unknown = audio(1, "none", 2, 48_000, None);
    unknown.codec = "unknown".into();
    let p = probe_of(
        "matroska",
        vec![video(0, "h264", 1920, 1080), unknown.clone()],
    );
    assert_eq!(
        reason(decide(&p, &prof)),
        "Couldn't read this file's audio — left unchanged"
    );
    // One readable track is enough.
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            unknown,
            audio(2, "aac", 2, 48_000, None),
        ],
    );
    assert_eq!(decide(&p, &prof), Decision::Transcode);
    // A track the quick probe could not fully read (it starts late in a
    // transport stream) is not a reason to skip.
    let p = probe_of(
        "mpegts",
        vec![
            video(0, "mpeg2video", 720, 576),
            audio(1, "ac3", 0, 0, None),
        ],
    );
    assert_eq!(decide(&p, &prof), Decision::Transcode);
    // Video without any audio is fine.
    let p = probe_of("matroska", vec![video(0, "h264", 1920, 1080)]);
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
fn size_limit_works_in_both_orientations_and_never_reselects() {
    let mut prof = efficient(VideoCodec::Hevc);
    prof.max_height = Some(1080);
    let hevc = |w, h| probe_of("matroska", vec![video(0, "hevc", w, h)]);
    // Portrait 1080p (a converted phone clip) is within the limit.
    assert_eq!(reason(decide(&hevc(1080, 1920), &prof)), "Already HEVC");
    // Portrait 4K and scope 4K are not.
    assert_eq!(decide(&hevc(2160, 3840), &prof), Decision::Transcode);
    assert_eq!(decide(&hevc(3840, 1600), &prof), Decision::Transcode);
    // What the planner writes for them is within the limit.
    let limit = chrysopoeia_worker::plan::size_limit(&prof).expect("limit");
    for (w, h) in [(2160, 3840), (3840, 1600), (3840, 2160), (4096, 1716)] {
        let (ow, oh) = limit.fit(w, h);
        assert_eq!(
            reason(decide(&hevc(ow, oh), &prof)),
            "Already HEVC",
            "{w}x{h} -> {ow}x{oh}"
        );
        // Rotated players swap the sides; still within the limit.
        assert_eq!(reason(decide(&hevc(oh, ow), &prof)), "Already HEVC");
    }
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
    assert_eq!(
        reason(decide(&h264, &compatible)),
        "Already H.264 in an MP4 or MOV file"
    );
    assert_eq!(decide(&hevc, &compatible), Decision::Transcode);
}
