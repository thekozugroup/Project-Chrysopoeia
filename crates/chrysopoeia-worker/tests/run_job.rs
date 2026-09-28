//! `run_job_with` end to end on synthetic media, with a simple fake planner
//! (libx264 ultrafast + AAC) standing in for `plan::build_plan`.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrysopoeia_core::{
    CheckStatus, Container, EncoderCandidate, Goal, HwApi, JobProgress, JobStage, OutputMode,
    ProbeInfo, TranscodeProfile, ValidationLevel, VideoCodec,
};
use chrysopoeia_worker::run::{JobOutcome, JobSpec, RunConfig, run_job_with};
use chrysopoeia_worker::{Decision, FfmpegPlan, PlanRequest};
use filetime::FileTime;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Name of a fake hardware encoder whose command always fails.
const BROKEN_HW: &str = "h264_broken_hw";
/// Name of a fake hardware encoder that "succeeds" with the wrong picture.
const CORRUPT_HW: &str = "h264_corrupt_hw";

fn config(validation: ValidationLevel) -> RunConfig {
    RunConfig {
        ffmpeg: PathBuf::from("ffmpeg"),
        ffprobe: PathBuf::from("ffprobe"),
        temp_dir: None,
        validation,
        output_mode: OutputMode::Replace,
        output_folder: None,
        keep_file_dates: true,
        low_priority: true,
    }
}

fn candidate(name: &str, api: HwApi, hw_decode: bool) -> EncoderCandidate {
    EncoderCandidate {
        name: name.into(),
        codec: VideoCodec::H264,
        api,
        device: None,
        hw_decode,
    }
}

fn software() -> EncoderCandidate {
    candidate("libx264", HwApi::Software, false)
}

fn profile() -> TranscodeProfile {
    TranscodeProfile {
        video_codec: VideoCodec::H264,
        container: Container::Mkv,
        min_savings_pct: None,
        ..TranscodeProfile::from_goal(Goal::Compatible)
    }
}

fn spec(input: &Path, library_root: &Path, profile: TranscodeProfile) -> JobSpec {
    JobSpec {
        job_id: Uuid::new_v4(),
        file_id: Uuid::new_v4(),
        input: input.to_path_buf(),
        library_root: library_root.to_path_buf(),
        probe: support::probe(input),
        profile,
        candidates: vec![software()],
    }
}

/// The fake planner: libx264 ultrafast + AAC into Matroska, keeping video,
/// audio and subtitles. The fake hardware encoders misbehave on purpose.
fn fake_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let input = req.input.to_string_lossy().into_owned();
    let output = req.output.to_string_lossy().into_owned();
    let mut args: Vec<String> = vec!["-y".into()];
    if req.encoder.hw_decode {
        // Differs from the CPU-decode retry so the retry is not deduplicated.
        args.extend(["-threads".into(), "0".into()]);
    }
    args.extend(["-i".into(), input]);
    let mut video_map = "0:v:0".to_string();
    if req.encoder.name == CORRUPT_HW {
        // Wrong picture, right shape: what a broken GPU encoder can produce.
        let (w, h) = req
            .probe
            .primary_video()
            .and_then(|v| Some((v.width?, v.height?)))
            .unwrap_or((1280, 720));
        let secs = req.probe.duration_secs.unwrap_or(4.0);
        args.extend([
            "-f".into(),
            "lavfi".into(),
            "-i".into(),
            format!("smptebars=size={w}x{h}:rate=24:duration={secs}"),
        ]);
        video_map = "1:v:0".into();
    }
    args.extend([
        "-map".into(),
        video_map,
        "-map".into(),
        "0:a?".into(),
        "-map".into(),
        "0:s?".into(),
        "-c:v".into(),
        if req.encoder.name == BROKEN_HW {
            "chrysopoeia_no_such_encoder".into()
        } else {
            "libx264".into()
        },
        "-preset".into(),
        "ultrafast".into(),
        "-crf".into(),
        "20".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-c:a".into(),
        "aac".into(),
        "-c:s".into(),
        "copy".into(),
        "-progress".into(),
        "pipe:1".into(),
        "-nostats".into(),
        "-f".into(),
        "matroska".into(),
        output,
    ]);
    Ok(FfmpegPlan {
        args,
        notes: vec!["Test plan".into()],
        expected: support::counts(req.probe),
    })
}

/// Like [`fake_plan`], but reads the input at its native speed so a job
/// takes as long as the clip.
fn realtime_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let mut plan = fake_plan(req)?;
    let at = plan.args.iter().position(|a| a == "-i").unwrap_or(0);
    plan.args.insert(at, "-re".into());
    Ok(plan)
}

fn transcode(_: &ProbeInfo, _: &TranscodeProfile) -> Decision {
    Decision::Transcode
}

async fn run(
    cfg: &RunConfig,
    spec: &JobSpec,
    planner: &chrysopoeia_worker::run::PlanFn,
) -> (JobOutcome, Vec<JobProgress>) {
    let (tx, mut rx) = mpsc::channel(1024);
    let outcome = run_job_with(cfg, spec, planner, &transcode, tx, CancellationToken::new()).await;
    let mut updates = Vec::new();
    while let Some(p) = rx.recv().await {
        updates.push(p);
    }
    (outcome, updates)
}

fn set_old_mtime(path: &Path) -> FileTime {
    let t = FileTime::from_unix_time(1_000_000_000, 0);
    filetime::set_file_times(path, t, t).unwrap();
    t
}

fn mtime(path: &Path) -> FileTime {
    FileTime::from_last_modification_time(&std::fs::metadata(path).unwrap())
}

#[tokio::test]
async fn success_replaces_the_original_and_keeps_its_date() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MKV_1080P_SUBS, dir.path());
    let original = std::fs::read(&input).unwrap();
    let old = set_old_mtime(&input);
    let spec = spec(&input, dir.path(), profile());
    let cfg = config(ValidationLevel::Standard);

    let (outcome, updates) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Done {
        output_path,
        output_size,
        original_size,
        encoder,
        hw_api,
        attempt,
        validation,
        command,
        notes,
    } = outcome
    else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!(output_path, input, "same container: replaced in place");
    assert_eq!(original_size, original.len() as u64);
    assert_eq!(output_size, std::fs::metadata(&input).unwrap().len());
    assert_ne!(
        std::fs::read(&input).unwrap(),
        original,
        "file was re-encoded"
    );
    assert_eq!(mtime(&input), old);
    assert_eq!(
        (encoder.as_str(), hw_api, attempt),
        ("libx264", HwApi::Software, 1)
    );
    assert!(
        command.starts_with("ffmpeg -hide_banner -nostdin -y -i "),
        "{command}"
    );
    assert!(command.contains("'Show - S01E01.mkv'") || command.contains("Show - S01E01"));
    assert_eq!(notes, ["Test plan"]);
    let report = validation.expect("verified");
    assert!(report.passed);
    assert!(report.checks.iter().all(|c| c.status != CheckStatus::Fail));
    assert!(support::artifacts_in(dir.path()).is_empty());

    // The new file really is what the plan asked for.
    let probe = support::probe(&input);
    assert_eq!(probe.video_codec(), Some("h264"));
    assert_eq!(support::counts(&probe), support::counts(&spec.probe));

    // Every stage reported, each ending at 100 %.
    for stage in [
        JobStage::Preparing,
        JobStage::Transcoding,
        JobStage::Verifying,
        JobStage::Finalizing,
    ] {
        let last = updates
            .iter()
            .rfind(|p| p.stage == stage)
            .unwrap_or_else(|| panic!("no {stage:?} update"));
        assert_eq!(last.progress, 100.0, "{stage:?}");
    }
    assert!(updates.iter().all(|p| p.job_id == spec.job_id));
    let transcoding: Vec<_> = updates
        .iter()
        .filter(|p| p.stage == JobStage::Transcoding)
        .collect();
    assert!(
        transcoding
            .iter()
            .all(|p| p.encoder.as_deref() == Some("libx264") && p.attempt == 1)
    );
}

#[tokio::test]
async fn extension_change_writes_the_new_name_and_removes_the_old() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let spec = spec(&input, dir.path(), profile());
    let cfg = config(ValidationLevel::Quick);

    let (outcome, _) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Done { output_path, .. } = outcome else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!(output_path, input.with_extension("mkv"));
    assert!(output_path.exists());
    assert!(!input.exists());
    assert_eq!(support::walk(dir.path()), [output_path]);
}

#[tokio::test]
async fn folder_mode_leaves_the_original_in_place() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    let season = library.join("TV/Show");
    std::fs::create_dir_all(&season).unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, &season);
    let original = std::fs::read(&input).unwrap();
    let spec = spec(&input, &library, profile());
    let mut cfg = config(ValidationLevel::Quick);
    cfg.output_mode = OutputMode::Folder;
    cfg.output_folder = Some(dir.path().join("converted"));
    cfg.temp_dir = Some(dir.path().join("scratch"));

    let (outcome, _) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Done { output_path, .. } = outcome else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!(
        output_path,
        dir.path().join("converted/TV/Show/Big Test (2020).mkv")
    );
    assert!(output_path.exists());
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(support::artifacts_in(dir.path()).is_empty());
}

#[tokio::test]
async fn size_rule_skips_and_keeps_the_original() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MKV_1080P_SUBS, dir.path());
    let original = std::fs::read(&input).unwrap();
    let old = set_old_mtime(&input);
    let profile = TranscodeProfile {
        min_savings_pct: Some(90),
        ..profile()
    };
    let spec = spec(&input, dir.path(), profile);
    let cfg = config(ValidationLevel::Standard);

    let (outcome, updates) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Skipped {
        reason,
        encoder,
        output_size,
    } = outcome
    else {
        panic!("expected Skipped, got {outcome:?}");
    };
    assert!(reason.ends_with("— kept the original"), "{reason}");
    assert_eq!(encoder.as_deref(), Some("libx264"));
    assert!(output_size.is_some_and(|s| s > 0));
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert_eq!(mtime(&input), old);
    assert!(support::artifacts_in(dir.path()).is_empty());
    assert!(
        updates.iter().all(|p| p.stage != JobStage::Verifying),
        "size is checked before verification"
    );
}

#[tokio::test]
async fn failing_first_candidate_falls_back_to_the_next() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let mut spec = spec(&input, dir.path(), profile());
    // GPU decode + broken encoder, then the same with CPU decode, then CPU.
    spec.candidates = vec![candidate(BROKEN_HW, HwApi::Nvenc, true), software()];
    let cfg = config(ValidationLevel::Quick);

    let (outcome, updates) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Done {
        encoder,
        attempt,
        notes,
        ..
    } = outcome
    else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!(encoder, "libx264");
    assert_eq!(attempt, 3);
    assert!(
        notes
            .iter()
            .any(|n| n.contains("h264_broken_hw (NVIDIA NVENC) didn't work")),
        "{notes:?}"
    );
    let attempts: Vec<_> = updates
        .iter()
        .filter(|p| p.stage == JobStage::Transcoding && p.progress == 0.0)
        .map(|p| (p.encoder.clone().unwrap_or_default(), p.attempt))
        .collect();
    assert_eq!(
        attempts,
        [
            (BROKEN_HW.to_string(), 1),
            (BROKEN_HW.to_string(), 2),
            ("libx264".to_string(), 3)
        ]
    );
    assert!(support::artifacts_in(dir.path()).is_empty());
}

#[tokio::test]
async fn all_candidates_failing_reports_the_last_error() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let original = std::fs::read(&input).unwrap();
    let mut spec = spec(&input, dir.path(), profile());
    spec.candidates = vec![candidate(BROKEN_HW, HwApi::Vaapi, false)];
    let cfg = config(ValidationLevel::Quick);

    let (outcome, _) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Failed {
        error,
        log_tail,
        command,
        encoder,
        attempt,
        validation,
    } = outcome
    else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert!(
        error.starts_with("h264_broken_hw stopped with exit code"),
        "{error}"
    );
    assert!(log_tail.is_some_and(|t| t.contains("chrysopoeia_no_such_encoder")));
    assert!(command.is_some_and(|c| c.contains("chrysopoeia_no_such_encoder")));
    assert_eq!(encoder.as_deref(), Some(BROKEN_HW));
    assert_eq!(attempt, 1);
    assert_eq!(validation, None);
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(support::artifacts_in(dir.path()).is_empty());
}

#[tokio::test]
async fn corrupt_hardware_output_falls_back_after_verification() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let mut spec = spec(&input, dir.path(), profile());
    spec.candidates = vec![candidate(CORRUPT_HW, HwApi::Qsv, false), software()];
    let cfg = config(ValidationLevel::Standard);

    let (outcome, updates) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Done {
        encoder,
        attempt,
        validation,
        ..
    } = outcome
    else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!((encoder.as_str(), attempt), ("libx264", 2));
    assert!(validation.is_some_and(|r| r.passed));
    // Verification ran twice: once failing (hardware), once passing.
    let verifying_starts = updates
        .iter()
        .filter(|p| p.stage == JobStage::Verifying && p.progress == 0.0)
        .count();
    assert_eq!(verifying_starts, 2);
    assert!(support::artifacts_in(dir.path()).is_empty());
}

#[tokio::test]
async fn verification_failure_on_the_last_attempt_fails_the_job() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let original = std::fs::read(&input).unwrap();
    let mut spec = spec(&input, dir.path(), profile());
    spec.candidates = vec![candidate(CORRUPT_HW, HwApi::Qsv, false)];
    let cfg = config(ValidationLevel::Standard);

    let (outcome, _) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Failed {
        error, validation, ..
    } = outcome
    else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert!(
        error.starts_with("Verification failed: Looks like the original — "),
        "{error}"
    );
    assert!(validation.is_some_and(|r| !r.passed));
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(support::artifacts_in(dir.path()).is_empty());
}

#[tokio::test]
async fn cancellation_mid_encode_leaves_no_temp_and_the_original_intact() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let original = std::fs::read(&input).unwrap();
    let old = set_old_mtime(&input);
    let spec = spec(&input, dir.path(), profile());
    let cfg = config(ValidationLevel::Standard);

    let (tx, mut rx) = mpsc::channel::<JobProgress>(1024);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    // Cancel once the encode is visibly under way.
    let watcher = tokio::spawn(async move {
        let mut saw_progress = false;
        while let Some(p) = rx.recv().await {
            if p.stage == JobStage::Transcoding && p.progress > 0.0 && !saw_progress {
                saw_progress = true;
                trigger.cancel();
            }
        }
        saw_progress
    });
    let started = std::time::Instant::now();
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        run_job_with(&cfg, &spec, &realtime_plan, &transcode, tx, cancel),
    )
    .await
    .expect("job finished");
    assert_eq!(outcome, JobOutcome::Cancelled);
    assert!(watcher.await.unwrap(), "cancelled mid-encode");
    assert!(
        started.elapsed() < Duration::from_secs(u64::from(support::CLIP_SECS) + 2),
        "stopped early"
    );
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert_eq!(mtime(&input), old);
    assert!(support::artifacts_in(dir.path()).is_empty());
    assert_eq!(support::walk(dir.path()), [input]);
}

#[tokio::test]
async fn cancelled_before_start_does_nothing() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let spec = spec(&input, dir.path(), profile());
    let (tx, _rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    cancel.cancel();
    let outcome = run_job_with(
        &config(ValidationLevel::Standard),
        &spec,
        &fake_plan,
        &transcode,
        tx,
        cancel,
    )
    .await;
    assert_eq!(outcome, JobOutcome::Cancelled);
    assert_eq!(support::walk(dir.path()), [input]);
}

#[tokio::test]
async fn preparing_checks_fail_fast() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let cfg = config(ValidationLevel::Quick);

    // The decider says there is nothing to do.
    let spec_ok = spec(&input, dir.path(), profile());
    let (tx, _rx) = mpsc::channel(16);
    let skip = |_: &ProbeInfo, _: &TranscodeProfile| Decision::Skip {
        reason: "Already H.264".into(),
    };
    let outcome = run_job_with(
        &cfg,
        &spec_ok,
        &fake_plan,
        &skip,
        tx,
        CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcome,
        JobOutcome::Skipped {
            reason: "Already H.264".into(),
            encoder: None,
            output_size: None
        }
    );

    // A file with the target name already exists next to the original.
    let blocker = input.with_extension("mkv");
    std::fs::write(&blocker, b"unrelated").unwrap();
    let (outcome, _) = run(&cfg, &spec_ok, &fake_plan).await;
    match outcome {
        JobOutcome::Failed { error, .. } => assert_eq!(
            error,
            "A file named \"Big Test (2020).mkv\" already exists next to the original"
        ),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(std::fs::read(&blocker).unwrap(), b"unrelated");
    std::fs::remove_file(&blocker).unwrap();

    // No encoders at all.
    let mut no_encoders = spec_ok.clone();
    no_encoders.candidates.clear();
    let (outcome, _) = run(&cfg, &no_encoders, &fake_plan).await;
    assert!(
        matches!(&outcome, JobOutcome::Failed { error, .. } if error.starts_with("No working encoder was found for H.264")),
        "{outcome:?}"
    );

    // Folder mode without a folder.
    let mut folder_cfg = cfg.clone();
    folder_cfg.output_mode = OutputMode::Folder;
    let (outcome, _) = run(&folder_cfg, &spec_ok, &fake_plan).await;
    assert!(matches!(outcome, JobOutcome::Failed { .. }), "{outcome:?}");

    // The file disappeared after it was queued.
    std::fs::remove_file(&input).unwrap();
    let (outcome, _) = run(&cfg, &spec_ok, &fake_plan).await;
    assert_eq!(
        outcome,
        JobOutcome::Failed {
            error: "The file no longer exists".into(),
            log_tail: None,
            command: None,
            encoder: None,
            attempt: 0,
            validation: None
        }
    );
    assert!(support::walk(dir.path()).is_empty());
}

#[tokio::test]
async fn planner_errors_move_on_to_the_next_candidate() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let mut spec = spec(&input, dir.path(), profile());
    spec.candidates = vec![candidate("h264_vaapi", HwApi::Vaapi, false), software()];
    let picky = |req: &PlanRequest<'_>| {
        if req.encoder.api.is_hardware() {
            anyhow::bail!("no render node configured")
        }
        fake_plan(req)
    };
    let (outcome, _) = run(&config(ValidationLevel::Quick), &spec, &picky).await;
    assert!(
        matches!(&outcome, JobOutcome::Done { encoder, attempt: 1, .. } if encoder == "libx264"),
        "{outcome:?}"
    );
}
