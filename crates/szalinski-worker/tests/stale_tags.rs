//! The owner's clip, rebuilt: a short MKV cut from a "film" whose tracks
//! still carry the film's Matroska statistics (`DURATION-eng` 2:21:02,
//! `BPS-eng`, `NUMBER_OF_FRAMES-eng`, `_STATISTICS_*-eng`) next to the
//! clip's own `DURATION` that ffmpeg wrote. Three audio tracks, an ASS and
//! an SRT subtitle and a font attachment, like a real Blu-ray rip.
//!
//! The real planner, real ffmpeg and thorough verification: the job passes
//! on its first attempt, the new file carries none of the old statistics,
//! and a cut-short conversion of the same clip still fails.

mod support;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use szalinski_core::{
    CheckStatus, Container, EncoderCandidate, Goal, HwApi, OutputMode, ProbeInfo, TranscodeProfile,
    ValidationLevel, ValidationReport, VideoCodec,
};
use szalinski_worker::run::{JobOutcome, JobSpec, RunConfig, run_job};
use szalinski_worker::{PlanRequest, ValidateRequest, build_plan, validate_output};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// The film's length, as its old statistics state it.
const FILM: &str = "02:21:02.000000000";
/// Length of the clip, in seconds.
const CLIP: f64 = 6.0;

fn s(p: &Path) -> &str {
    p.to_str().expect("utf-8 path")
}

/// The clip. Every track gets the film's statistics, as mkvmerge v9 wrote
/// them (with the language `eng`), the way an ffmpeg cut keeps them.
fn reproduction(dir: &Path) -> PathBuf {
    let srt = dir.join("subs.srt");
    std::fs::write(
        &srt,
        "1\n00:00:00,500 --> 00:00:02,000\nHello\n\n2\n00:00:03,000 --> 00:00:05,500\nWorld\n",
    )
    .expect("write subtitles");
    // Attachments are copied byte for byte; the content doesn't matter.
    let font = dir.join("Font.ttf");
    std::fs::write(&font, [0u8, 1, 0, 0, 0, 4, 0, 0]).expect("write font");

    let clip = dir.join("clip.mkv");
    let secs = CLIP.to_string();
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!("testsrc2=size=640x360:rate=24:duration={secs}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=440:duration={secs}"),
        "-f",
        "lavfi",
        "-i",
        &format!("sine=frequency=660:duration={secs}"),
        "-i",
        s(&srt),
        "-map",
        "0",
        "-map",
        "1",
        "-map",
        "2",
        "-map",
        "2",
        "-map",
        "3",
        "-map",
        "3",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-crf",
        "14",
        "-pix_fmt",
        "yuv420p",
        "-c:a:0",
        "aac",
        "-c:a:1",
        "ac3",
        "-c:a:2",
        "aac",
        "-c:s:0",
        "ass",
        "-c:s:1",
        "srt",
        "-metadata:s:a:0",
        "language=eng",
        "-metadata:s:a:1",
        "language=ger",
        "-metadata:s:a:2",
        "language=eng",
        "-metadata:s:a:2",
        "title=Commentary",
        "-metadata:s:s:0",
        "language=eng",
        "-metadata:s:s:1",
        "language=fre",
        s(&clip),
    ]);

    let film = dir.join("Film (2017).mkv");
    let mut args: Vec<String> = [
        "-i",
        s(&clip),
        "-map",
        "0",
        "-c",
        "copy",
        "-attach",
        s(&font),
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    args.extend(
        [
            "-metadata:s:t:0",
            "mimetype=application/x-truetype-font",
            "-metadata:s:t:0",
            "filename=Font.ttf",
        ]
        .iter()
        .map(|a| a.to_string()),
    );
    for spec in ["v:0", "a:0", "a:1", "a:2", "s:0", "s:1"] {
        for (key, value) in [
            ("DURATION-eng", FILM),
            ("BPS-eng", "61234567"),
            ("NUMBER_OF_FRAMES-eng", "203088"),
            ("NUMBER_OF_BYTES-eng", "64765432109"),
            (
                "_STATISTICS_WRITING_APP-eng",
                "mkvmerge v9.8.0 ('Kuglblitz') 64-bit",
            ),
            ("_STATISTICS_WRITING_DATE_UTC-eng", "2017-12-01 10:00:00"),
            (
                "_STATISTICS_TAGS-eng",
                "BPS DURATION NUMBER_OF_FRAMES NUMBER_OF_BYTES",
            ),
        ] {
            args.push(format!("-metadata:s:{spec}"));
            args.push(format!("{key}={value}"));
        }
    }
    args.push(s(&film).to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    support::ffmpeg(&refs);
    std::fs::remove_file(&clip).ok();
    std::fs::remove_file(&srt).ok();
    std::fs::remove_file(&font).ok();
    film
}

/// ffprobe's streams (with their tags) and format, as JSON.
fn ffprobe_json(path: &Path) -> Value {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
        ])
        .arg(path)
        .output()
        .expect("run ffprobe");
    assert!(
        output.status.success(),
        "ffprobe failed on {}",
        path.display()
    );
    serde_json::from_slice(&output.stdout).expect("ffprobe json")
}

fn tags(stream: &Value) -> Vec<(String, String)> {
    stream
        .get("tags")
        .and_then(Value::as_object)
        .map(|t| {
            t.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or_default().to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn streams(json: &Value) -> Vec<Value> {
    json["streams"].as_array().cloned().unwrap_or_default()
}

/// Seconds in a `DURATION` tag value.
fn clock(value: &str) -> f64 {
    szalinski_core::tags::parse_clock(value).unwrap_or(0.0)
}

fn describe(report: &ValidationReport) -> String {
    report
        .checks
        .iter()
        .map(|c| format!("  {:?} {}: {}", c.status, c.label, c.detail))
        .collect::<Vec<_>>()
        .join("\n")
}

fn check<'r>(report: &'r ValidationReport, id: &str) -> &'r szalinski_core::ValidationCheck {
    report
        .checks
        .iter()
        .find(|c| c.id == id)
        .unwrap_or_else(|| panic!("no {id} check:\n{}", describe(report)))
}

/// The Balanced goal (HEVC in MKV, audio copied), as the owner's library,
/// without the size rule (a synthetic picture compresses unpredictably).
fn balanced() -> TranscodeProfile {
    TranscodeProfile {
        min_savings_pct: None,
        ..TranscodeProfile::from_goal(Goal::Balanced)
    }
}

fn libx265() -> EncoderCandidate {
    EncoderCandidate {
        name: "libx265".into(),
        codec: VideoCodec::Hevc,
        api: HwApi::Software,
        device: None,
        hw_decode: false,
    }
}

fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(name))
}

/// The source as the scanner sees it: the film's tags are listed, the
/// length is the clip's.
fn assert_reproduces_the_owners_clip(film: &Path, probe: &ProbeInfo) {
    let json = ffprobe_json(film);
    let all = streams(&json);
    assert_eq!(all.len(), 7, "video, 3 audio, 2 subtitles, 1 font");
    for stream in &all[..6] {
        let tags = tags(stream);
        let get = |key: &str| tags.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(get("DURATION-eng").as_deref(), Some(FILM), "{stream}");
        let fresh = get("DURATION").map(|v| clock(&v)).unwrap_or_default();
        assert!((fresh - CLIP).abs() < 1.0, "{stream}");
    }
    assert!(probe.duration_secs.is_some_and(|d| (d - CLIP).abs() < 1.0));
    assert!(
        probe.streams[0]
            .statistics_tags
            .iter()
            .any(|k| k == "DURATION-eng")
    );
}

/// What the new file must look like: none of the film's statistics on any
/// track, ffmpeg's own `DURATION` for the clip on every one, and titles,
/// languages and the font kept.
fn assert_no_stale_statistics(output: &Path) {
    let json = ffprobe_json(output);
    let all = streams(&json);
    assert_eq!(all.len(), 7, "{json}");
    for stream in &all {
        let tags = tags(stream);
        let stale: Vec<&(String, String)> = tags
            .iter()
            .filter(|(k, _)| szalinski_core::tags::is_statistics_tag(k) && k != "DURATION")
            .collect();
        assert!(stale.is_empty(), "stale statistics {stale:?} in {stream}");
        assert!(
            !tags.iter().any(|(_, v)| v == FILM),
            "the film's length is still there: {stream}"
        );
        if stream["codec_type"] != "attachment" {
            let duration = tags
                .iter()
                .find(|(k, _)| k == "DURATION")
                .map(|(_, v)| clock(v))
                .unwrap_or_default();
            assert!(duration > 0.0 && duration <= CLIP + 1.0, "{stream}");
        }
    }
    let tag = |i: usize, key: &str| {
        tags(&all[i])
            .into_iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v)
    };
    assert_eq!(tag(1, "language").as_deref(), Some("eng"));
    assert_eq!(tag(2, "language").as_deref(), Some("ger"));
    assert_eq!(tag(3, "title").as_deref(), Some("Commentary"));
    assert_eq!(tag(5, "language").as_deref(), Some("fre"));
    assert_eq!(tag(6, "filename").as_deref(), Some("Font.ttf"));
    assert_eq!(all[4]["codec_name"], "ass");
    assert_eq!(all[5]["codec_name"], "subrip");
}

/// (a) + (b): the whole job, as the server runs it, passes thorough
/// verification on its first attempt, and the new file has no stale
/// statistics.
#[tokio::test]
async fn a_clip_with_the_films_statistics_converts_and_verifies() {
    require_ffmpeg!();
    if !has_encoder("libx265") {
        eprintln!("this ffmpeg has no libx265; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let film = reproduction(dir.path());
    let probe = support::probe(&film);
    assert_reproduces_the_owners_clip(&film, &probe);

    let cfg = RunConfig {
        ffmpeg: PathBuf::from("ffmpeg"),
        ffprobe: PathBuf::from("ffprobe"),
        temp_dir: None,
        validation: ValidationLevel::Thorough,
        output_mode: OutputMode::Replace,
        output_folder: None,
        keep_file_dates: true,
        low_priority: false,
    };
    let spec = JobSpec {
        job_id: Uuid::new_v4(),
        file_id: Uuid::new_v4(),
        input: film.clone(),
        library_root: dir.path().to_path_buf(),
        probe,
        profile: balanced(),
        candidates: vec![libx265()],
        force: false,
        mounts: Vec::new(),
    };
    let (tx, mut rx) = mpsc::channel(4096);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let outcome = run_job(&cfg, &spec, tx, CancellationToken::new()).await;
    drain.await.ok();

    let JobOutcome::Done {
        output_path,
        attempt,
        validation,
        command,
        ..
    } = outcome
    else {
        panic!("expected the job to finish, got {outcome:?}");
    };
    assert_eq!(attempt, 1);
    let report = validation.expect("verified");
    eprintln!("{}", describe(&report));
    assert!(report.passed, "{}", describe(&report));
    assert_eq!(report.level, ValidationLevel::Thorough);
    for c in &report.checks {
        assert!(
            matches!(c.status, CheckStatus::Pass | CheckStatus::Warn) || c.id == "size",
            "{}",
            describe(&report)
        );
        assert_ne!(c.status, CheckStatus::Skipped, "{}", describe(&report));
    }
    // The clip's length on both sides, never the film's.
    let length = check(&report, "duration");
    assert!(
        length.detail.starts_with("Matches the original (6.") && length.detail.ends_with(" s)"),
        "{}",
        describe(&report)
    );
    let decode = check(&report, "decode");
    assert_eq!(decode.status, CheckStatus::Pass);
    assert!(
        decode.detail.starts_with("Decoded all 6."),
        "{}",
        describe(&report)
    );
    assert!(
        decode.value.is_some_and(|secs| (secs - CLIP).abs() < 0.5),
        "{}",
        describe(&report)
    );
    for id in ["visual", "black_frames", "frozen_frames"] {
        assert_eq!(check(&report, id).status, CheckStatus::Pass, "{id}");
    }
    assert!(command.contains("DURATION-eng="), "{command}");

    assert_eq!(output_path, film, "replaced in place");
    assert_no_stale_statistics(&output_path);
}

/// The plan (as the job ran it), for a hand-made attempt.
fn plan_args(film: &Path, probe: &ProbeInfo, output: &Path) -> Vec<String> {
    build_plan(&PlanRequest {
        input: film,
        output,
        probe,
        profile: &balanced(),
        encoder: &libx265(),
    })
    .expect("plan")
    .args
}

async fn verify(film: &Path, probe: &ProbeInfo, output: &Path) -> ValidationReport {
    let profile = balanced();
    let req = ValidateRequest {
        ffmpeg: Path::new("ffmpeg"),
        ffprobe: Path::new("ffprobe"),
        source: film,
        source_probe: probe,
        output,
        profile: &profile,
        level: ValidationLevel::Thorough,
        expected: support::counts(probe),
    };
    let report = validate_output(&req, &CancellationToken::new(), &|_| {}).await;
    eprintln!("{}:\n{}", output.display(), describe(&report));
    report
}

/// (c): conversions of the same clip cut short still fail, whether the new
/// file says it is short or claims the whole length.
#[tokio::test]
async fn a_cut_short_conversion_of_the_clip_still_fails() {
    require_ffmpeg!();
    if !has_encoder("libx265") {
        eprintln!("this ffmpeg has no libx265; skipping");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let film = reproduction(dir.path());
    let probe = support::probe(&film);
    assert_eq!(probe.container, "matroska");

    // The encode stopped half way: the new file says 3 s.
    let short = dir.path().join("short.mkv");
    let mut args = plan_args(&film, &probe, &short);
    let at = args.iter().rposition(|a| a == "-f").expect("-f");
    args.splice(at..at, ["-t".to_string(), "3".to_string()]);
    let ran = Command::new("ffmpeg").args(&args).output().expect("ffmpeg");
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let report = verify(&film, &probe, &short).await;
    assert!(!report.passed);
    let duration = check(&report, "duration");
    assert_eq!(duration.status, CheckStatus::Fail);
    assert!(
        duration
            .detail
            .starts_with("The new file is shorter than the original (3.")
            && duration.detail.contains(" s instead of 6."),
        "{}",
        describe(&report)
    );

    // A whole conversion, then its second half lost on disk: the header
    // still claims the whole length, the pictures stop early.
    let whole = dir.path().join("whole.mkv");
    let args = plan_args(&film, &probe, &whole);
    let ran = Command::new("ffmpeg").args(&args).output().expect("ffmpeg");
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    assert_no_stale_statistics(&whole);
    let full = verify(&film, &probe, &whole).await;
    assert!(
        full.passed,
        "the whole conversion passes:\n{}",
        describe(&full)
    );

    let cut = dir.path().join("cut.mkv");
    let bytes = std::fs::read(&whole).expect("read");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).expect("write");
    let report = verify(&film, &probe, &cut).await;
    assert!(!report.passed, "{}", describe(&report));
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .map(|c| c.id.as_str())
        .collect();
    // ffmpeg reads the cut as damage ("File ended prematurely") or as an
    // early end ("Playback stopped at …"); either way it fails.
    assert!(
        failed == ["duration"] || failed == ["decode"],
        "{}",
        describe(&report)
    );
}

/// The same clip converted to MP4 (Plays everywhere): MP4 never keeps the
/// statistics, and verification measures the clip's real length.
#[tokio::test]
async fn the_clip_converted_to_mp4_verifies() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let film = reproduction(dir.path());
    let probe = support::probe(&film);
    let profile = TranscodeProfile {
        container: Container::Mp4,
        ..TranscodeProfile::from_goal(Goal::Compatible)
    };
    let encoder = EncoderCandidate {
        name: "libx264".into(),
        codec: VideoCodec::H264,
        api: HwApi::Software,
        device: None,
        hw_decode: false,
    };
    let output = dir.path().join("Film (2017).mp4");
    let plan = build_plan(&PlanRequest {
        input: &film,
        output: &output,
        probe: &probe,
        profile: &profile,
        encoder: &encoder,
    })
    .expect("plan");
    let ran = Command::new("ffmpeg")
        .args(&plan.args)
        .output()
        .expect("ffmpeg");
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    for stream in streams(&ffprobe_json(&output)) {
        assert!(
            !tags(&stream)
                .iter()
                .any(|(k, _)| szalinski_core::tags::is_statistics_tag(k)),
            "{stream}"
        );
    }
    let req = ValidateRequest {
        ffmpeg: Path::new("ffmpeg"),
        ffprobe: Path::new("ffprobe"),
        source: &film,
        source_probe: &probe,
        output: &output,
        profile: &profile,
        level: ValidationLevel::Thorough,
        expected: plan.expected,
    };
    let report = validate_output(&req, &CancellationToken::new(), &|_| {}).await;
    eprintln!("{}", describe(&report));
    assert!(report.passed, "{}", describe(&report));
    assert_eq!(check(&report, "duration").status, CheckStatus::Pass);
    assert_eq!(check(&report, "decode").status, CheckStatus::Pass);
}
