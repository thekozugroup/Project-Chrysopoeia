//! Originals whose stated length can't be taken as it is, rebuilt with
//! real ffmpeg: a short clip cut from a "film" whose tracks still carry the
//! film's `DURATION-eng` (2:21:02) and nothing else to say how long they
//! are, written
//!
//! - as a live stream (`-live 1`): no length of its own, so the film's
//!   tag was trusted, every attempt failed and the original was called
//!   "damaged or incomplete";
//! - with timestamps from 10:00, through a pipe: ffmpeg 7 states the clip's
//!   length (1:01-style) while its packets end at 10:00 plus that, ffmpeg 6
//!   states none;
//! - with timestamps from 10:00 and fresh tags: ffmpeg's writer states where
//!   the clip ends (11:01-style) as its length and in every `DURATION`.
//!
//! Each converts and passes thorough verification on its first attempt,
//! the source probed as the server probes it. A conversion of each cut
//! short still fails, the live-stream clip really cut short is still
//! reported as damaged, and statistics tags stored for the whole file are
//! removed from the new MKV.
//!
//! Originals that state no length at all (a live-stream MKV without tags,
//! a raw H.264 stream) are measured by their packets: a conversion cut to
//! 30 s no longer replaces the 61 s original, a complete one passes with
//! every check run, and one whose packets can't be listed is kept.

mod support;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde_json::Value;
use szalinski_core::{
    CheckStatus, EncoderCandidate, Goal, HwApi, OutputMode, ProbeInfo, ProblemKind,
    TranscodeProfile, ValidationLevel, ValidationReport, VideoCodec,
};
use szalinski_worker::run::{JobOutcome, JobSpec, RunConfig, run_job};
use szalinski_worker::{PlanRequest, ValidateRequest, build_plan, validate_output};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// The film's length, as its old statistics state it.
const FILM: &str = "02:21:02.000000000";
const FILM_SECS: f64 = 8462.0;
/// Length of the clip, in seconds.
const CLIP: f64 = 6.0;
/// Where the shifted clips' timestamps start.
const OFFSET: &str = "600";

fn s(p: &Path) -> &str {
    p.to_str().expect("utf-8 path")
}

/// The clip as ffmpeg encodes it: picture, sound and a subtitle.
fn clip(dir: &Path) -> PathBuf {
    let srt = dir.join("subs.srt");
    std::fs::write(
        &srt,
        "1\n00:00:00,500 --> 00:00:02,000\nHello\n\n2\n00:00:03,000 --> 00:00:05,500\nWorld\n",
    )
    .expect("write subtitles");
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
        "-i",
        s(&srt),
        "-map",
        "0",
        "-map",
        "1",
        "-map",
        "2",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-crf",
        "14",
        "-g",
        "48",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        "-c:s",
        "srt",
        "-metadata:s:a:0",
        "language=eng",
        s(&clip),
    ]);
    std::fs::remove_file(&srt).ok();
    clip
}

/// ffmpeg arguments that copy the clip with the film's statistics on
/// every track and none of the clip's own (as an mkvmerge v9 rip cut by
/// ffmpeg would keep them, with `extra` options before the output).
fn copy_with_films_tags(clip: &Path, extra: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "-i",
        s(clip),
        "-map",
        "0",
        "-c",
        "copy",
        "-map_metadata",
        "-1",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    for spec in ["v:0", "a:0", "s:0"] {
        for (key, value) in [
            ("DURATION-eng", FILM),
            ("BPS-eng", "61234567"),
            ("NUMBER_OF_FRAMES-eng", "203088"),
            (
                "_STATISTICS_WRITING_APP-eng",
                "mkvmerge v9.8.0 ('Kuglblitz') 64-bit",
            ),
        ] {
            args.push(format!("-metadata:s:{spec}"));
            args.push(format!("{key}={value}"));
        }
    }
    args.push("-metadata:s:a:0".into());
    args.push("language=eng".into());
    args.extend(extra.iter().map(|a| a.to_string()));
    args
}

/// The clip written as a live stream: no length but the film's tags.
fn live(dir: &Path, clip: &Path) -> PathBuf {
    let out = dir.join("Live (2017).mkv");
    let mut args = copy_with_films_tags(clip, &["-live", "1"]);
    args.push(s(&out).to_string());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    support::ffmpeg(&refs);
    out
}

/// The clip with timestamps from 10:00 and only the film's tags, written
/// through a real pipe (ffmpeg then writes no `DURATION` of its own).
fn shifted_through_a_pipe(dir: &Path, clip: &Path) -> PathBuf {
    let out = dir.join("Shifted (2017).mkv");
    let mut args = copy_with_films_tags(clip, &["-output_ts_offset", OFFSET]);
    args.extend(["-f", "matroska", "pipe:1"].iter().map(|a| a.to_string()));
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(&args)
        .stdout(Stdio::piped())
        .spawn()
        .expect("run ffmpeg");
    let mut file = std::fs::File::create(&out).expect("create");
    let mut stdout = child.stdout.take().expect("stdout");
    std::io::copy(&mut stdout, &mut file).expect("copy the pipe");
    assert!(child.wait().expect("ffmpeg").success());
    out
}

/// The clip with timestamps from 10:00 and fresh tags (ffmpeg's own
/// `DURATION`, which states where each track ends), plus the film's
/// statistics in the file's own tags next to its title.
fn shifted_with_fresh_tags(dir: &Path, clip: &Path) -> PathBuf {
    let out = dir.join("Fresh (2017).mkv");
    support::ffmpeg(&[
        "-i",
        s(clip),
        "-map",
        "0",
        "-c",
        "copy",
        "-output_ts_offset",
        OFFSET,
        "-metadata",
        "title=The Film",
        "-metadata",
        &format!("DURATION-eng={FILM}"),
        "-metadata",
        "BPS-eng=61234567",
        s(&out),
    ]);
    out
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

fn format_field(json: &Value, key: &str) -> Option<f64> {
    json["format"][key].as_str().and_then(|v| v.parse().ok())
}

/// The source as the server probes it (the length checked against the
/// packets when it can't be taken as stated).
async fn scan(path: &Path) -> ProbeInfo {
    szalinski_scanner::probe_file(Path::new("ffprobe"), path, Duration::from_secs(60))
        .await
        .expect("probe")
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

/// The Balanced goal (HEVC in MKV), without the size rule (a synthetic
/// picture compresses unpredictably).
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

fn has_libx265() -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("libx265"))
}

macro_rules! require_libx265 {
    () => {
        require_ffmpeg!();
        if !has_libx265() {
            eprintln!("this ffmpeg has no libx265; skipping");
            return;
        }
    };
}

/// How the server runs a job on `input`: new files go to a `converted`
/// folder next to it.
fn config(input: &Path, validation: ValidationLevel) -> RunConfig {
    RunConfig {
        ffmpeg: PathBuf::from("ffmpeg"),
        ffprobe: PathBuf::from("ffprobe"),
        temp_dir: None,
        validation,
        output_mode: OutputMode::Folder,
        output_folder: Some(input.with_file_name("converted")),
        keep_file_dates: true,
        low_priority: false,
    }
}

/// The whole job, as the server runs it, with the original in `dir`.
async fn convert(input: &Path, probe: ProbeInfo, validation: ValidationLevel) -> JobOutcome {
    convert_with(&config(input, validation), input, probe).await
}

/// [`convert`], run with `cfg`.
async fn convert_with(cfg: &RunConfig, input: &Path, probe: ProbeInfo) -> JobOutcome {
    let spec = JobSpec {
        job_id: Uuid::new_v4(),
        file_id: Uuid::new_v4(),
        input: input.to_path_buf(),
        library_root: input.parent().expect("folder").to_path_buf(),
        probe,
        profile: balanced(),
        candidates: vec![libx265()],
        force: false,
        mounts: Vec::new(),
    };
    let (tx, mut rx) = mpsc::channel(4096);
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let outcome = run_job(cfg, &spec, tx, CancellationToken::new()).await;
    drain.await.ok();
    outcome
}

/// A finished job whose thorough checks all passed on the first attempt,
/// comparing the clip's length on both sides; the new file.
fn assert_converted_and_verified(name: &str, outcome: JobOutcome) -> PathBuf {
    let JobOutcome::Done {
        output_path,
        attempt,
        validation,
        ..
    } = outcome
    else {
        panic!("{name}: expected the job to finish, got {outcome:?}");
    };
    assert_eq!(attempt, 1, "{name}");
    let report = validation.expect("verified");
    eprintln!("{name}:\n{}", describe(&report));
    assert!(report.passed, "{name}:\n{}", describe(&report));
    assert_eq!(report.level, ValidationLevel::Thorough);
    for c in &report.checks {
        assert!(
            matches!(c.status, CheckStatus::Pass | CheckStatus::Warn) || c.id == "size",
            "{name}:\n{}",
            describe(&report)
        );
        assert_ne!(
            c.status,
            CheckStatus::Skipped,
            "{name}:\n{}",
            describe(&report)
        );
    }
    let length = check(&report, "duration");
    assert!(
        length.detail.starts_with("Matches the original (6.") && length.detail.ends_with(" s)"),
        "{name}:\n{}",
        describe(&report)
    );
    let decode = check(&report, "decode");
    assert!(
        decode.detail.starts_with("Decoded all 6."),
        "{name}:\n{}",
        describe(&report)
    );
    for id in ["visual", "black_frames", "frozen_frames"] {
        assert_eq!(check(&report, id).status, CheckStatus::Pass, "{name}: {id}");
    }
    output_path
}

/// The plan for a hand-made attempt.
fn plan_args(source: &Path, probe: &ProbeInfo, output: &Path) -> Vec<String> {
    build_plan(&PlanRequest {
        input: source,
        output,
        probe,
        profile: &balanced(),
        encoder: &libx265(),
    })
    .expect("plan")
    .args
}

async fn verify(source: &Path, probe: &ProbeInfo, output: &Path) -> ValidationReport {
    let profile = balanced();
    let req = ValidateRequest {
        ffmpeg: Path::new("ffmpeg"),
        ffprobe: Path::new("ffprobe"),
        source,
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

/// Conversions of `source` cut short fail: one that stopped half way (and
/// says so), and a whole one with its second half lost on disk (which
/// still claims the whole length).
async fn assert_cut_short_conversions_fail(source: &Path, probe: &ProbeInfo) {
    let dir = source.parent().expect("folder");
    let short = dir.join("short.mkv");
    let mut args = plan_args(source, probe, &short);
    let at = args.iter().rposition(|a| a == "-f").expect("-f");
    args.splice(at..at, ["-t".to_string(), "3".to_string()]);
    let ran = Command::new("ffmpeg").args(&args).output().expect("ffmpeg");
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let report = verify(source, probe, &short).await;
    assert!(!report.passed, "{}", describe(&report));
    let duration = check(&report, "duration");
    assert_eq!(duration.status, CheckStatus::Fail, "{}", describe(&report));
    assert!(
        duration
            .detail
            .starts_with("The new file is shorter than the original (3.")
            && duration.detail.contains(" s instead of 6."),
        "{}",
        describe(&report)
    );

    let whole = dir.join("whole.mkv");
    let args = plan_args(source, probe, &whole);
    let ran = Command::new("ffmpeg").args(&args).output().expect("ffmpeg");
    assert!(
        ran.status.success(),
        "{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let cut = dir.join("cut.mkv");
    let bytes = std::fs::read(&whole).expect("read");
    std::fs::write(&cut, &bytes[..bytes.len() / 2]).expect("write");
    let report = verify(source, probe, &cut).await;
    assert!(!report.passed, "{}", describe(&report));
    let failed: Vec<&str> = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .map(|c| c.id.as_str())
        .collect();
    assert!(
        failed == ["duration"] || failed == ["decode"],
        "{}",
        describe(&report)
    );
    for file in [short, whole, cut] {
        std::fs::remove_file(file).ok();
    }
}

/// The live-stream clip: the scanner lists it as the 6 s it plays (not
/// the film's 2:21:02), it converts and verifies, with checks on or off,
/// and is never called damaged. A conversion of it cut short still fails.
#[tokio::test]
async fn a_live_stream_mkv_with_only_the_films_tags() {
    require_libx265!();
    let dir = tempfile::tempdir().unwrap();
    let clip = clip(dir.path());
    let source = live(dir.path(), &clip);
    std::fs::remove_file(&clip).ok();

    let json = ffprobe_json(&source);
    assert_eq!(
        format_field(&json, "duration"),
        None,
        "no length of its own"
    );
    let probe = scan(&source).await;
    let length = probe.duration_secs.unwrap_or_default();
    assert!((length - CLIP).abs() < 0.2, "listed as {length} s");
    assert!(
        probe.streams[0]
            .statistics_tags
            .iter()
            .any(|k| k == "DURATION-eng")
    );

    let output = assert_converted_and_verified(
        "live, thorough",
        convert(&source, probe.clone(), ValidationLevel::Thorough).await,
    );
    // The new file has a length of its own and none of the film's tags.
    let out = ffprobe_json(&output);
    let stated = format_field(&out, "duration").unwrap_or_default();
    assert!((stated - CLIP).abs() < 0.5, "{out}");
    assert!(!out.to_string().contains(FILM), "{out}");
    std::fs::remove_file(&output).ok();

    // Unverified, a result shorter than half the claimed length used to
    // be enough to call the original cut off. Even probed as an older
    // version stored it (the film's length), it isn't called damaged: its
    // length is read again first.
    let stale = ProbeInfo {
        duration_secs: Some(FILM_SECS),
        ..probe.clone()
    };
    for probe in [probe.clone(), stale] {
        match convert(&source, probe, ValidationLevel::Off).await {
            JobOutcome::Done { output_path, .. } => std::fs::remove_file(output_path).ok(),
            other => panic!("expected Done, got {other:?}"),
        };
    }

    assert_cut_short_conversions_fail(&source, &probe).await;
}

/// The same live-stream clip really cut in half on disk: ffprobe finds it
/// stops in the middle of a packet, so its packets don't tell its length
/// and the length it states stands. The job fails as before, with the
/// original reported as damaged and left as it was.
#[tokio::test]
async fn the_live_stream_mkv_really_cut_short_is_still_reported() {
    require_libx265!();
    let dir = tempfile::tempdir().unwrap();
    let clip = clip(dir.path());
    let whole = live(dir.path(), &clip);
    let source = dir.path().join("Cut (2017).mkv");
    let bytes = std::fs::read(&whole).expect("read");
    std::fs::write(&source, &bytes[..bytes.len() / 2]).expect("write");
    std::fs::remove_file(&whole).ok();
    std::fs::remove_file(&clip).ok();
    let before = std::fs::read(&source).unwrap();

    let probe = scan(&source).await;
    assert_eq!(probe.duration_secs, Some(FILM_SECS), "as it states");
    for validation in [ValidationLevel::Thorough, ValidationLevel::Off] {
        match convert(&source, probe.clone(), validation).await {
            JobOutcome::Failed { error, problem, .. } => {
                assert_eq!(problem, ProblemKind::UnreadableSource, "{validation:?}");
                assert!(
                    error.starts_with(
                        "The original file appears damaged or incomplete (it stops after"
                    ),
                    "{validation:?}: {error}"
                );
                assert!(error.ends_with("It was left unchanged."), "{error}");
            }
            other => panic!("{validation:?}: expected Failed, got {other:?}"),
        }
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }
}

/// Timestamps from 10:00 and only the film's tags, through a pipe. The
/// scanner lists it as 6 s, the job passes, and a conversion cut short
/// still fails.
#[tokio::test]
async fn a_clip_starting_at_ten_minutes_with_only_the_films_tags() {
    require_libx265!();
    let dir = tempfile::tempdir().unwrap();
    let clip = clip(dir.path());
    let source = shifted_through_a_pipe(dir.path(), &clip);
    std::fs::remove_file(&clip).ok();

    let json = ffprobe_json(&source);
    assert_eq!(format_field(&json, "start_time"), Some(600.0), "{json}");
    // ffmpeg 7 states the clip's length here, ffmpeg 6 none.
    if let Some(stated) = format_field(&json, "duration") {
        assert!((stated - CLIP).abs() < 0.5, "{json}");
    }
    let probe = scan(&source).await;
    let length = probe.duration_secs.unwrap_or_default();
    assert!((length - CLIP).abs() < 0.2, "listed as {length} s");

    assert_converted_and_verified(
        "shifted, film's tags",
        convert(&source, probe.clone(), ValidationLevel::Thorough).await,
    );
    assert_cut_short_conversions_fail(&source, &probe).await;
}

/// Timestamps from 10:00 and fresh tags: the container and every
/// `DURATION` state 10:06, where the clip ends. Before, that was compared
/// with the conversion's 6 s and failed (on every version). The film's
/// statistics in the file's own tags don't follow into the new MKV; its
/// title does.
#[tokio::test]
async fn a_clip_starting_at_ten_minutes_with_fresh_tags() {
    require_libx265!();
    let dir = tempfile::tempdir().unwrap();
    let clip = clip(dir.path());
    let source = shifted_with_fresh_tags(dir.path(), &clip);
    std::fs::remove_file(&clip).ok();

    let json = ffprobe_json(&source);
    let stated = format_field(&json, "duration").unwrap_or_default();
    assert!((stated - 606.0).abs() < 0.5, "states its end: {json}");
    let probe = scan(&source).await;
    let length = probe.duration_secs.unwrap_or_default();
    assert!((length - CLIP).abs() < 0.2, "listed as {length} s");
    assert_eq!(probe.statistics_tags, ["BPS-eng", "DURATION-eng"]);

    let output = assert_converted_and_verified(
        "shifted, fresh tags",
        convert(&source, probe.clone(), ValidationLevel::Thorough).await,
    );
    let out = ffprobe_json(&output);
    let tags = out["format"]["tags"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        tags.get("title").and_then(Value::as_str),
        Some("The Film"),
        "{out}"
    );
    assert!(
        !tags
            .keys()
            .any(|k| szalinski_core::tags::is_statistics_tag(k)),
        "{out}"
    );
    std::fs::remove_file(&output).ok();

    assert_cut_short_conversions_fail(&source, &probe).await;
}

/// An MP4 whose metadata carries the film's `DURATION-eng` (kept with
/// `use_metadata_tags`): the scanner lists it for removal and the new MKV
/// doesn't carry it; the title stays.
#[tokio::test]
async fn statistics_in_an_mp4s_metadata_are_not_carried_into_the_mkv() {
    require_libx265!();
    let dir = tempfile::tempdir().unwrap();
    let clip = clip(dir.path());
    let source = dir.path().join("Tagged (2017).mp4");
    support::ffmpeg(&[
        "-i",
        s(&clip),
        "-map",
        "0:v",
        "-map",
        "0:a",
        "-c",
        "copy",
        "-metadata",
        "title=The Film",
        "-metadata",
        &format!("DURATION-eng={FILM}"),
        "-movflags",
        "use_metadata_tags",
        s(&source),
    ]);
    std::fs::remove_file(&clip).ok();
    let probe = scan(&source).await;
    assert_eq!(probe.statistics_tags, ["DURATION-eng"]);
    let output = assert_converted_and_verified(
        "mp4 with the film's metadata",
        convert(&source, probe, ValidationLevel::Thorough).await,
    );
    let out = ffprobe_json(&output);
    let tags = out["format"]["tags"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        tags.get("title").and_then(Value::as_str),
        Some("The Film"),
        "{out}"
    );
    assert!(!tags.contains_key("DURATION-eng"), "{out}");
    assert!(!out.to_string().contains(FILM), "{out}");
}

// ---------------------------------------------------------------------------
// Originals that state no length at all.

/// The tester's original: 61 s, written so it states no length.
const UNSTATED_SECS: u32 = 61;

/// [`config`] replacing the original, as the tester's library did.
fn replacing(input: &Path, validation: ValidationLevel) -> RunConfig {
    RunConfig {
        output_mode: OutputMode::Replace,
        output_folder: None,
        ..config(input, validation)
    }
}

/// A job that the checks failed: its problem, its error and its checks.
fn failed(outcome: JobOutcome) -> (ProblemKind, String, ValidationReport) {
    match outcome {
        JobOutcome::Failed {
            problem,
            error,
            validation: Some(report),
            ..
        } => {
            eprintln!("{error}\n{}", describe(&report));
            (problem, error, report)
        }
        other => panic!("expected the checks to fail the job, got {other:?}"),
    }
}

/// A finished job that replaced the original after thorough checks that
/// all ran (none skipped), comparing the 1:01 on both sides.
fn assert_replaced_after_every_check(name: &str, source: &Path, outcome: JobOutcome) {
    let JobOutcome::Done {
        output_path,
        validation,
        ..
    } = outcome
    else {
        panic!("{name}: expected the job to finish, got {outcome:?}");
    };
    let report = validation.expect("verified");
    eprintln!("{name}:\n{}", describe(&report));
    assert!(report.passed, "{name}:\n{}", describe(&report));
    assert_eq!(report.level, ValidationLevel::Thorough);
    for c in &report.checks {
        assert_ne!(
            c.status,
            CheckStatus::Skipped,
            "{name}:\n{}",
            describe(&report)
        );
    }
    assert_eq!(
        check(&report, "duration").detail,
        "Matches the original (1:01 vs 1:01)",
        "{name}"
    );
    assert_eq!(
        check(&report, "decode").detail,
        "Decoded all 1:01 without errors",
        "{name}"
    );
    let visual = check(&report, "visual");
    assert_eq!(visual.status, CheckStatus::Pass, "{name}");
    assert!(
        visual
            .detail
            .starts_with("Matches the original at 10 points"),
        "{name}: {}",
        visual.detail
    );
    for id in ["black_frames", "frozen_frames"] {
        assert_eq!(check(&report, id).status, CheckStatus::Pass, "{name}: {id}");
    }
    assert_eq!(output_path, source.with_extension("mkv"), "{name}");
    let stated = format_field(&ffprobe_json(&output_path), "duration").unwrap_or_default();
    assert!(
        (stated - 61.0).abs() < 0.5,
        "{name}: the new file states {stated}"
    );
}

/// The tester's report: a Matroska file written as a live stream without
/// tags (`-live 1`; through a pipe it is the same) states no length at
/// all. Before, the scanner listed none, the checks said "Same length as
/// the original [skipped]" and "Looks like the original [skipped]: The
/// file is too short to compare pictures", and a conversion cut to 30 s
/// passed and replaced the 61 s original. Its packets are listed now (all
/// of them: there is no end to read back from). The cut conversion fails
/// and the original is kept byte for byte, whether the server stored its
/// length as ffprobe states it (none) or measured; a complete conversion
/// replaces it after thorough checks that all ran. The same holds for its
/// picture as a raw H.264 stream, a file that states no length in any
/// container.
#[cfg(unix)]
#[tokio::test]
async fn a_live_stream_mkv_with_no_length_at_all() {
    require_libx265!();
    let dir = tempfile::tempdir().unwrap();
    let cut_at_30 = support::cutting_ffmpeg(dir.path(), 30);
    let live = support::small_clip(
        dir.path(),
        "Live (2017).mkv",
        UNSTATED_SECS,
        &["-live", "1"],
    );
    let raw = dir.path().join("Raw (2017).h264");
    support::ffmpeg(&["-i", s(&live), "-map", "0:v", "-c", "copy", s(&raw)]);

    for source in [live, raw] {
        let name = source.file_name().unwrap().to_string_lossy().into_owned();
        let json = ffprobe_json(&source);
        assert_eq!(format_field(&json, "duration"), None, "{name}: {json}");
        assert!(!json.to_string().contains("DURATION"), "{name}: {json}");
        let stated = support::probe(&source);
        assert_eq!(stated.duration_secs, None, "{name}: as ffprobe states it");
        let scanned = scan(&source).await;
        let length = scanned.duration_secs.unwrap_or_default();
        assert!((length - 61.0).abs() < 0.2, "{name}: listed as {length} s");

        let before = std::fs::read(&source).unwrap();
        for probe in [stated.clone(), scanned] {
            let cfg = RunConfig {
                ffmpeg: cut_at_30.clone(),
                ..replacing(&source, ValidationLevel::Thorough)
            };
            let (problem, error, report) = failed(convert_with(&cfg, &source, probe).await);
            assert_eq!(problem, ProblemKind::Verification, "{name}");
            assert_eq!(
                error,
                "The new file is shorter than the original (30.0 s instead of 1:01). The \
                 original was kept. Try again.",
                "{name}"
            );
            assert_eq!(
                report.first_failure().map(|c| c.id.as_str()),
                Some("duration"),
                "{name}"
            );
            assert_eq!(std::fs::read(&source).unwrap(), before, "{name}: kept");
            assert!(support::artifacts_in(dir.path()).is_empty(), "{name}");
        }

        let cfg = replacing(&source, ValidationLevel::Thorough);
        let outcome = convert_with(&cfg, &source, stated).await;
        assert_replaced_after_every_check(&name, &source, outcome);
    }
}

/// The same kind of original when its packets can't be listed (ffprobe
/// fails on them): its length can't be read, so nothing could tell a
/// complete conversion from one cut short. The length check fails at
/// every level that runs it (Quick too) instead of being skipped, and the
/// original is kept byte for byte. The advice is about the original, not
/// lighter checks, which fail the same way.
#[cfg(unix)]
#[tokio::test]
async fn an_original_with_no_length_whose_packets_cant_be_listed_is_kept() {
    require_libx265!();
    let dir = tempfile::tempdir().unwrap();
    let ffprobe = support::unlisting_ffprobe(dir.path());
    let cut_at_3 = support::cutting_ffmpeg(dir.path(), 3);
    let source = support::small_clip(dir.path(), "Live (2017).mkv", 6, &["-live", "1"]);
    // The scanner can't measure it either.
    let probe = szalinski_scanner::probe_file(&ffprobe, &source, Duration::from_secs(60))
        .await
        .expect("probe");
    assert_eq!(probe.duration_secs, None);

    let before = std::fs::read(&source).unwrap();
    for (validation, ffmpeg) in [
        (ValidationLevel::Quick, PathBuf::from("ffmpeg")),
        (ValidationLevel::Thorough, PathBuf::from("ffmpeg")),
        (ValidationLevel::Thorough, cut_at_3.clone()),
    ] {
        let cfg = RunConfig {
            ffmpeg,
            ffprobe: ffprobe.clone(),
            ..replacing(&source, validation)
        };
        let (problem, error, report) = failed(convert_with(&cfg, &source, probe.clone()).await);
        assert_eq!(problem, ProblemKind::Verification, "{validation:?}");
        assert_eq!(
            error,
            "The original's length couldn't be read (it states none that can be trusted, and \
             its contents couldn't be listed), so the new file couldn't be checked against it. \
             The original was kept. Try again; if it happens again, check that the original \
             plays to the end, or replace it with a good copy.",
            "{validation:?}"
        );
        assert_eq!(check(&report, "duration").status, CheckStatus::Fail);
        assert_eq!(std::fs::read(&source).unwrap(), before, "{validation:?}");
        assert!(support::artifacts_in(dir.path()).is_empty());
    }
}
