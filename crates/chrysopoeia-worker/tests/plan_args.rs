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
    // The cover is attached to the MKV instead (see the cover tests).
    assert_eq!(values(&plan.args, "-attach"), ["/tmp/out.cover-1.jpg"]);
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
    assert!(
        err.to_string()
            .starts_with("The encoder Chrysopoeia picked makes HEVC (H.265) video"),
        "{err}"
    );

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

/// HDR10 static metadata (mastering display, light levels) is written by
/// the CPU encoders, which don't copy it from the decoded frames.
#[test]
fn hdr10_mastering_metadata_is_passed_to_cpu_encoders() {
    let mut v = hdr10(video_10bit(0, "hevc", 3840, 2160));
    v.mastering_display = Some(chrysopoeia_core::MasteringDisplay {
        red: [0.68, 0.32],
        green: [0.265, 0.69],
        blue: [0.15, 0.06],
        white_point: [0.3127, 0.329],
        max_luminance: 1000.0,
        min_luminance: 0.0001,
    });
    v.content_light = Some(chrysopoeia_core::ContentLight {
        max_cll: 1000,
        max_fall: 400,
    });
    let p = probe_of("matroska", vec![v]);
    let hevc = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = common::plan(&p, &hevc, &software(VideoCodec::Hevc));
    assert_eq!(
        value(&plan.args, "-x265-params").as_deref(),
        Some(
            "log-level=error:hdr-opt=1:repeat-headers=1:\
             master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)\
             L(10000000,1):max-cll=1000,400"
        )
    );
    let av1 = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    let plan = common::plan(&p, &av1, &software(VideoCodec::Av1));
    assert_eq!(
        value(&plan.args, "-svtav1-params").as_deref(),
        Some(
            "mastering-display=G(0.2650,0.6900)B(0.1500,0.0600)R(0.6800,0.3200)\
             WP(0.3127,0.3290)L(1000.0000,0.0001):content-light=1000,400"
        )
    );
    // H.264 (8-bit, SDR) never gets HDR metadata; plain HDR10 without
    // metadata gets no SVT-AV1 options.
    let p = probe_of("matroska", vec![hdr10(video_10bit(0, "hevc", 3840, 2160))]);
    let plan = common::plan(&p, &av1, &software(VideoCodec::Av1));
    assert_absent(&plan.args, "-svtav1-params");
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

    // CPU frames: an orientation-free fit into 1920x1080 (or 1080x1920).
    let fit = fit_filter(1920, 1080);
    assert_eq!(
        fit,
        "scale=w='if(gte(iw,ih),if(gte(iw*1080,ih*1920),1920,-2),if(gte(ih*1080,iw*1920),-2,1080))'\
         :h='if(gte(iw,ih),if(gte(iw*1080,ih*1920),-2,1080),if(gte(ih*1080,iw*1920),1920,-2))'\
         :flags=lanczos"
    );
    let cases = [
        (software(VideoCodec::Hevc), fit.clone()),
        (
            candidate("hevc_nvenc", true),
            "scale_cuda=w=1920:h=1080:format=nv12".into(),
        ),
        (candidate("hevc_nvenc", false), fit.clone()),
        (
            candidate("hevc_vaapi", true),
            "scale_vaapi=w=1920:h=1080:format=nv12".into(),
        ),
        (
            candidate("hevc_vaapi", false),
            format!("{fit},format=nv12,hwupload"),
        ),
        (
            candidate("hevc_qsv", true),
            "scale_qsv=w=1920:h=1080:format=nv12".into(),
        ),
        (
            candidate("hevc_qsv", false),
            format!("{fit},format=nv12,hwupload=extra_hw_frames=64,format=qsv"),
        ),
    ];
    for (enc, expected) in cases {
        let plan = plan(&p, &prof, &enc);
        assert_eq!(
            vf(&plan.args),
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
        Some("scale_cuda=w=1920:h=1080:format=p010le")
    );

    // Odd limits round down to even; shorter sources are left alone.
    prof.max_height = Some(719);
    let plan = common::plan(&p, &prof, &software(VideoCodec::Hevc));
    assert_eq!(vf(&plan.args), Some(fit_filter(1278, 718)));
    let plan = common::plan(&p, &prof, &candidate("hevc_vaapi", true));
    assert_eq!(
        vf(&plan.args).as_deref(),
        Some("scale_vaapi=w=1276:h=718:format=nv12")
    );
    prof.max_height = Some(2160);
    let plan = common::plan(&p, &prof, &software(VideoCodec::Hevc));
    assert_absent(&plan.args, "-vf");
}

#[test]
fn size_limit_is_a_resolution_class() {
    let mut prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    prof.max_height = Some(1080);
    let plan_for = |w, h, enc: &chrysopoeia_core::EncoderCandidate| {
        common::plan(
            &probe_of("matroska", vec![video(0, "h264", w, h)]),
            &prof,
            enc,
        )
    };
    // Scope 4K is limited by its width: 1920x800, not 2592x1080.
    let plan = plan_for(3840, 1600, &candidate("hevc_vaapi", true));
    assert_eq!(
        vf(&plan.args).as_deref(),
        Some("scale_vaapi=w=1920:h=800:format=nv12")
    );
    // Portrait video stored upright is limited on its short side.
    let plan = plan_for(2160, 3840, &candidate("hevc_nvenc", true));
    assert_eq!(
        vf(&plan.args).as_deref(),
        Some("scale_cuda=w=1080:h=1920:format=nv12")
    );
    // Portrait 1080p and 4:3 1080p are within the limit.
    assert_absent(
        &plan_for(1080, 1920, &software(VideoCodec::Hevc)).args,
        "-vf",
    );
    assert_absent(
        &plan_for(1440, 1080, &software(VideoCodec::Hevc)).args,
        "-vf",
    );
}

#[test]
fn qsv_scaling_uses_explicit_even_widths() {
    // scale_qsv's own -1 would compute 1719 for 1920x804 at 720 lines.
    let p = probe_of("matroska", vec![video(0, "h264", 1920, 804)]);
    let mut prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    prof.max_height = Some(720);
    let plan = plan(&p, &prof, &candidate("hevc_qsv", true));
    assert_eq!(
        vf(&plan.args).as_deref(),
        Some("scale_qsv=w=1280:h=536:format=nv12")
    );
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
    assert_eq!(vf(&plan.args), Some(fit_filter(428, 240)));
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
    // `+default` adds the flag without wiping the track's other flags.
    assert!(has_pair(&plan.args, "-disposition:a:0", "+default"));
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
    // Exactly one layout: a list lets ffmpeg pad the track with silent
    // channels.
    assert_eq!(
        value(a, "-filter:a:0").as_deref(),
        Some("aformat=channel_layouts=5.1")
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
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "Left out 2 subtitle fonts because MP4 can't hold attachments"),
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
    // A copied camera timecode must not become an extra data track.
    assert!(has_pair(&plan.args, "-write_tmcd", "0"));
    assert!(pos(&plan.args, "-write_tmcd").unwrap() < pos(&plan.args, "-f").unwrap());
    assert_absent(&plan.args, "-tag:v");
    let mkv = common::plan(
        &movie(),
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Hevc),
    );
    assert_absent(&mkv.args, "-tag:v");
    assert_absent(&mkv.args, "-movflags");
    assert_absent(&mkv.args, "-write_tmcd");
}

// ---------------------------------------------------------------------------
// Regression tests for review findings

#[test]
fn audio_the_quick_probe_could_not_read_is_kept_when_it_is_all_there_is() {
    // A plain ffprobe reports a track that starts late in a TS as 0 channels
    // at 0 Hz. Dropping it would write a silent file that verification
    // accepts (expected.audio would be 0).
    let late = audio(1, "ac3", 0, 0, None);
    let p = probe_of("mpegts", vec![video(0, "mpeg2video", 720, 576), late]);
    let prof = profile(VideoCodec::Hevc, AudioCodec::Opus, Container::Mkv);
    let plan = plan(&p, &prof, &software(VideoCodec::Hevc));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:1"]);
    assert_eq!(plan.expected.audio, 1);
    assert!(has_pair(&plan.args, "-c:a:0", "libopus"));
    // Unknown channel count: the encoder picks the bitrate and layout.
    assert_absent(&plan.args, "-b:a:0");
    assert!(has_pair(&plan.args, "-ar:a:0", "48000"));
    assert!(
        plan.notes.iter().all(|n| !n.contains("no readable sound")),
        "{:?}",
        plan.notes
    );

    // Copy keeps it as it is.
    let copy = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = common::plan(&p, &copy, &software(VideoCodec::Hevc));
    assert!(has_pair(&plan.args, "-c:a:0", "copy"));
    assert_eq!(plan.expected.audio, 1);

    // The language filter still applies among such tracks, and still never
    // drops them all.
    let p = probe_of(
        "mpegts",
        vec![
            video(0, "mpeg2video", 720, 576),
            audio(1, "ac3", 0, 0, Some("deu")),
            audio(2, "mp2", 0, 0, Some("eng")),
        ],
    );
    let mut eng = copy.clone();
    eng.audio_languages = vec!["eng".into()];
    let plan = common::plan(&p, &eng, &software(VideoCodec::Hevc));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:2"]);
    eng.audio_languages = vec!["fra".into()];
    let plan = common::plan(&p, &eng, &software(VideoCodec::Hevc));
    assert_eq!(plan.expected.audio, 1);
}

#[test]
fn unconfirmed_tracks_are_dropped_next_to_readable_ones() {
    // An empty PID beside a real track (common in DVB recordings) is removed,
    // even when it is the one in the chosen language.
    let p = probe_of(
        "mpegts",
        vec![
            video(0, "h264", 1280, 720),
            audio(1, "ac3", 2, 48_000, Some("deu")),
            audio(2, "ac3", 0, 0, Some("eng")),
        ],
    );
    let mut prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    prof.audio_languages = vec!["eng".into()];
    let plan = plan(&p, &prof, &software(VideoCodec::Hevc));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:1"]);
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "Removed 1 audio track that contains no readable sound"),
        "{:?}",
        plan.notes
    );
    assert!(
        plan.notes.iter().any(|n| n.contains("(eng)")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn audio_nobody_can_read_fails_the_plan_instead_of_going_silent() {
    let mut unknown = audio(1, "unknown", 2, 48_000, None);
    unknown.channels = None;
    let p = probe_of("avi", vec![video(0, "mpeg4", 640, 480), unknown]);
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let err = build_plan(&PlanRequest {
        input: Path::new("/media/in.avi"),
        output: Path::new("/tmp/out.mkv"),
        probe: &p,
        profile: &prof,
        encoder: &software(VideoCodec::Hevc),
    })
    .expect_err("silent output must be refused");
    assert!(err.to_string().contains("silent"), "{err}");
}

#[test]
fn a_new_default_track_keeps_its_other_flags_and_skips_commentary() {
    let mut commentary = audio(1, "aac", 2, 48_000, Some("eng"));
    commentary.title = Some("Director's Commentary".into());
    let mut main = audio(2, "aac", 2, 48_000, Some("eng"));
    main.title = Some("English".into());
    let mut ger = audio(3, "aac", 2, 48_000, Some("ger"));
    ger.is_default = true;
    let p = probe_of(
        "matroska",
        vec![video(0, "h264", 1920, 1080), commentary, main, ger],
    );
    let mut prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    prof.audio_languages = vec!["eng".into()];
    let plan = plan(&p, &prof, &software(VideoCodec::Av1));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:1", "0:2"]);
    assert!(
        has_pair(&plan.args, "-disposition:a:1", "+default"),
        "{:?}",
        plan.args
    );
    assert_absent(&plan.args, "-disposition:a:0");

    // The never-drop-all fallback also passes over commentary.
    let mut first = audio(1, "aac", 2, 48_000, Some("eng"));
    first.title = Some("Audio Description".into());
    let p = probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            first,
            audio(2, "aac", 2, 48_000, Some("eng")),
        ],
    );
    prof.audio_languages = vec!["jpn".into()];
    let plan = common::plan(&p, &prof, &software(VideoCodec::Av1));
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:2"]);
    assert!(
        plan.notes.iter().any(|n| n.contains("first main track")),
        "{:?}",
        plan.notes
    );
}

#[test]
fn surround_layouts_keep_their_channel_count() {
    let with_layout = |channels, layout: &str| {
        let mut a = audio(1, "ac3", channels, 48_000, None);
        a.channel_layout = Some(layout.into());
        probe_of("matroska", vec![video(0, "mpeg2video", 720, 576), a])
    };
    // DVD 3/2 AC-3 without LFE.
    let dvd = with_layout(5, "5.0(side)");
    for (codec, container, encoder, kbps) in [
        (AudioCodec::Aac, Container::Mp4, "aac", "384k"),
        (AudioCodec::Opus, Container::Mkv, "libopus", "320k"),
        (AudioCodec::Vorbis, Container::Webm, "libvorbis", "320k"),
    ] {
        let video_codec = if container == Container::Webm {
            VideoCodec::Vp9
        } else {
            VideoCodec::Hevc
        };
        let plan = plan(
            &dvd,
            &profile(video_codec, codec, container),
            &software(video_codec),
        );
        assert!(has_pair(&plan.args, "-c:a:0", encoder));
        assert_eq!(
            value(&plan.args, "-filter:a:0").as_deref(),
            Some("aformat=channel_layouts=5.0"),
            "{encoder}"
        );
        assert!(has_pair(&plan.args, "-b:a:0", kbps), "{encoder}");
    }
    // Quad and 4.0 map to the layout each format has for four channels.
    let aac = profile(VideoCodec::Hevc, AudioCodec::Aac, Container::Mp4);
    let opus = profile(VideoCodec::Hevc, AudioCodec::Opus, Container::Mkv);
    let enc = software(VideoCodec::Hevc);
    let filter = |p: &chrysopoeia_core::ProbeInfo, prof| {
        value(&common::plan(p, prof, &enc).args, "-filter:a:0")
    };
    assert_eq!(
        filter(&with_layout(4, "quad"), &aac).as_deref(),
        Some("aformat=channel_layouts=4.0")
    );
    assert_eq!(
        filter(&with_layout(4, "4.0"), &opus).as_deref(),
        Some("aformat=channel_layouts=quad")
    );
    assert_eq!(
        filter(&with_layout(8, "7.1(wide)"), &opus).as_deref(),
        Some("aformat=channel_layouts=7.1")
    );
    // 6.1 has no standard AAC layout: 5.1, with the bitrate for 5.1.
    let plan = common::plan(&with_layout(7, "6.1"), &aac, &enc);
    assert_eq!(
        value(&plan.args, "-filter:a:0").as_deref(),
        Some("aformat=channel_layouts=5.1")
    );
    assert!(has_pair(&plan.args, "-b:a:0", "384k"));
    // Vorbis gets a layout too (libvorbis relabels others silently).
    let vorbis = profile(VideoCodec::Hevc, AudioCodec::Vorbis, Container::Mkv);
    assert_eq!(
        filter(&with_layout(3, "2.1"), &vorbis).as_deref(),
        Some("aformat=channel_layouts=5.1")
    );
}

#[test]
fn subtitles_matroska_cannot_store_are_converted_or_dropped() {
    let p = probe_of(
        "avi",
        vec![
            video(0, "mpeg4", 720, 480),
            audio(1, "mp3", 2, 48_000, None),
            subtitle(2, "xsub", Some("eng")),
            subtitle(3, "microdvd", Some("eng")),
            subtitle(4, "subrip", Some("eng")),
        ],
    );
    let plan = plan(
        &p,
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv),
        &software(VideoCodec::Hevc),
    );
    assert_eq!(values(&plan.args, "-map"), ["0:0", "0:1", "0:3", "0:4"]);
    assert!(has_pair(&plan.args, "-c:s:0", "srt"));
    assert!(has_pair(&plan.args, "-c:s:1", "copy"));
    assert_eq!(plan.expected.subtitle, 2);
    assert!(
        plan.notes
            .iter()
            .any(|n| n == "Removed 1 picture-based subtitle because MKV can't hold it"),
        "{:?}",
        plan.notes
    );
}

#[test]
fn dolby_vision_notes_name_the_picture_that_is_kept() {
    let prof = profile(VideoCodec::Av1, AudioCodec::Copy, Container::Mkv);
    let enc = software(VideoCodec::Av1);
    let note_for = |v: StreamInfo| {
        let plan = common::plan(&probe_of("matroska", vec![v]), &prof, &enc);
        plan.notes
            .into_iter()
            .find(|n| n.contains("Dolby Vision"))
            .expect("a note")
    };
    let mut hdr10_base = hdr10(video_10bit(0, "hevc", 3840, 2160));
    hdr10_base.hdr = Some(chrysopoeia_core::HdrFormat::DolbyVision);
    assert!(note_for(hdr10_base.clone()).ends_with("standard HDR10 picture"));
    let mut hlg = video_10bit(0, "hevc", 3840, 2160);
    hlg.hdr = Some(chrysopoeia_core::HdrFormat::DolbyVision);
    hlg.color_transfer = Some("arib-std-b67".into());
    assert!(note_for(hlg).ends_with("standard HLG picture"));
    let mut sdr = video(0, "h264", 1920, 1080);
    sdr.hdr = Some(chrysopoeia_core::HdrFormat::DolbyVision);
    sdr.color_transfer = Some("bt709".into());
    assert!(!note_for(sdr).contains("HDR10"));

    // x265's HDR10 options follow the PQ transfer, not the Dolby Vision flag.
    let hevc = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = common::plan(
        &probe_of("matroska", vec![hdr10_base]),
        &hevc,
        &software(VideoCodec::Hevc),
    );
    assert!(
        value(&plan.args, "-x265-params")
            .unwrap()
            .contains("hdr-opt=1")
    );

    // Profile 5 (no standard layer) is refused even when queued by hand.
    let mut dv5 = video_10bit(0, "hevc", 3840, 2160);
    dv5.hdr = Some(chrysopoeia_core::HdrFormat::DolbyVision);
    let p = probe_of("matroska", vec![dv5]);
    let err = build_plan(&PlanRequest {
        input: Path::new("/media/in.mkv"),
        output: Path::new("/tmp/out.mkv"),
        probe: &p,
        profile: &prof,
        encoder: &enc,
    })
    .expect_err("profile 5 must be refused");
    assert!(err.to_string().contains("Dolby Vision"), "{err}");
}

#[test]
fn quality_override_of_another_scale_is_ignored_with_a_note() {
    let mut prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    prof.quality_override = Some(24);
    // A CRF-style 24 fits libx265 ...
    let plan = plan(&movie(), &prof, &software(VideoCodec::Hevc));
    assert!(has_pair(&plan.args, "-crf", "24"));
    assert!(plan.notes.is_empty(), "{:?}", plan.notes);
    // ... but not VideoToolbox's 1-100 (higher is better) scale.
    let plan = common::plan(&movie(), &prof, &candidate("hevc_videotoolbox", false));
    assert!(has_pair(&plan.args, "-q:v", "60"));
    assert!(
        plan.notes.iter().any(|n| n.contains("(24)")
            && n.contains("with Apple VideoToolbox")
            && n.contains("Balanced")),
        "{:?}",
        plan.notes
    );
    // A value above libx265's maximum was meant for another encoder.
    prof.quality_override = Some(60);
    let plan = common::plan(&movie(), &prof, &software(VideoCodec::Hevc));
    assert!(has_pair(&plan.args, "-crf", "24"));
    assert!(
        plan.notes.iter().any(|n| n.contains("(60)")),
        "{:?}",
        plan.notes
    );
}

// ---------------------------------------------------------------------------
// Cover images

fn cover(index: u32, codec: &str, filename: Option<&str>) -> StreamInfo {
    StreamInfo {
        is_attached_pic: true,
        is_default: false,
        filename: filename.map(Into::into),
        mimetype: filename.map(|_| "image/jpeg".into()),
        ..video(index, codec, 600, 600)
    }
}

/// A source with a styled subtitle, its font and two cover images (one
/// named, one not).
fn covered() -> chrysopoeia_core::ProbeInfo {
    probe_of(
        "matroska",
        vec![
            video(0, "h264", 1920, 1080),
            audio(1, "aac", 2, 48_000, None),
            subtitle(2, "ass", None),
            attachment(3),
            cover(4, "mjpeg", Some("cover.jpg")),
            cover(5, "png", None),
        ],
    )
}

/// The video's own options: everything between `-map_chapters 0` and the
/// first audio, subtitle, cover or muxer option.
fn video_section(args: &[String]) -> Vec<String> {
    let start = pair_pos(args, "-map_chapters", "0").expect("-map_chapters") + 2;
    let end = args[start..]
        .iter()
        .position(|a| {
            a.starts_with("-c:a:")
                || a.starts_with("-c:s:")
                || a == "-c:v:1"
                || a == "-max_muxing_queue_size"
        })
        .map_or(args.len(), |p| start + p);
    args[start..end].to_vec()
}

/// MKV keeps cover images as attachments: they are copied out of the
/// original first and attached, under their own name (or `cover.<ext>`).
#[test]
fn mkv_attaches_cover_images_copied_out_first() {
    use chrysopoeia_worker::{CoverFile, cover_extract_args};
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = plan(&covered(), &prof, &software(VideoCodec::Hevc));
    let a = &plan.args;
    assert_eq!(values(a, "-map"), ["0:0", "0:1", "0:2", "0:3"]);
    assert_eq!(
        plan.covers,
        [
            CoverFile {
                index: 4,
                path: "/tmp/out.cover-1.jpg".into()
            },
            CoverFile {
                index: 5,
                path: "/tmp/out.cover-2.png".into()
            }
        ]
    );
    assert_eq!(
        values(a, "-attach"),
        ["/tmp/out.cover-1.jpg", "/tmp/out.cover-2.png"]
    );
    assert!(pos(a, "-attach").unwrap() > pos(a, "-map").unwrap());
    // The font is attachment 0; the covers follow it.
    assert!(has_pair(a, "-c:t", "copy"));
    assert_eq!(
        values(a, "-metadata:s:t:1"),
        ["mimetype=image/jpeg", "filename=cover.jpg"]
    );
    assert_eq!(
        values(a, "-metadata:s:t:2"),
        ["mimetype=image/png", "filename=cover.png"]
    );
    // Nothing else changes: the video's options are the usual ones.
    assert!(has_pair(a, "-c:v", "libx265"));
    assert_absent(a, "-c:v:1");
    assert!(
        plan.notes.iter().all(|n| !n.contains("cover")),
        "{:?}",
        plan.notes
    );
    assert_eq!(plan.expected.video, 1);

    assert_eq!(
        cover_extract_args(Path::new("/media/in.mkv"), &plan.covers[0]).unwrap(),
        [
            "-hide_banner",
            "-nostdin",
            "-y",
            "-loglevel",
            "error",
            "-nostats",
            "-i",
            "/media/in.mkv",
            "-map",
            "0:4",
            "-c",
            "copy",
            "-frames:v",
            "1",
            "-f",
            "rawvideo",
            "/tmp/out.cover-1.jpg"
        ]
    );
}

/// The copied-out covers are named after the work file, so crash recovery
/// knows them as this job's leftovers and deletes them.
#[test]
fn cover_work_files_are_recognised_leftovers() {
    use chrysopoeia_core::paths::{is_artifact_of, is_backup};
    let job = uuid::Uuid::from_u128(0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
    let temp = chrysopoeia_worker::finalize::temp_output_path(
        Path::new("/media/Movie.mkv"),
        Container::Mkv,
        job,
        Some(Path::new("/work")),
    );
    let prof = profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mkv);
    let plan = build_plan(&PlanRequest {
        input: Path::new("/media/Movie.mkv"),
        output: &temp,
        probe: &covered(),
        profile: &prof,
        encoder: &software(VideoCodec::Hevc),
    })
    .unwrap();
    assert_eq!(plan.covers.len(), 2);
    for cover in &plan.covers {
        assert_eq!(cover.path.parent(), Some(Path::new("/work")));
        let name = cover.path.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(".Movie.chrysopoeia-01234567.tmp.cover-"),
            "{name}"
        );
        assert!(is_artifact_of(name, job) && !is_backup(name), "{name}");
    }
}

/// MP4 keeps JPEG, PNG and BMP covers as cover art: a copied picture
/// stream marked as attached. Every video option then names the video
/// (`v:0`) alone, for every encoder, or ffmpeg would filter, re-encode or
/// tag the cover too.
#[test]
fn mp4_copies_cover_art_and_pins_video_options_to_the_video() {
    let mut without = covered();
    without.streams.retain(|s| !s.is_attached_pic);
    let mut checked = 0;
    for info in chrysopoeia_core::encoder::VIDEO_ENCODERS {
        if !Container::Mp4.supports_video(info.codec) {
            continue;
        }
        for (hw_decode, max_height) in [
            (false, None),
            (true, None),
            (false, Some(720)),
            (true, Some(720)),
        ] {
            let encoder = candidate(info.name, hw_decode);
            let mut prof = profile(info.codec, AudioCodec::Copy, Container::Mp4);
            prof.max_height = max_height;
            let plain = plan(&without, &prof, &encoder);
            let with = plan(&covered(), &prof, &encoder);
            let a = &with.args;
            let context = format!(
                "{} hw_decode={hw_decode} max_height={max_height:?}",
                info.name
            );
            assert_eq!(
                values(a, "-map"),
                ["0:0", "0:1", "0:2", "0:4", "0:5"],
                "{context}"
            );
            assert!(has_pair(a, "-c:v:1", "copy"), "{context}");
            assert!(has_pair(a, "-disposition:v:1", "attached_pic"), "{context}");
            assert!(has_pair(a, "-c:v:2", "copy"), "{context}");
            assert!(has_pair(a, "-disposition:v:2", "attached_pic"), "{context}");
            assert!(with.covers.is_empty(), "{context}");
            assert_eq!(with.expected, plain.expected, "{context}");

            // Same options, same values, each naming the video.
            let before = video_section(&plain.args);
            let after = video_section(a);
            assert_eq!(
                before.len(),
                after.len(),
                "{context}\n{before:?}\n{after:?}"
            );
            assert!(!before.is_empty(), "{context}");
            for (i, (b, w)) in before.iter().zip(&after).enumerate() {
                if i % 2 == 1 {
                    assert_eq!(b, w, "{context}");
                    continue;
                }
                assert!(b.starts_with('-'), "{context}: {b}");
                let pinned = if b == "-vf" {
                    w == "-filter:v:0"
                } else if b.contains(':') && !b.ends_with(":v") {
                    w == b
                } else {
                    w.starts_with(b.as_str()) && w.ends_with(":v:0")
                };
                assert!(pinned, "{context}: {b} became {w}");
                assert!(w.contains(":v:0"), "{context}: {w}");
            }
            // The generic forms are gone everywhere.
            for generic in ["-c:v", "-vf", "-pix_fmt", "-tag:v", "-b:v", "-profile:v"] {
                assert_absent(a, generic);
            }
            checked += 1;
        }
    }
    assert!(checked >= 40, "{checked}");
    // Quick Sync decoding names its decoder for the video stream alone,
    // or ffmpeg would read the cover as H.264 too.
    let qsv = plan(
        &covered(),
        &profile(VideoCodec::H264, AudioCodec::Copy, Container::Mp4),
        &candidate("h264_qsv", true),
    );
    assert!(has_pair(&qsv.args, "-c:0", "h264_qsv"), "{:?}", qsv.args);
    assert!(pos(&qsv.args, "-c:0").unwrap() < input_pos(&qsv.args));
    let qsv_plain = plan(
        &without,
        &profile(VideoCodec::H264, AudioCodec::Copy, Container::Mp4),
        &candidate("h264_qsv", true),
    );
    assert!(has_pair(&qsv_plain.args, "-c:v", "h264_qsv"));
    // HEVC in MP4: only the video is tagged hvc1.
    let hevc = plan(
        &covered(),
        &profile(VideoCodec::Hevc, AudioCodec::Copy, Container::Mp4),
        &software(VideoCodec::Hevc),
    );
    assert!(has_pair(&hevc.args, "-tag:v:0", "hvc1"));
    assert!(has_pair(&hevc.args, "-c:v:0", "libx265"));
}

/// WebM holds no cover image, and MP4 no GIF or WebP one: they are left
/// out with a note, as are fonts and styling.
#[test]
fn covers_the_container_cannot_hold_are_left_out_with_a_note() {
    let webm = plan(
        &covered(),
        &profile(VideoCodec::Vp9, AudioCodec::Opus, Container::Webm),
        &software(VideoCodec::Vp9),
    );
    assert_eq!(values(&webm.args, "-map"), ["0:0", "0:1", "0:2"]);
    assert_absent(&webm.args, "-attach");
    assert_absent(&webm.args, "-c:v:1");
    assert!(webm.covers.is_empty());
    assert!(has_pair(&webm.args, "-c:v", "libvpx-vp9"));
    for note in [
        "Left out 2 cover images because WebM can't hold them",
        "Left out 1 subtitle font because WebM can't hold attachments",
        "Converted 1 styled subtitle to plain text because WebM can't keep its styling",
    ] {
        assert!(
            webm.notes.iter().any(|n| n == note),
            "{note}: {:?}",
            webm.notes
        );
    }

    let mut gif = covered();
    gif.streams[5].codec = "gif".into();
    let mp4 = plan(
        &gif,
        &profile(VideoCodec::H264, AudioCodec::Copy, Container::Mp4),
        &software(VideoCodec::H264),
    );
    assert_eq!(values(&mp4.args, "-map"), ["0:0", "0:1", "0:2", "0:4"]);
    assert!(has_pair(&mp4.args, "-c:v:1", "copy"));
    assert_absent(&mp4.args, "-c:v:2");
    assert!(
        mp4.notes
            .iter()
            .any(|n| n == "Left out 1 cover image because MP4 can't hold it"),
        "{:?}",
        mp4.notes
    );
}
