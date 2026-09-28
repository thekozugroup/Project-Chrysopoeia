//! `build_plan`: argument construction for every hardware API, pixel formats,
//! filters, audio, subtitles, attachments and container flags.

mod common;

use std::path::Path;

use chrysopoeia_core::{AudioCodec, Container, StreamInfo, StreamKind, SubtitlePolicy, VideoCodec};
use chrysopoeia_worker::{PlanRequest, StreamSummary, build_plan};
use common::*;

/// A typical Blu-ray style source: 1080p H.264, English 5.1 AC-3 (default)
/// and Japanese stereo AAC, English SRT.
fn movie() -> chrysopoeia_core::ProbeInfo {
    let mut eng = audio(1, "ac3", 6, 48_000, Some("eng"));
    eng.is_default = true;
    probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            eng,
            audio(2, "aac", 2, 48_000, Some("jpn")),
            subtitle(3, "subrip", Some("eng")),
        ],
    )
}

// ---------------------------------------------------------------------------
// Structure and ordering

#[test]
fn software_plan_has_the_documented_shape() {
    let prof = profile(VideoCodec::Av1, AudioCodec::Opus, Container::Mkv);
    let plan = plan(&movie(), &prof, &software(VideoCodec::Av1));
    let a = &plan.args;

    assert_eq!(
        &a[..8],
        [
            "-hide_banner",
            "-nostdin",
            "-y",
            "-loglevel",
            "warning",
            "-nostats",
            "-progress",
            "pipe:1"
        ]
    );
    assert_before_input(a, &["-analyzeduration", "-probesize"]);
    assert_eq!(value(a, "-analyzeduration").as_deref(), Some("100M"));
    assert_eq!(value(a, "-probesize").as_deref(), Some("100M"));
    assert_eq!(value(a, "-i").as_deref(), Some("/media/in.mkv"));
    assert_eq!(values(a, "-map"), ["0:0", "0:1", "0:2", "0:3"]);
    assert!(pos(a, "-map").unwrap() > input_pos(a));
    assert!(has_pair(a, "-map_metadata", "0"));
    assert!(has_pair(a, "-map_chapters", "0"));
    assert!(has_pair(a, "-max_muxing_queue_size", "9999"));
    assert!(has_pair(a, "-c:v", "libsvtav1"));
    assert!(has_pair(a, "-pix_fmt", "yuv420p"));
    assert_absent(a, "-hwaccel");
    assert_absent(a, "-init_hw_device");
    assert_absent(a, "-vf");
    assert_absent(a, "-fflags");
    // Muxer and output come last.
    let n = a.len();
    assert_eq!(&a[n - 3..], ["-f", "matroska", "/tmp/out.mkv"]);
    assert_eq!(
        plan.expected,
        StreamSummary {
            video: 1,
            audio: 2,
            subtitle: 1
        }
    );
    assert!(plan.notes.is_empty(), "{:?}", plan.notes);
}

#[test]
fn only_the_primary_video_is_mapped() {
    let cover = StreamInfo {
        index: 1,
        kind: Some(StreamKind::Video),
        codec: "mjpeg".into(),
        is_attached_pic: true,
        ..Default::default()
    };
    let data = StreamInfo {
        index: 3,
        kind: Some(StreamKind::Data),
        codec: "bin_data".into(),
        ..Default::default()
    };
    let unknown = StreamInfo {
        index: 4,
        kind: None,
        codec: "tmcd".into(),
        ..Default::default()
    };
    let p = probe_of(
        "mov",
        vec![
            cover,
            video(0, "h264", 1280, 720),
            audio(2, "aac", 2, 48_000, None),
            data,
            unknown,
        ],
    );
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&p, &prof, &software(VideoCodec::Hevc));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:2"]);
    assert_eq!(plan.expected.video, 1);
}

#[test]
fn quality_args_follow_the_encoder() {
    let mut prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    prof.quality_override = Some(21);
    let plan = plan(&movie(), &prof, &software(VideoCodec::Hevc));
    assert!(has_pair(&plan.args, "-crf", "21"));
    assert!(has_pair(&plan.args, "-preset", "fast"));
    assert!(has_pair(&plan.args, "-x265-params", "log-level=error"));
}

#[test]
fn relative_paths_are_protected_and_bad_inputs_fail() {
    let prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    let enc = software(VideoCodec::Av1);
    let probe = movie();
    let plan = build_plan(&PlanRequest {
        input: Path::new("-weird: name.mkv"),
        output: Path::new("out.mkv"),
        probe: &probe,
        profile: &prof,
        encoder: &enc,
    })
    .unwrap();
    assert_eq!(
        value(&plan.args, "-i").as_deref(),
        Some("file:-weird: name.mkv")
    );
    assert_eq!(plan.args.last().map(String::as_str), Some("file:out.mkv"));

    // No video stream.
    let audio_only = probe_of("flac", vec![audio(0, "flac", 2, 44_100, None)]);
    let err = build_plan(&PlanRequest {
        input: Path::new("/a.flac"),
        output: Path::new("/b.mkv"),
        probe: &audio_only,
        profile: &prof,
        encoder: &enc,
    })
    .unwrap_err();
    assert!(err.to_string().contains("no video"), "{err}");

    // Encoder for another codec.
    let hevc = software(VideoCodec::Hevc);
    let err = build_plan(&PlanRequest {
        input: Path::new("/a.mkv"),
        output: Path::new("/b.mkv"),
        probe: &probe,
        profile: &prof,
        encoder: &hevc,
    })
    .unwrap_err();
    assert!(err.to_string().contains("libx265"), "{err}");

    // WebM cannot hold HEVC.
    let webm_hevc = profile(VideoCodec::Hevc, AudioCodec::Opus, Container::Webm);
    let err = build_plan(&PlanRequest {
        input: Path::new("/a.mkv"),
        output: Path::new("/b.webm"),
        probe: &probe,
        profile: &webm_hevc,
        encoder: &hevc,
    })
    .unwrap_err();
    assert!(err.to_string().contains("WebM"), "{err}");
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_fail_cleanly() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    let prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    let enc = software(VideoCodec::Av1);
    let probe = movie();
    let bad = Path::new(OsStr::from_bytes(b"/media/caf\xe9.mkv"));
    let err = build_plan(&PlanRequest {
        input: bad,
        output: Path::new("/b.mkv"),
        probe: &probe,
        profile: &prof,
        encoder: &enc,
    })
    .unwrap_err();
    assert!(
        err.to_string().contains("can't be passed to ffmpeg"),
        "{err}"
    );
}

#[test]
fn genpts_for_avi_and_mpeg_family() {
    let prof = profile(VideoCodec::H264, AudioCodec::Aac, Container::Mp4);
    for (container, expected) in [
        ("avi", true),
        ("mpegts", true),
        ("mpeg", true),
        ("matroska", false),
        ("mov", false),
    ] {
        let p = probe_of(container, vec![video(0, "mpeg2video", 720, 576)]);
        let plan = plan(&p, &prof, &software(VideoCodec::H264));
        assert_eq!(
            has_pair(&plan.args, "-fflags", "+genpts"),
            expected,
            "{container}"
        );
        if expected {
            assert_before_input(&plan.args, &["-fflags"]);
        }
    }
}

// ---------------------------------------------------------------------------
// Hardware APIs

#[test]
fn nvenc_with_gpu_decode_keeps_frames_on_the_gpu() {
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&movie(), &prof, &candidate("hevc_nvenc", true));
    let a = &plan.args;
    assert!(has_pair(a, "-hwaccel", "cuda"));
    assert!(has_pair(a, "-hwaccel_output_format", "cuda"));
    assert_before_input(a, &["-hwaccel", "-hwaccel_output_format"]);
    assert!(has_pair(a, "-c:v", "hevc_nvenc"));
    assert!(pair_pos(a, "-c:v", "hevc_nvenc").unwrap() > input_pos(a));
    // CUDA frames cannot take a -pix_fmt conversion.
    assert_absent(a, "-pix_fmt");
    assert_absent(a, "-vf");
    assert!(has_pair(a, "-rc", "vbr"));
    assert!(has_pair(a, "-cq", "26"));
    assert!(has_pair(a, "-preset", "p4"));
}

#[test]
fn nvenc_with_cpu_decode_takes_system_frames() {
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&movie(), &prof, &candidate("hevc_nvenc", false));
    assert_absent(&plan.args, "-hwaccel");
    assert!(has_pair(&plan.args, "-pix_fmt", "nv12"));
}

#[test]
fn nvenc_ten_bit() {
    let p = probe_of("matroska", vec![video_10bit(0, "hevc", 3840, 2160)]);
    let prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    // GPU decode: p010 CUDA frames pass straight through.
    let gpu = plan(&p, &prof, &candidate("av1_nvenc", true));
    assert_absent(&gpu.args, "-vf");
    assert_absent(&gpu.args, "-pix_fmt");
    // CPU decode: 10-bit system frames.
    let cpu = plan(&p, &prof, &candidate("av1_nvenc", false));
    assert!(has_pair(&cpu.args, "-pix_fmt", "p010le"));

    let hevc = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&p, &hevc, &candidate("hevc_nvenc", true));
    assert!(has_pair(&plan.args, "-profile:v", "main10"));

    // 10-bit source to 8-bit H.264 converts on the GPU.
    let h264 = profile(VideoCodec::H264, AudioCodec::Copy, Container::Mkv);
    let plan = common::plan(&p, &h264, &candidate("h264_nvenc", true));
    assert_eq!(vf(&plan.args).as_deref(), Some("scale_cuda=format=nv12"));
    assert_absent(&plan.args, "-profile:v");
    assert!(
        plan.notes.iter().any(|n| n.contains("8-bit")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn vaapi_with_gpu_decode() {
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let mut enc = candidate("hevc_vaapi", true);
    enc.device = Some("/dev/dri/renderD129".into());
    let plan = plan(&movie(), &prof, &enc);
    let a = &plan.args;
    assert!(has_pair(
        a,
        "-init_hw_device",
        "vaapi=va:/dev/dri/renderD129"
    ));
    assert!(has_pair(a, "-filter_hw_device", "va"));
    assert!(has_pair(a, "-hwaccel", "vaapi"));
    assert!(has_pair(a, "-hwaccel_output_format", "vaapi"));
    assert!(has_pair(a, "-hwaccel_device", "va"));
    assert_before_input(
        a,
        &[
            "-init_hw_device",
            "-filter_hw_device",
            "-hwaccel",
            "-hwaccel_output_format",
            "-hwaccel_device",
        ],
    );
    // The device must exist before -hwaccel_device names it.
    assert!(pos(a, "-init_hw_device").unwrap() < pos(a, "-hwaccel_device").unwrap());
    assert_absent(a, "-vf");
    assert_absent(a, "-pix_fmt");
    assert!(has_pair(a, "-rc_mode", "CQP"));
    assert!(has_pair(a, "-global_quality:v", "24"));
}

#[test]
fn vaapi_with_cpu_decode_uploads_frames() {
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&movie(), &prof, &candidate("hevc_vaapi", false));
    let a = &plan.args;
    assert!(has_pair(
        a,
        "-init_hw_device",
        "vaapi=va:/dev/dri/renderD128"
    ));
    assert!(has_pair(a, "-filter_hw_device", "va"));
    assert_absent(a, "-hwaccel");
    assert_absent(a, "-pix_fmt");
    assert_eq!(vf(a).as_deref(), Some("format=nv12,hwupload"));

    let p = probe_of("matroska", vec![video_10bit(0, "hevc", 1920, 1080)]);
    let plan = common::plan(&p, &prof, &candidate("hevc_vaapi", false));
    assert_eq!(vf(&plan.args).as_deref(), Some("format=p010le,hwupload"));
    assert!(has_pair(&plan.args, "-profile:v", "main10"));
}

#[test]
fn qsv_with_gpu_decode_names_the_qsv_decoder() {
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&movie(), &prof, &candidate("hevc_qsv", true));
    let a = &plan.args;
    assert_eq!(
        values(a, "-init_hw_device"),
        ["vaapi=va:/dev/dri/renderD128", "qsv=qs@va"]
    );
    assert!(has_pair(a, "-filter_hw_device", "qs"));
    assert!(has_pair(a, "-hwaccel", "qsv"));
    assert!(has_pair(a, "-hwaccel_output_format", "qsv"));
    // Decoder before -i, encoder after.
    let dec = pair_pos(a, "-c:v", "h264_qsv").expect("qsv decoder");
    let enc = pair_pos(a, "-c:v", "hevc_qsv").expect("qsv encoder");
    assert!(dec < input_pos(a) && enc > input_pos(a));
    assert_absent(a, "-vf");
    assert!(has_pair(a, "-global_quality:v", "24"));
    assert!(has_pair(a, "-preset", "veryfast"));
}

#[test]
fn qsv_with_cpu_decode_uploads_frames() {
    let prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&movie(), &prof, &candidate("av1_qsv", false));
    assert_absent(&plan.args, "-hwaccel");
    assert!(has_pair(&plan.args, "-filter_hw_device", "qs"));
    assert_eq!(
        vf(&plan.args).as_deref(),
        Some("format=nv12,hwupload=extra_hw_frames=64,format=qsv")
    );
}

#[test]
fn gpu_decode_falls_back_for_codecs_the_gpu_cannot_decode() {
    let p = probe_of("avi", vec![video(0, "mpeg4", 640, 480)]);
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    for name in ["hevc_qsv", "hevc_vaapi", "hevc_nvenc"] {
        let plan = plan(&p, &prof, &candidate(name, true));
        assert_absent(&plan.args, "-hwaccel");
        assert!(
            plan.notes.iter().any(|n| n.contains("can't decode MPEG-4")),
            "{name}: {:?}",
            plan.notes
        );
    }
    // 10-bit H.264 (High 10) has no GPU decoder either.
    let mut hi10 = video_10bit(0, "h264", 1920, 1080);
    hi10.bit_depth = Some(10);
    let p = probe_of("matroska", vec![hi10]);
    let plan = plan(&p, &prof, &candidate("hevc_nvenc", true));
    assert_absent(&plan.args, "-hwaccel");
    // 4:2:2 sources neither.
    let mut v422 = video(0, "hevc", 1920, 1080);
    v422.pix_fmt = Some("yuv422p10le".into());
    let p = probe_of("matroska", vec![v422]);
    let plan = common::plan(&p, &prof, &candidate("hevc_vaapi", true));
    assert_absent(&plan.args, "-hwaccel");
    assert!(
        plan.notes.iter().any(|n| n.contains("4:2:2")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn videotoolbox() {
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mp4);
    let on = plan(&movie(), &prof, &candidate("hevc_videotoolbox", true));
    assert!(has_pair(&on.args, "-hwaccel", "videotoolbox"));
    assert_before_input(&on.args, &["-hwaccel"]);
    assert_absent(&on.args, "-hwaccel_output_format");
    assert!(has_pair(&on.args, "-pix_fmt", "nv12"));
    assert!(has_pair(&on.args, "-q:v", "60"));
    assert!(has_pair(&on.args, "-allow_sw", "0"));

    let off = plan(&movie(), &prof, &candidate("hevc_videotoolbox", false));
    assert_absent(&off.args, "-hwaccel");

    let p = probe_of("matroska", vec![video_10bit(0, "hevc", 1920, 1080)]);
    let ten = common::plan(&p, &prof, &candidate("hevc_videotoolbox", true));
    assert!(has_pair(&ten.args, "-pix_fmt", "p010le"));
    assert!(has_pair(&ten.args, "-profile:v", "main10"));
}

#[test]
fn amf_rkmpp_and_v4l2_take_system_frames() {
    let p = probe_of("matroska", vec![video_10bit(0, "hevc", 1920, 1080)]);
    let hevc = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    for (name, fmt) in [
        ("hevc_amf", "p010le"),
        ("hevc_rkmpp", "nv12"),
        ("hevc_v4l2m2m", "nv12"),
    ] {
        let plan = plan(&p, &hevc, &candidate(name, true));
        assert_absent(&plan.args, "-hwaccel");
        assert_absent(&plan.args, "-init_hw_device");
        assert!(
            has_pair(&plan.args, "-pix_fmt", fmt),
            "{name}: {:?}",
            plan.args
        );
    }
    let rk = plan(&p, &hevc, &candidate("hevc_rkmpp", false));
    assert!(
        rk.notes
            .iter()
            .any(|n| n.contains("Rockchip MPP can only write 8-bit")),
        "{:?}",
        rk.notes
    );
    let amf = plan(&movie(), &hevc, &candidate("hevc_amf", false));
    assert!(has_pair(&amf.args, "-rc", "cqp"));
    assert!(has_pair(&amf.args, "-qp_i", "24"));
    let v4l2 = plan(
        &movie(),
        &profile(VideoCodec::H264, AudioCodec::Copy, Container::Mkv),
        &candidate("h264_v4l2m2m", false),
    );
    assert!(has_pair(&v4l2.args, "-b:v", "6500k"));
}

// ---------------------------------------------------------------------------
// Pixel formats and colour

#[test]
fn software_bit_depth_per_codec() {
    let p = probe_of("matroska", vec![video_10bit(0, "hevc", 1920, 1080)]);
    for codec in [VideoCodec::Av1, VideoCodec::Hevc, VideoCodec::Vp9] {
        let container = if codec == VideoCodec::Vp9 {
            Container::Webm
        } else {
            Container::Mkv
        };
        let prof = profile(codec, AudioCodec::Copy, container);
        let plan = plan(&p, &prof, &software(codec));
        assert!(has_pair(&plan.args, "-pix_fmt", "yuv420p10le"), "{codec:?}");
        assert!(plan.notes.is_empty(), "{codec:?}: {:?}", plan.notes);
    }
    let vp9 = plan(
        &p,
        &profile(VideoCodec::Vp9, AudioCodec::Copy, Container::Webm),
        &software(VideoCodec::Vp9),
    );
    assert!(has_pair(&vp9.args, "-profile:v", "2"));

    // H.264 stays 8-bit for compatibility.
    let h264 = plan(
        &p,
        &profile(VideoCodec::H264, AudioCodec::Copy, Container::Mp4),
        &software(VideoCodec::H264),
    );
    assert!(has_pair(&h264.args, "-pix_fmt", "yuv420p"));
    assert!(
        h264.notes.iter().any(|n| n.contains("8-bit")),
        "{:?}",
        h264.notes
    );

    // 8-bit sources are never promoted.
    let av1 = plan(
        &movie(),
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    assert!(has_pair(&av1.args, "-pix_fmt", "yuv420p"));
}

#[test]
fn hdr_colour_passes_through() {
    let p = probe_of("matroska", vec![hdr10(video_10bit(0, "hevc", 3840, 2160))]);
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&p, &prof, &software(VideoCodec::Hevc));
    let a = &plan.args;
    assert!(has_pair(a, "-color_primaries", "bt2020"));
    assert!(has_pair(a, "-color_trc", "smpte2084"));
    assert!(has_pair(a, "-colorspace", "bt2020nc"));
    assert!(has_pair(
        a,
        "-x265-params",
        "log-level=error:hdr-opt=1:repeat-headers=1"
    ));
    assert!(has_pair(a, "-pix_fmt", "yuv420p10le"));

    // HLG is HDR but not PQ: no hdr-opt.
    let mut hlg = video_10bit(0, "hevc", 3840, 2160);
    hlg.color_transfer = Some("arib-std-b67".into());
    hlg.hdr = Some(chrysopoeia_core::HdrFormat::Hlg);
    let p = probe_of("matroska", vec![hlg]);
    let plan = common::plan(&p, &prof, &software(VideoCodec::Hevc));
    assert!(has_pair(&plan.args, "-x265-params", "log-level=error"));
    assert!(has_pair(&plan.args, "-color_trc", "arib-std-b67"));
}

#[test]
fn unknown_and_rgb_colour_values_are_not_copied() {
    let mut v = video(0, "h264", 1920, 1080);
    v.color_primaries = Some("unknown".into());
    v.color_transfer = Some("reserved".into());
    v.color_space = Some("gbr".into());
    let p = probe_of("matroska", vec![v]);
    let plan = plan(
        &p,
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    assert_absent(&plan.args, "-color_primaries");
    assert_absent(&plan.args, "-color_trc");
    assert_absent(&plan.args, "-colorspace");

    let mut v = video(0, "h264", 1920, 1080);
    v.color_space = Some("bt709".into());
    let p = probe_of("matroska", vec![v]);
    let plan = common::plan(
        &p,
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    assert!(has_pair(&plan.args, "-colorspace", "bt709"));
}

#[test]
fn dolby_vision_and_hdr10plus_are_explained() {
    let mut dv = hdr10(video_10bit(0, "hevc", 3840, 2160));
    dv.hdr = Some(chrysopoeia_core::HdrFormat::DolbyVision);
    let p = probe_of("matroska", vec![dv]);
    let plan = plan(
        &p,
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    assert!(
        plan.notes.iter().any(|n| n.contains("Dolby Vision")),
        "{:?}",
        plan.notes
    );
}

// ---------------------------------------------------------------------------
// Filters

#[test]
fn interlaced_sources_are_deinterlaced_on_the_cpu() {
    let mut v = video(0, "mpeg2video", 720, 576);
    v.interlaced = true;
    let p = probe_of("mpegts", vec![v]);
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);

    let sw = plan(&p, &prof, &software(VideoCodec::Hevc));
    assert_eq!(vf(&sw.args).as_deref(), Some("bwdif=mode=send_frame"));

    for name in ["hevc_nvenc", "hevc_vaapi", "hevc_qsv", "hevc_videotoolbox"] {
        let plan = plan(&p, &prof, &candidate(name, true));
        assert_absent(&plan.args, "-hwaccel");
        assert!(
            vf(&plan.args).unwrap().starts_with("bwdif=mode=send_frame"),
            "{name}"
        );
        assert!(
            plan.notes.iter().any(|n| n.contains("interlaced")),
            "{name}: {:?}",
            plan.notes
        );
    }
    let va = plan(&p, &prof, &candidate("hevc_vaapi", true));
    assert_eq!(
        vf(&va.args).as_deref(),
        Some("bwdif=mode=send_frame,format=nv12,hwupload")
    );
}

#[test]
fn downscale_per_frame_location() {
    let p = probe_of("matroska", vec![video(0, "h264", 3840, 2160)]);
    let mut prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    prof.max_height = Some(1080);

    let cases = [
        (software(VideoCodec::Hevc), "scale=-2:1080:flags=lanczos"),
        (
            candidate("hevc_nvenc", true),
            "scale_cuda=w=-2:h=1080:format=nv12",
        ),
        (
            candidate("hevc_nvenc", false),
            "scale=-2:1080:flags=lanczos",
        ),
        (
            candidate("hevc_vaapi", true),
            "scale_vaapi=w=-2:h=1080:format=nv12",
        ),
        (
            candidate("hevc_vaapi", false),
            "scale=-2:1080:flags=lanczos,format=nv12,hwupload",
        ),
        (
            candidate("hevc_qsv", true),
            "scale_qsv=w=-1:h=1080:format=nv12",
        ),
        (
            candidate("hevc_qsv", false),
            "scale=-2:1080:flags=lanczos,format=nv12,hwupload=extra_hw_frames=64,format=qsv",
        ),
    ];
    for (enc, expected) in cases {
        let plan = plan(&p, &prof, &enc);
        assert_eq!(
            vf(&plan.args).as_deref(),
            Some(expected),
            "{} hw_decode={}",
            enc.name,
            enc.hw_decode
        );
    }

    // 10-bit sources keep 10-bit through the GPU scaler.
    let p10 = probe_of("matroska", vec![video_10bit(0, "hevc", 3840, 2160)]);
    let plan = plan(&p10, &prof, &candidate("hevc_nvenc", true));
    assert_eq!(
        vf(&plan.args).as_deref(),
        Some("scale_cuda=w=-2:h=1080:format=p010le")
    );

    // Odd limits round down to even; shorter sources are left alone.
    prof.max_height = Some(719);
    let plan = common::plan(&p, &prof, &software(VideoCodec::Hevc));
    assert_eq!(
        vf(&plan.args).as_deref(),
        Some("scale=-2:718:flags=lanczos")
    );
    prof.max_height = Some(2160);
    let plan = common::plan(&p, &prof, &software(VideoCodec::Hevc));
    assert_absent(&plan.args, "-vf");
}

#[test]
fn odd_dimensions_are_made_even() {
    let p = probe_of("matroska", vec![video(0, "vp9", 639, 359)]);
    let prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    let sw = plan(&p, &prof, &software(VideoCodec::Av1));
    assert_eq!(
        vf(&sw.args).as_deref(),
        Some("scale=trunc(iw/2)*2:trunc(ih/2)*2:flags=lanczos")
    );
    assert!(
        sw.notes.iter().any(|n| n.contains("639×359 to 638×358")),
        "{:?}",
        sw.notes
    );

    // GPU decode would hand odd-sized surfaces to the encoder: decode on the CPU.
    let nv = plan(&p, &prof, &candidate("av1_nvenc", true));
    assert_absent(&nv.args, "-hwaccel");
    assert!(vf(&nv.args).unwrap().starts_with("scale=trunc(iw/2)*2"));

    // A downscale fixes odd sizes on its own.
    let mut small = prof.clone();
    small.max_height = Some(240);
    let plan = plan(&p, &small, &software(VideoCodec::Av1));
    assert_eq!(
        vf(&plan.args).as_deref(),
        Some("scale=-2:240:flags=lanczos")
    );
}

// ---------------------------------------------------------------------------
// Audio

#[test]
fn audio_language_filter_keeps_untagged_and_restores_default() {
    let mut eng = audio(1, "aac", 2, 48_000, Some("eng"));
    eng.is_default = true;
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            eng,
            audio(2, "aac", 2, 48_000, Some("ja")),
            audio(3, "aac", 2, 48_000, Some("fre")),
            audio(4, "aac", 2, 48_000, None),
            audio(5, "aac", 2, 48_000, Some("und")),
        ],
    );
    let mut prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    prof.audio_languages = vec!["jpn".into()];
    let plan = plan(&p, &prof, &software(VideoCodec::Av1));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:2", "0:4", "0:5"]);
    assert_eq!(plan.expected.audio, 3);
    assert!(has_pair(&plan.args, "-disposition:a:0", "default"));
    assert!(plan.notes.is_empty(), "{:?}", plan.notes);
}

#[test]
fn language_filter_never_drops_all_audio() {
    let mut eng = audio(2, "ac3", 6, 48_000, Some("eng"));
    eng.is_default = true;
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "aac", 2, 48_000, Some("jpn")),
            eng,
        ],
    );
    let mut prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    prof.audio_languages = vec!["ger".into()];
    let plan = plan(&p, &prof, &software(VideoCodec::Av1));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:2"]);
    assert_absent(&plan.args, "-disposition:a:0");
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("(ger)") && n.contains("default track was kept")),
        "{:?}",
        plan.notes
    );

    // Without a default track, the first one is kept.
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "aac", 2, 48_000, Some("jpn")),
            audio(2, "aac", 2, 48_000, Some("eng")),
        ],
    );
    let plan = common::plan(&p, &prof, &software(VideoCodec::Av1));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:1"]);
    assert!(
        plan.notes.iter().any(|n| n.contains("first track")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn opus_from_side_surround_gets_a_normalised_layout() {
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "ac3", 6, 44_100, Some("eng")),
        ],
    );
    let prof = profile(VideoCodec::Av1, AudioCodec::Opus, Container::Mkv);
    let plan = plan(&p, &prof, &software(VideoCodec::Av1));
    let a = &plan.args;
    assert!(has_pair(a, "-c:a:0", "libopus"));
    assert!(has_pair(a, "-b:a:0", "320k"));
    assert!(has_pair(a, "-ar:a:0", "48000"));
    assert!(has_pair(a, "-mapping_family:a:0", "1"));
    let filter = value(a, "-filter:a:0").unwrap();
    assert!(filter.starts_with("aformat=channel_layouts="), "{filter}");
    assert!(
        filter.contains("5.1|") && !filter.contains("5.1(side)"),
        "{filter}"
    );
    assert_absent(a, "-ac:a:0");
}

#[test]
fn stereo_opus_needs_no_mapping_family() {
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "aac", 2, 48_000, None),
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::Av1, AudioCodec::Opus, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    assert!(has_pair(&plan.args, "-b:a:0", "128k"));
    assert_absent(&plan.args, "-mapping_family:a:0");
    assert_absent(&plan.args, "-filter:a:0");
    assert_absent(&plan.args, "-ar:a:0");
}

#[test]
fn per_stream_specifiers_target_output_indices() {
    // Source index 3 is the second output audio track.
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "opus", 2, 48_000, Some("eng")),
            subtitle(2, "subrip", Some("eng")),
            audio(3, "dts", 6, 48_000, Some("eng")),
        ],
    );
    let prof = profile(VideoCodec::Av1, AudioCodec::Opus, Container::Mkv);
    let plan = plan(&p, &prof, &software(VideoCodec::Av1));
    let a = &plan.args;
    assert_eq!(values(a, "-map"), ["0:0", "0:1", "0:3", "0:2"]);
    assert!(has_pair(a, "-c:a:0", "copy"), "already Opus: copied");
    assert!(has_pair(a, "-c:a:1", "libopus"));
    assert!(has_pair(a, "-b:a:1", "320k"));
    assert!(has_pair(a, "-mapping_family:a:1", "1"));
    assert_absent(a, "-b:a:0");
    assert!(has_pair(a, "-c:s:0", "copy"));
}

#[test]
fn sample_rates_are_fixed_per_codec() {
    let src = |rate| {
        probe_of(
            "matroska",
            vec![video(0, "h264", 1280, 720), audio(1, "mp2", 2, rate, None)],
        )
    };
    let ac3 = profile(VideoCodec::Av1, AudioCodec::Ac3, Container::Mkv);
    let plan44 = plan(&src(44_100), &ac3, &software(VideoCodec::Av1));
    assert_absent(&plan44.args, "-ar:a:0");
    assert!(has_pair(&plan44.args, "-b:a:0", "192k"));
    let plan96 = plan(&src(96_000), &ac3, &software(VideoCodec::Av1));
    assert!(has_pair(&plan96.args, "-ar:a:0", "48000"));

    let opus = profile(VideoCodec::Av1, AudioCodec::Opus, Container::Mkv);
    let plan = plan(&src(44_100), &opus, &software(VideoCodec::Av1));
    assert!(has_pair(&plan.args, "-ar:a:0", "48000"));
    let plan = common::plan(&src(24_000), &opus, &software(VideoCodec::Av1));
    assert_absent(&plan.args, "-ar:a:0");
}

#[test]
fn mp3_downmixes_to_stereo() {
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "dts", 6, 48_000, None),
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::H264, AudioCodec::Mp3, Container::Mkv),
        &software(VideoCodec::H264),
    );
    assert!(has_pair(&plan.args, "-c:a:0", "libmp3lame"));
    assert!(has_pair(&plan.args, "-ac:a:0", "2"));
    assert!(has_pair(&plan.args, "-b:a:0", "192k"));
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "Downmixed 5.1 audio to stereo because MP3 holds at most 2 channels"),
        "{:?}",
        plan.notes
    );
}

#[test]
fn dolby_encoders_stop_at_six_channels() {
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "truehd", 8, 48_000, None),
        ],
    );
    for (codec, encoder, kbps) in [
        (AudioCodec::Ac3, "ac3", "640k"),
        (AudioCodec::Eac3, "eac3", "640k"),
    ] {
        let plan = plan(
            &p,
            &profile(VideoCodec::Hevc, codec, Container::Mkv),
            &software(VideoCodec::Hevc),
        );
        assert!(has_pair(&plan.args, "-c:a:0", encoder));
        assert!(has_pair(&plan.args, "-ac:a:0", "6"), "{encoder}");
        assert!(has_pair(&plan.args, "-b:a:0", kbps), "{encoder}");
    }
}

#[test]
fn aac_surround_gets_standard_layouts() {
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "ac3", 6, 48_000, None),
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::H264, AudioCodec::Aac, Container::Mp4),
        &software(VideoCodec::H264),
    );
    assert!(has_pair(&plan.args, "-c:a:0", "aac"));
    assert!(has_pair(&plan.args, "-b:a:0", "384k"));
    assert!(
        value(&plan.args, "-filter:a:0")
            .unwrap()
            .starts_with("aformat=channel_layouts=")
    );
}

#[test]
fn keep_original_audio_respects_the_container() {
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "truehd", 8, 48_000, Some("eng")),
            audio(2, "dts", 6, 48_000, Some("eng")),
            audio(3, "ac3", 6, 48_000, Some("eng")),
        ],
    );
    // MKV takes everything.
    let mkv = plan(
        &p,
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Hevc),
    );
    for n in 0..3 {
        assert!(has_pair(&mkv.args, &format!("-c:a:{n}"), "copy"));
    }
    // MP4 converts what it can't hold.
    let mp4 = plan(
        &p,
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mp4),
        &software(VideoCodec::Hevc),
    );
    assert!(has_pair(&mp4.args, "-c:a:0", "aac"));
    assert!(has_pair(&mp4.args, "-b:a:0", "512k"));
    assert!(has_pair(&mp4.args, "-c:a:1", "aac"));
    assert!(has_pair(&mp4.args, "-c:a:2", "copy"));
    assert!(
        mp4.notes
            .iter()
            .any(|n| n
                == "Converted 2 audio tracks (TrueHD, DTS) to AAC because MP4 can't hold them"),
        "{:?}",
        mp4.notes
    );
    // WebM only takes Opus/Vorbis.
    let webm = plan(
        &p,
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Webm),
        &software(VideoCodec::Av1),
    );
    assert!(has_pair(&webm.args, "-c:a:2", "libopus"));
}

#[test]
fn blu_ray_pcm_becomes_flac_in_mkv() {
    let p = probe_of(
        "mpegts",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "pcm_bluray", 6, 48_000, None),
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Hevc),
    );
    assert!(has_pair(&plan.args, "-c:a:0", "flac"));
    assert!(has_pair(&plan.args, "-sample_fmt:a:0", "s32"));
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("PCM audio track to FLAC")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn flac_bit_depth_follows_the_source() {
    let lossy = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "ac3", 6, 48_000, None),
        ],
    );
    let prof = profile(VideoCodec::Av1, AudioCodec::Flac, Container::Mkv);
    let plan = plan(&lossy, &prof, &software(VideoCodec::Av1));
    assert!(has_pair(&plan.args, "-c:a:0", "flac"));
    assert!(has_pair(&plan.args, "-sample_fmt:a:0", "s16"));
    assert_absent(&plan.args, "-b:a:0");
    let hires = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "pcm_s24le", 2, 96_000, None),
        ],
    );
    let plan = common::plan(&hires, &prof, &software(VideoCodec::Av1));
    assert!(has_pair(&plan.args, "-sample_fmt:a:0", "s32"));
}

#[test]
fn unsupported_target_audio_falls_back_with_a_note() {
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "ac3", 2, 48_000, None),
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::H264, AudioCodec::Flac, Container::Mp4),
        &software(VideoCodec::H264),
    );
    assert!(has_pair(&plan.args, "-c:a:0", "aac"));
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "MP4 can't hold FLAC audio, so AAC was used instead"),
        "{:?}",
        plan.notes
    );
}

#[test]
fn source_already_in_target_codec_is_copied() {
    let p = probe_of(
        "mov",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "aac", 6, 48_000, None),
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::Hevc, AudioCodec::Aac, Container::Mp4),
        &software(VideoCodec::Hevc),
    );
    assert!(has_pair(&plan.args, "-c:a:0", "copy"));
}

#[test]
fn broken_audio_tracks_are_dropped() {
    let mut empty = audio(2, "mp2", 0, 0, None);
    empty.channels = Some(0);
    let p = probe_of(
        "mpegts",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "ac3", 2, 48_000, None),
            empty,
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Hevc),
    );
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:1"]);
    assert!(
        plan.notes.iter().any(|n| n.contains("no readable sound")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn statistics_tags_are_cleared_for_reencoded_streams_in_matroska() {
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "ac3", 2, 48_000, None),
        ],
    );
    let mkv = plan(
        &p,
        &profile(VideoCodec::Av1, AudioCodec::Opus, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    assert!(has_pair(&mkv.args, "-metadata:s:v:0", "BPS="));
    assert!(has_pair(&mkv.args, "-metadata:s:a:0", "NUMBER_OF_FRAMES="));
    let mp4 = plan(
        &p,
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mp4),
        &software(VideoCodec::Av1),
    );
    assert_absent(&mp4.args, "-metadata:s:v:0");
}

// ---------------------------------------------------------------------------
// Subtitles and attachments

fn subtitled() -> chrysopoeia_core::ProbeInfo {
    let mut forced = subtitle(5, "subrip", Some("fre"));
    forced.is_forced = true;
    probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "aac", 2, 48_000, Some("eng")),
            subtitle(2, "subrip", Some("eng")),
            subtitle(3, "hdmv_pgs_subtitle", Some("eng")),
            subtitle(4, "ass", Some("eng")),
            forced,
            subtitle(6, "hdmv_pgs_subtitle", Some("ger")),
            subtitle(7, "dvb_teletext", Some("eng")),
            attachment(8),
            attachment(9),
        ],
    )
}

#[test]
fn mkv_keeps_text_and_picture_subtitles_and_attachments() {
    let plan = plan(
        &subtitled(),
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    let a = &plan.args;
    assert_eq!(
        values(a, "-map"),
        [
            "0:0", "0:1", "0:2", "0:3", "0:4", "0:5", "0:6", "0:8", "0:9"
        ]
    );
    for n in 0..5 {
        assert!(has_pair(a, &format!("-c:s:{n}"), "copy"));
    }
    assert!(has_pair(a, "-c:t", "copy"));
    assert_eq!(plan.expected.subtitle, 5);
    assert!(
        plan.notes.iter().any(|n| n.contains("dvb_teletext")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn mp4_converts_text_and_drops_picture_subtitles() {
    let plan = plan(
        &subtitled(),
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mp4),
        &software(VideoCodec::Hevc),
    );
    let a = &plan.args;
    assert_eq!(values(a, "-map"), ["0:0", "0:1", "0:2", "0:4", "0:5"]);
    for n in 0..3 {
        assert!(has_pair(a, &format!("-c:s:{n}"), "mov_text"));
    }
    assert_absent(a, "-c:t");
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "Removed 2 picture-based subtitles because MP4 can't hold them"),
        "{:?}",
        plan.notes
    );
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("1 styled subtitle to plain text")),
        "{:?}",
        plan.notes
    );
    assert!(has_pair(a, "-movflags", "+faststart"));
    assert!(has_pair(a, "-tag:v", "hvc1"));
    assert_eq!(value(a, "-f").as_deref(), Some("mp4"));
}

#[test]
fn webm_converts_text_to_webvtt() {
    let plan = plan(
        &subtitled(),
        &profile(VideoCodec::Vp9, AudioCodec::Opus, Container::Webm),
        &software(VideoCodec::Vp9),
    );
    assert!(has_pair(&plan.args, "-c:s:0", "webvtt"));
    assert!(
        plan.notes
            .iter()
            .any(|n| n.contains("WebM can't hold them")),
        "{:?}",
        plan.notes
    );
    assert_eq!(value(&plan.args, "-f").as_deref(), Some("webm"));
    assert_absent(&plan.args, "-movflags");
}

#[test]
fn mov_text_becomes_srt_in_mkv() {
    let p = probe_of(
        "mov",
        vec![
            video(0, "h264", 1280, 720),
            subtitle(1, "mov_text", Some("eng")),
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Av1),
    );
    assert!(has_pair(&plan.args, "-c:s:0", "srt"));
}

#[test]
fn subtitle_language_filter_keeps_forced_tracks() {
    let mut prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    prof.subtitle_languages = vec!["en".into()];
    let plan = plan(&subtitled(), &prof, &software(VideoCodec::Av1));
    // 6 (German PGS) goes; 5 (forced French) stays.
    assert_eq!(
        values(&plan.args, "-map"),
        ["0:0", "0:1", "0:2", "0:3", "0:4", "0:5", "0:8", "0:9"]
    );
}

#[test]
fn subtitle_drop_policy_removes_all() {
    let mut prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    prof.subtitles = SubtitlePolicy::Drop;
    let plan = plan(&subtitled(), &prof, &software(VideoCodec::Av1));
    assert_absent(&plan.args, "-c:s:0");
    assert_eq!(plan.expected.subtitle, 0);
    assert!(plan.notes.is_empty(), "{:?}", plan.notes);
}

#[test]
fn mp4_tags_only_hevc() {
    let plan = plan(
        &movie(),
        &profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mp4),
        &software(VideoCodec::Av1),
    );
    assert!(has_pair(&plan.args, "-movflags", "+faststart"));
    assert_absent(&plan.args, "-tag:v");
    let mkv = common::plan(
        &movie(),
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Hevc),
    );
    assert_absent(&mkv.args, "-tag:v");
    assert_absent(&mkv.args, "-movflags");
}
