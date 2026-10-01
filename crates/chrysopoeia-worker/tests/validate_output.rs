//! `validate_output` against real ffmpeg encodes of the synthetic library:
//! good encodes pass, and each kind of damage fails the check meant to catch
//! it.

mod support;

use std::path::Path;
use std::sync::Mutex;

use chrysopoeia_core::{
    CheckStatus, Goal, ProbeInfo, TranscodeProfile, ValidationLevel, ValidationReport, VideoCodec,
};
use chrysopoeia_worker::{StreamSummary, ValidateRequest, validate_output};
use tokio_util::sync::CancellationToken;

fn h264_profile() -> TranscodeProfile {
    TranscodeProfile {
        video_codec: VideoCodec::H264,
        container: chrysopoeia_core::Container::Mkv,
        ..TranscodeProfile::from_goal(Goal::Compatible)
    }
}

/// Encode `source` to an H.264 MKV with extra ffmpeg arguments.
fn encode(source: &Path, output: &Path, video_filter: Option<&str>, extra: &[&str]) {
    let src = source.to_str().unwrap();
    let out = output.to_str().unwrap();
    let mut args = vec!["-i", src, "-map", "0:v:0", "-map", "0:a?"];
    if let Some(vf) = video_filter {
        args.extend(["-vf", vf]);
    }
    args.extend([
        "-c:v", "libx264", "-preset", "veryfast", "-crf", "22", "-pix_fmt", "yuv420p", "-c:a",
        "aac", "-b:a", "128k",
    ]);
    args.extend_from_slice(extra);
    args.extend(["-f", "matroska", out]);
    support::ffmpeg(&args);
}

async fn validate(
    source: &Path,
    source_probe: &ProbeInfo,
    output: &Path,
    level: ValidationLevel,
    expected: StreamSummary,
) -> ValidationReport {
    let profile = h264_profile();
    let req = ValidateRequest {
        ffmpeg: Path::new("ffmpeg"),
        ffprobe: Path::new("ffprobe"),
        source,
        source_probe,
        output,
        profile: &profile,
        level,
        expected,
    };
    let seen = Mutex::new(Vec::new());
    let report = validate_output(&req, &CancellationToken::new(), &|p| {
        seen.lock().unwrap().push(p);
    })
    .await;
    let seen = seen.into_inner().unwrap();
    assert!(seen.iter().all(|p| (0.0..=100.0).contains(p)), "{seen:?}");
    assert_eq!(seen.last().copied(), Some(100.0));
    report
}

fn describe(report: &ValidationReport) -> String {
    report
        .checks
        .iter()
        .map(|c| format!("{:?} {}: {}", c.status, c.label, c.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_all_pass(report: &ValidationReport) {
    eprintln!(
        "{}\nssim min {:?} avg {:?}, psnr {:?}, {:.1} s",
        describe(report),
        report.ssim_min,
        report.ssim_avg,
        report.psnr_avg,
        report.elapsed_secs
    );
    assert!(report.passed, "expected a pass:\n{}", describe(report));
    for check in &report.checks {
        assert!(
            matches!(check.status, CheckStatus::Pass | CheckStatus::Warn),
            "{}",
            describe(report)
        );
        assert!(!check.detail.is_empty());
    }
    // Only the size check may warn (a re-encode can be larger than a tiny
    // synthetic source).
    assert!(
        report
            .checks
            .iter()
            .all(|c| c.status == CheckStatus::Pass || c.id == "size"),
        "{}",
        describe(report)
    );
}

fn first_failure(report: &ValidationReport) -> (&str, &str) {
    let fail = report
        .first_failure()
        .unwrap_or_else(|| panic!("expected a failure:\n{}", describe(report)));
    (fail.label.as_str(), fail.detail.as_str())
}

#[tokio::test]
async fn good_encode_passes_standard_and_thorough() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let output = dir.path().join("out.mkv");
    encode(&source, &output, None, &[]);
    let expected = support::counts(&probe);
    assert_eq!(expected.audio, 2);

    let standard = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Standard,
        expected,
    )
    .await;
    assert_all_pass(&standard);
    assert_eq!(standard.level, ValidationLevel::Standard);
    let ids: Vec<_> = standard.checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        ["probe", "streams", "duration", "size", "decode", "visual"]
    );
    let labels: Vec<_> = standard.checks.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"Plays start to finish"));
    assert!(labels.contains(&"Looks like the original"));
    let ssim_min = standard.ssim_min.unwrap();
    let ssim_avg = standard.ssim_avg.unwrap();
    assert!(ssim_min > 0.9, "ssim_min {ssim_min}");
    assert!(ssim_avg > 0.95, "ssim_avg {ssim_avg}");
    assert!(standard.psnr_avg.unwrap() > 30.0);
    assert!(standard.elapsed_secs > 0.0);

    let thorough = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Thorough,
        expected,
    )
    .await;
    assert_all_pass(&thorough);
    let ids: Vec<_> = thorough.checks.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "probe",
            "streams",
            "duration",
            "size",
            "decode",
            "visual",
            "black_frames",
            "frozen_frames"
        ]
    );
    let labels: Vec<_> = thorough.checks.iter().map(|c| c.label.as_str()).collect();
    assert!(labels.contains(&"No extra black frames"));
    assert!(labels.contains(&"No frozen frames"));

    let quick = validate(&source, &probe, &output, ValidationLevel::Quick, expected).await;
    assert_all_pass(&quick);
    assert_eq!(quick.checks.len(), 4);
    assert_eq!(quick.ssim_avg, None);
}

#[tokio::test]
async fn good_encodes_of_every_clip_pass() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    // (clip, video filter the planner would use, subtitle handling)
    let cases: [(&str, Option<&str>, bool); 3] = [
        (support::MKV_1080P_SUBS, None, true),
        (support::MKV_HEVC_10BIT, None, false),
        // Odd dimensions (639x359) cropped to even, as the planner does.
        (
            support::AVI_ODD_SIZE,
            Some("crop=trunc(iw/2)*2:trunc(ih/2)*2"),
            false,
        ),
    ];
    for (clip, vf, subs) in cases {
        let source = support::copy_media(clip, dir.path());
        let probe = support::probe(&source);
        let output = dir.path().join("out.mkv");
        let extra: &[&str] = if subs {
            &["-map", "0:s?", "-c:s", "copy"]
        } else {
            &[]
        };
        encode(&source, &output, vf, extra);
        let report = validate(
            &source,
            &probe,
            &output,
            ValidationLevel::Standard,
            support::counts(&probe),
        )
        .await;
        assert_all_pass(&report);
        std::fs::remove_file(&output).unwrap();
    }
}

#[tokio::test]
async fn deinterlaced_output_of_interlaced_source_passes() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::TS_INTERLACED, dir.path());
    let probe = support::probe(&source);
    assert!(
        probe.primary_video().unwrap().interlaced,
        "test clip should be interlaced"
    );
    let expected = support::counts(&probe);

    // Frame-rate deinterlacing (25i → 25p).
    let output = dir.path().join("frame.mkv");
    encode(&source, &output, Some("bwdif=mode=send_frame"), &[]);
    let report = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Thorough,
        expected,
    )
    .await;
    assert_all_pass(&report);

    // Field-rate deinterlacing (25i → 50p).
    let output = dir.path().join("field.mkv");
    encode(&source, &output, Some("bwdif=mode=send_field"), &[]);
    let report = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Standard,
        expected,
    )
    .await;
    assert_all_pass(&report);
}

#[tokio::test]
async fn corrupted_bytes_fail_plays_start_to_finish() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let output = dir.path().join("out.mkv");
    encode(&source, &output, None, &[]);
    support::corrupt_middle(&output, 20_000);

    let report = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Standard,
        support::counts(&probe),
    )
    .await;
    assert!(!report.passed);
    let (label, detail) = first_failure(&report);
    assert_eq!(label, "Plays start to finish", "{}", describe(&report));
    assert!(!detail.is_empty());
    // Later checks are reported, but skipped.
    let visual = report.checks.iter().find(|c| c.id == "visual").unwrap();
    assert_eq!(visual.status, CheckStatus::Skipped);
}

#[tokio::test]
async fn different_content_fails_looks_like_the_original() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let secs = support::CLIP_SECS.to_string();
    let output = dir.path().join("other.mkv");
    // Same size, rate, length and tracks; different picture.
    let bars = format!("smptebars=size=1280x720:rate=24:duration={secs}");
    let tone = format!("sine=frequency=300:duration={secs}");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &bars,
        "-f",
        "lavfi",
        "-i",
        &tone,
        "-f",
        "lavfi",
        "-i",
        &tone,
        "-map",
        "0:v",
        "-map",
        "1:a",
        "-map",
        "2:a",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-f",
        "matroska",
        output.to_str().unwrap(),
    ]);

    let report = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Standard,
        support::counts(&probe),
    )
    .await;
    eprintln!(
        "{}\nssim min {:?} avg {:?}",
        describe(&report),
        report.ssim_min,
        report.ssim_avg
    );
    assert!(!report.passed);
    let (label, _) = first_failure(&report);
    assert_eq!(label, "Looks like the original", "{}", describe(&report));
    assert!(report.ssim_avg.unwrap() < 0.85, "{:?}", report.ssim_avg);
}

#[tokio::test]
async fn truncated_output_fails_the_duration_check() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let output = dir.path().join("short.mkv");
    encode(&source, &output, None, &["-t", "2"]);

    let report = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Standard,
        support::counts(&probe),
    )
    .await;
    let (label, detail) = first_failure(&report);
    assert_eq!(
        label,
        "Same length as the original",
        "{}",
        describe(&report)
    );
    assert!(detail.contains("2.0 s"), "{detail}");
    assert!(
        detail.starts_with("The new file is shorter than the original ("),
        "{detail}"
    );
}

#[tokio::test]
async fn file_cut_short_on_disk_fails_plays_start_to_finish() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let output = dir.path().join("cut.mkv");
    encode(&source, &output, None, &[]);
    // Keep the first 60 % of the bytes: the header still claims the full
    // length, as after a full disk or a crash mid-write.
    let bytes = std::fs::read(&output).unwrap();
    std::fs::write(&output, &bytes[..bytes.len() * 6 / 10]).unwrap();

    let report = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Standard,
        support::counts(&probe),
    )
    .await;
    let (label, detail) = first_failure(&report);
    assert_eq!(label, "Plays start to finish", "{}", describe(&report));
    eprintln!("{detail}");
}

#[tokio::test]
async fn missing_audio_fails_the_track_check() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let output = dir.path().join("silent.mkv");
    support::ffmpeg(&[
        "-i",
        source.to_str().unwrap(),
        "-map",
        "0:v:0",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-an",
        "-f",
        "matroska",
        output.to_str().unwrap(),
    ]);

    let report = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Quick,
        support::counts(&probe),
    )
    .await;
    let (label, detail) = first_failure(&report);
    assert_eq!(label, "All tracks present");
    assert_eq!(
        detail,
        "Expected 1 video track and 2 audio tracks but found 1 video track"
    );
}

#[tokio::test]
async fn wrong_video_codec_fails_opens_correctly() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let output = dir.path().join("out.mkv");
    encode(&source, &output, None, &[]);
    let profile = TranscodeProfile::from_goal(Goal::Balanced); // wants HEVC
    let req = ValidateRequest {
        ffmpeg: Path::new("ffmpeg"),
        ffprobe: Path::new("ffprobe"),
        source: &source,
        source_probe: &probe,
        output: &output,
        profile: &profile,
        level: ValidationLevel::Standard,
        expected: support::counts(&probe),
    };
    let report = validate_output(&req, &CancellationToken::new(), &|_| {}).await;
    let (label, detail) = first_failure(&report);
    assert_eq!(label, "Opens correctly");
    assert_eq!(
        detail,
        "The new file's video is H.264 instead of HEVC (H.265)"
    );
}

#[tokio::test]
async fn not_a_media_file_fails_opens_correctly() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let output = dir.path().join("garbage.mkv");
    std::fs::write(&output, b"this is not a video").unwrap();
    let report = validate(
        &source,
        &probe,
        &output,
        ValidationLevel::Thorough,
        support::counts(&probe),
    )
    .await;
    let (label, detail) = first_failure(&report);
    assert_eq!(label, "Opens correctly");
    assert!(
        detail.starts_with("The new file could not be opened"),
        "{detail}"
    );
    assert_eq!(report.checks.len(), 8);
}

#[tokio::test]
async fn cancellation_stops_verification() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let probe = support::probe(&source);
    let output = dir.path().join("out.mkv");
    encode(&source, &output, None, &[]);
    let profile = h264_profile();
    let req = ValidateRequest {
        ffmpeg: Path::new("ffmpeg"),
        ffprobe: Path::new("ffprobe"),
        source: &source,
        source_probe: &probe,
        output: &output,
        profile: &profile,
        level: ValidationLevel::Thorough,
        expected: support::counts(&probe),
    };
    let cancel = CancellationToken::new();
    cancel.cancel();
    let report = validate_output(&req, &cancel, &|_| {}).await;
    assert!(!report.passed);
    assert!(
        report
            .checks
            .iter()
            .any(|c| c.detail == "Verification was cancelled")
    );
}
