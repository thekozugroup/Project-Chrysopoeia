//! Verification against real-world content the first version got wrong:
//! grainy film, MP4's padded first frames, damage the original already had,
//! corruption only a warning reveals, grainy still shots and short bursts of
//! corruption between the sampled segments.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use chrysopoeia_core::{
    CheckStatus, Container, Goal, TranscodeProfile, ValidationLevel, ValidationReport, VideoCodec,
};
use chrysopoeia_worker::{ValidateRequest, validate_output};
use tokio_util::sync::CancellationToken;

fn profile(codec: VideoCodec, container: Container) -> TranscodeProfile {
    TranscodeProfile {
        video_codec: codec,
        container,
        ..TranscodeProfile::from_goal(Goal::Compatible)
    }
}

async fn validate(
    source: &Path,
    output: &Path,
    level: ValidationLevel,
    codec: VideoCodec,
    container: Container,
) -> ValidationReport {
    let probe = support::probe(source);
    let profile = profile(codec, container);
    let req = ValidateRequest {
        ffmpeg: Path::new("ffmpeg"),
        ffprobe: Path::new("ffprobe"),
        source,
        source_probe: &probe,
        output,
        profile: &profile,
        level,
        expected: support::counts(&probe),
    };
    let report = validate_output(&req, &CancellationToken::new(), &|_| {}).await;
    eprintln!(
        "{} ({level:?}):\n{}\nssim min {:?} avg {:?}, psnr {:?}, {:.1} s",
        output.display(),
        describe(&report),
        report.ssim_min,
        report.ssim_avg,
        report.psnr_avg,
        report.elapsed_secs
    );
    report
}

fn describe(report: &ValidationReport) -> String {
    report
        .checks
        .iter()
        .map(|c| format!("  {:?} {}: {}", c.status, c.label, c.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

fn check<'r>(report: &'r ValidationReport, id: &str) -> &'r chrysopoeia_core::ValidationCheck {
    report
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no {id} check:\n{}", describe(report)))
}

/// Passed, and the picture comparison is a clean pass (not a warning).
fn assert_looks_right(report: &ValidationReport) {
    assert!(report.passed, "expected a pass:\n{}", describe(report));
    assert_eq!(
        check(report, "visual").status,
        CheckStatus::Pass,
        "{}",
        describe(report)
    );
}

fn path(dir: &Path, name: &str) -> PathBuf {
    dir.join(name)
}

fn s(p: &Path) -> &str {
    p.to_str().expect("utf-8 path")
}

/// Whether ffmpeg has an encoder (the distro build may lack SVT-AV1).
fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(name))
}

/// A 1080p source with strong temporal film grain, as on many Blu-rays.
fn grainy_source(dir: &Path) -> PathBuf {
    let out = path(dir, "grainy.mkv");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1920x1080:rate=24:duration=4",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=4",
        "-vf",
        "noise=c0s=12:c0f=t+u:c1s=4:c1f=t+u:c2s=4:c2f=t+u",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "17",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        s(&out),
    ]);
    out
}

#[tokio::test]
async fn grainy_film_at_normal_quality_passes() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = grainy_source(dir.path());

    // (encoder arguments, target codec) at the quality the planner's
    // "Balanced" level uses or below.
    let mut cases: Vec<(Vec<&str>, VideoCodec, &str)> = vec![
        (
            vec!["-c:v", "libx264", "-preset", "veryfast", "-crf", "28"],
            VideoCodec::H264,
            "x264.mkv",
        ),
        (
            vec![
                "-c:v",
                "libx265",
                "-preset",
                "superfast",
                "-crf",
                "28",
                "-x265-params",
                "log-level=error",
            ],
            VideoCodec::Hevc,
            "x265.mkv",
        ),
    ];
    if has_encoder("libsvtav1") {
        cases.push((
            vec!["-c:v", "libsvtav1", "-preset", "12", "-crf", "35"],
            VideoCodec::Av1,
            "av1.mkv",
        ));
    }
    for (encoder, codec, name) in cases {
        let output = path(dir.path(), name);
        let mut args = vec!["-i", s(&source), "-map", "0:v", "-map", "0:a"];
        args.extend(encoder);
        args.extend(["-c:a", "aac", "-loglevel", "quiet", s(&output)]);
        support::ffmpeg(&args);
        let report = validate(
            &source,
            &output,
            ValidationLevel::Standard,
            codec,
            Container::Mkv,
        )
        .await;
        assert_looks_right(&report);
    }
}

#[tokio::test]
async fn dark_grainy_scene_passes() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = path(dir.path(), "dark.mkv");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=0x101010:size=1920x1080:rate=24:duration=4",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=4",
        "-vf",
        "noise=c0s=12:c0f=t+u",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "17",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        s(&source),
    ]);
    let output = path(dir.path(), "dark28.mkv");
    support::ffmpeg(&[
        "-i",
        s(&source),
        "-map",
        "0",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "28",
        "-c:a",
        "copy",
        s(&output),
    ]);
    let report = validate(
        &source,
        &output,
        ValidationLevel::Standard,
        VideoCodec::H264,
        Container::Mkv,
    )
    .await;
    assert_looks_right(&report);
}

#[tokio::test]
async fn mp4_frames_padded_at_the_start_do_not_misalign_the_comparison() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();

    // An ordinary MKV (AAC starts 23 ms before the video) with a steady pan,
    // so a picture one frame off matches badly.
    let mkv = path(dir.path(), "pan.mkv");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=24:duration=6,scroll=horizontal=0.008",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=6",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        s(&mkv),
    ]);
    // A broadcast recording: timestamps from 10 hours, video 0.7 s after
    // the sound.
    let ts = path(dir.path(), "broadcast.ts");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=25:duration=8,scroll=horizontal=0.008",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=9",
        "-filter_complex",
        "[0:v]setpts=PTS+0.7/TB[v]",
        "-map",
        "[v]",
        "-map",
        "1:a",
        "-c:v",
        "mpeg2video",
        "-b:v",
        "8M",
        "-c:a",
        "mp2",
        "-output_ts_offset",
        "36000",
        "-f",
        "mpegts",
        s(&ts),
    ]);

    for (source, level) in [
        (&mkv, ValidationLevel::Standard),
        (&ts, ValidationLevel::Thorough),
    ] {
        // Default settings: MP4 is written at a constant frame rate, which
        // repeats the first picture to fill the gap before the video.
        let output = path(dir.path(), "out.mp4");
        support::ffmpeg(&[
            "-i",
            s(source),
            "-map",
            "0:v",
            "-map",
            "0:a",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "23",
            "-c:a",
            "aac",
            s(&output),
        ]);
        let report = validate(source, &output, level, VideoCodec::H264, Container::Mp4).await;
        assert_looks_right(&report);
        assert!(report.ssim_avg.unwrap() > 0.95, "{:?}", report.ssim_avg);
        std::fs::remove_file(&output).unwrap();
    }
}

#[tokio::test]
async fn damage_the_original_already_has_only_warns() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let clean = support::copy_media(support::MKV_1080P_SUBS, dir.path());
    // An old recording whose AC-3 track has bit errors.
    let source = path(dir.path(), "damaged.mkv");
    support::ffmpeg(&[
        "-i",
        s(&clean),
        "-map",
        "0",
        "-c",
        "copy",
        "-bsf:a",
        "noise=amount=2000",
        s(&source),
    ]);
    // Re-encode the video, copy the (damaged) sound and subtitles.
    let output = path(dir.path(), "out.mkv");
    support::ffmpeg(&[
        "-i",
        s(&source),
        "-map",
        "0",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "22",
        "-c:a",
        "copy",
        "-c:s",
        "copy",
        s(&output),
    ]);
    let report = validate(
        &source,
        &output,
        ValidationLevel::Standard,
        VideoCodec::H264,
        Container::Mkv,
    )
    .await;
    assert!(report.passed, "{}", describe(&report));
    let decode = check(&report, "decode");
    assert_eq!(decode.status, CheckStatus::Warn, "{}", describe(&report));
    assert!(decode.detail.contains("audio (ac3)"), "{}", decode.detail);

    // The same damage added to a clean original's copy is new: it fails.
    let report = validate(
        &clean,
        &output,
        ValidationLevel::Standard,
        VideoCodec::H264,
        Container::Mkv,
    )
    .await;
    assert!(!report.passed);
    let decode = check(&report, "decode");
    assert_eq!(decode.status, CheckStatus::Fail, "{}", describe(&report));
    assert!(decode.detail.contains("(ac3)"), "{}", decode.detail);
}

#[tokio::test]
async fn corruption_reported_only_as_a_warning_fails_plays_start_to_finish() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let output = path(dir.path(), "out.mp4");
    support::ffmpeg(&[
        "-i",
        s(&source),
        "-map",
        "0:v",
        "-map",
        "0:a",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "22",
        "-c:a",
        "aac",
        s(&output),
    ]);
    // 16 bytes in the middle: the H.264 decoder conceals the damage and
    // ffmpeg prints only "corrupt decoded frame", a warning.
    support::corrupt_middle(&output, 16);
    let errors = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-v",
            "error",
            "-i",
            s(&output),
            "-f",
            "null",
            "-",
        ])
        .output()
        .unwrap();
    if !errors.stderr.is_empty() {
        eprintln!(
            "note: this ffmpeg also reports an error here, so the warning-only path is not \
             exercised: {}",
            String::from_utf8_lossy(&errors.stderr)
        );
    }

    let report = validate(
        &source,
        &output,
        ValidationLevel::Standard,
        VideoCodec::H264,
        Container::Mp4,
    )
    .await;
    assert!(!report.passed);
    let failure = report.first_failure().unwrap();
    assert_eq!(
        failure.label,
        "Plays start to finish",
        "{}",
        describe(&report)
    );
    assert!(
        failure.detail.starts_with("Found a playback error: "),
        "{}",
        failure.detail
    );
}

#[tokio::test]
async fn a_grainy_still_shot_is_not_a_new_freeze() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    // Moving picture, then a locked-off grey card with film grain (moving
    // only because of the grain), then moving picture again.
    let source = path(dir.path(), "still.mkv");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=24:duration=3",
        "-f",
        "lavfi",
        "-i",
        "color=c=0x808080:size=1280x720:rate=24:duration=6",
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=1280x720:rate=24:duration=3",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=12",
        "-filter_complex",
        "[1:v]noise=c0s=6:c0f=t+u[g];[0:v][g][2:v]concat=n=3:v=1:a=0,format=yuv420p[v]",
        "-map",
        "[v]",
        "-map",
        "3:a",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "16",
        "-c:a",
        "aac",
        s(&source),
    ]);
    let output = path(dir.path(), "out.mkv");
    support::ffmpeg(&[
        "-i",
        s(&source),
        "-map",
        "0",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "28",
        "-c:a",
        "copy",
        s(&output),
    ]);
    let report = validate(
        &source,
        &output,
        ValidationLevel::Thorough,
        VideoCodec::H264,
        Container::Mkv,
    )
    .await;
    assert!(report.passed, "{}", describe(&report));
    assert_eq!(check(&report, "frozen_frames").status, CheckStatus::Pass);
    assert_eq!(check(&report, "black_frames").status, CheckStatus::Pass);
}

#[tokio::test]
async fn a_short_burst_between_the_samples_fails_thorough() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let source = path(dir.path(), "long.mkv");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=24:duration=60",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=60",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "18",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        s(&source),
    ]);
    // Half the picture turns green for 1.4 s, away from every sampled
    // segment: what a failing GPU encoder can produce.
    let output = path(dir.path(), "burst.mkv");
    support::ffmpeg(&[
        "-i",
        s(&source),
        "-map",
        "0",
        "-vf",
        "drawbox=x=0:y=0:w=iw/2:h=ih:color=green:t=fill:enable='between(t,13,14.4)'",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "23",
        "-c:a",
        "copy",
        s(&output),
    ]);

    let standard = validate(
        &source,
        &output,
        ValidationLevel::Standard,
        VideoCodec::H264,
        Container::Mkv,
    )
    .await;
    assert!(
        standard.passed,
        "the burst is between the standard samples:\n{}",
        describe(&standard)
    );

    let thorough = validate(
        &source,
        &output,
        ValidationLevel::Thorough,
        VideoCodec::H264,
        Container::Mkv,
    )
    .await;
    assert!(!thorough.passed);
    let failure = thorough.first_failure().unwrap();
    assert_eq!(
        failure.label,
        "Looks like the original",
        "{}",
        describe(&thorough)
    );
    assert!(
        failure.detail.starts_with("The picture from 12.")
            || failure.detail.starts_with("The picture from 13."),
        "{}",
        failure.detail
    );
}

#[tokio::test]
async fn a_truly_odd_sized_source_scaled_to_even_passes() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    // FFV1 keeps 639x359 exactly (MPEG-4 Part 2 would round it).
    let source = path(dir.path(), "odd.mkv");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=640x360:rate=25:duration=4,format=yuv444p,crop=639:359:0:0",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=4",
        "-c:v",
        "ffv1",
        "-c:a",
        "flac",
        s(&source),
    ]);
    let probe = support::probe(&source);
    let video = probe.primary_video().unwrap();
    assert_eq!((video.width, video.height), (Some(639), Some(359)));
    // Scaled to even, as the planner does.
    let output = path(dir.path(), "even.mkv");
    support::ffmpeg(&[
        "-i",
        s(&source),
        "-map",
        "0",
        "-vf",
        "scale=trunc(iw/2)*2:trunc(ih/2)*2:flags=lanczos",
        "-c:v",
        "libx264",
        "-preset",
        "veryfast",
        "-crf",
        "23",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        s(&output),
    ]);
    let report = validate(
        &source,
        &output,
        ValidationLevel::Standard,
        VideoCodec::H264,
        Container::Mkv,
    )
    .await;
    assert_looks_right(&report);
}
