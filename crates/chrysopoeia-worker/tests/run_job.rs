//! `run_job_with` end to end on synthetic media, with a simple fake planner
//! (libx264 ultrafast + AAC) standing in for `plan::build_plan`.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrysopoeia_core::{
    AttemptResult, CheckStatus, Container, EncoderCandidate, Goal, HwApi, JobProgress, JobStage,
    OutputMode, ProbeInfo, ProblemKind, ProgressBasis, TranscodeProfile, ValidationLevel,
    VideoCodec,
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
        force: false,
        mounts: Vec::new(),
    }
}

/// The fake planner: libx264 ultrafast + AAC into Matroska, keeping video,
/// audio and subtitles. The fake hardware encoders misbehave on purpose.
fn fake_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let input = req.input.to_string_lossy().into_owned();
    let output = req.output.to_string_lossy().into_owned();
    let mut args: Vec<String> = vec!["-y".into()];
    if req.encoder.hw_decode {
        // Differs from the CPU-decode retry so the retry is not deduplicated,
        // and names a decoder as a real GPU-decoding plan does (`none`
        // decodes on the CPU all the same).
        args.extend(["-hwaccel".into(), "none".into()]);
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
        covers: Vec::new(),
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

/// Like [`fake_plan`], but reads the input at a tenth of its native speed,
/// so a job takes ten times as long as the clip.
fn slow_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let mut plan = fake_plan(req)?;
    let at = plan.args.iter().position(|a| a == "-i").unwrap_or(0);
    plan.args
        .splice(at..at, ["-readrate".to_string(), "0.1".to_string()]);
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
        attempts,
    } = outcome
    else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!(output_path, input, "same container: replaced in place");
    // One attempt, which worked, with the command that made the file.
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    let only = &attempts[0];
    assert_eq!(
        (
            only.attempt,
            only.encoder.as_str(),
            only.hw_api,
            only.hw_decode
        ),
        (1, "libx264", HwApi::Software, false)
    );
    assert_eq!(only.result, AttemptResult::Succeeded);
    assert_eq!(only.command.as_deref(), Some(command.as_str()));
    assert_eq!(
        (&only.error, &only.failed_check, &only.log_tail),
        (&None, &None, &None)
    );
    assert!(only.elapsed_secs > 0.0);
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
        command.starts_with("ffmpeg -loglevel level+warning -hide_banner -nostdin -y -i "),
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
    // While ffmpeg encodes, each update says how its share was worked out,
    // with the frames so far and the time spent; the other stages don't.
    assert!(
        transcoding
            .iter()
            .any(|p| p.progress_basis.is_some() && p.frames.is_some() && p.elapsed_secs.is_some()),
        "{transcoding:?}"
    );
    assert!(
        updates
            .iter()
            .filter(|p| p.stage != JobStage::Transcoding)
            .all(|p| p.progress_basis.is_none() && p.frames.is_none()),
        "{updates:?}"
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
        attempts,
    } = outcome
    else {
        panic!("expected Skipped, got {outcome:?}");
    };
    assert!(reason.ends_with("— kept the original"), "{reason}");
    // The attempt worked; its file just wasn't small enough.
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    assert_eq!(attempts[0].result, AttemptResult::Succeeded);
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

/// "Convert anyway": neither the decider's skip nor the size rule stops a
/// forced job, and the result is still verified before it replaces anything.
#[tokio::test]
async fn forced_jobs_ignore_the_size_rule_and_efficiency_skips() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MKV_1080P_SUBS, dir.path());
    let profile = TranscodeProfile {
        min_savings_pct: Some(90),
        ..profile()
    };
    let mut spec = spec(&input, dir.path(), profile);
    spec.force = true;
    let cfg = config(ValidationLevel::Quick);
    let already = |_: &ProbeInfo, _: &TranscodeProfile| Decision::Skip {
        reason: "Already H.264".into(),
    };
    let (tx, mut rx) = mpsc::channel(1024);
    let outcome = run_job_with(
        &cfg,
        &spec,
        &fake_plan,
        &already,
        tx,
        CancellationToken::new(),
    )
    .await;
    let mut verified = false;
    while let Some(p) = rx.recv().await {
        verified |= p.stage == JobStage::Verifying;
    }
    let JobOutcome::Done { validation, .. } = outcome else {
        panic!("expected Done, got {outcome:?}");
    };
    assert!(verified, "a forced result is still verified");
    assert!(validation.is_some_and(|r| r.passed));
    assert!(support::artifacts_in(dir.path()).is_empty());
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
        command,
        attempts: history,
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
            .any(|n| n == "Converting on the NVIDIA GPU didn't work for this file, so it was converted on the CPU"),
        "{notes:?}"
    );
    // Every attempt is kept, with what each one was and why it failed: the
    // GPU decoding and encoding, the GPU encoding the CPU's frames, then
    // the CPU.
    let tried: Vec<_> = history
        .iter()
        .map(|a| {
            (
                a.attempt,
                a.encoder.as_str(),
                a.hw_api,
                a.hw_decode,
                a.result,
            )
        })
        .collect();
    assert_eq!(
        tried,
        [
            (1, BROKEN_HW, HwApi::Nvenc, true, AttemptResult::Failed),
            (2, BROKEN_HW, HwApi::Nvenc, false, AttemptResult::Failed),
            (
                3,
                "libx264",
                HwApi::Software,
                false,
                AttemptResult::Succeeded
            ),
        ]
    );
    for failed in &history[..2] {
        let error = failed.error.as_deref().unwrap_or_default();
        assert!(
            error.starts_with("Converting on the NVIDIA GPU stopped with an error")
                && error.contains("Unknown encoder 'chrysopoeia_no_such_encoder'"),
            "{error}"
        );
        assert_eq!(failed.problem, Some(ProblemKind::Encoder));
        assert_eq!(failed.failed_check, None);
        assert!(
            failed
                .command
                .as_deref()
                .is_some_and(|c| c.contains("chrysopoeia_no_such_encoder")),
            "{failed:?}"
        );
        let tail = failed.log_tail.as_deref().unwrap_or_default();
        assert!(tail.contains("chrysopoeia_no_such_encoder"), "{tail}");
        assert!(tail.lines().count() <= 12, "{tail}");
    }
    assert_eq!(
        history[0]
            .command
            .as_deref()
            .map(|c| c.contains("-hwaccel")),
        Some(true)
    );
    assert_eq!(history[2].command.as_deref(), Some(command.as_str()));
    assert_eq!(history[2].error, None);
    // The history went out as the job ran: the update that starts each
    // later attempt carries the attempts so far (the server stores them,
    // so the job shows why the GPU wasn't used while the CPU still works).
    let sent: Vec<usize> = updates
        .iter()
        .filter_map(|p| p.attempts.as_ref().map(Vec::len))
        .collect();
    assert_eq!(&sent[..2], [1, 2], "{sent:?}");
    let second_start = updates
        .iter()
        .find(|p| p.attempts.as_ref().is_some_and(|a| a.len() == 2))
        .unwrap();
    assert_eq!(
        (second_start.stage, second_start.attempt),
        (JobStage::Transcoding, 3)
    );
    assert_eq!(second_start.attempts.as_deref(), Some(&history[..2]));
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
        problem,
        log_tail,
        command,
        encoder,
        attempt,
        validation,
        attempts,
    } = outcome
    else {
        panic!("expected Failed, got {outcome:?}");
    };
    // The one attempt, failed, as the job's error says.
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    assert_eq!(attempts[0].result, AttemptResult::Failed);
    assert_eq!(attempts[0].error.as_deref(), Some(error.as_str()));
    assert_eq!(attempts[0].command, command);
    // Plain words first, no encoder name.
    assert!(
        error.starts_with(
            "Converting on the GPU (VA-API) stopped with an error, so the original was left \
             unchanged."
        ),
        "{error}"
    );
    assert!(!error.contains(BROKEN_HW), "{error}");
    assert!(!error.contains("exit code"), "{error}");
    // The cause, not ffmpeg's closing remarks or encoder statistics.
    assert!(
        error.ends_with("ffmpeg said: \"Unknown encoder 'chrysopoeia_no_such_encoder'\"."),
        "{error}"
    );
    assert_eq!(problem, ProblemKind::Encoder);
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
        notes,
        attempts,
        ..
    } = outcome
    else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!((encoder.as_str(), attempt), ("libx264", 2));
    assert!(validation.is_some_and(|r| r.passed));
    // The failed check is kept with the first attempt, in plain words, and
    // the note on the fallback names it.
    let first = &attempts[0];
    assert_eq!(
        (first.encoder.as_str(), first.result),
        (CORRUPT_HW, AttemptResult::Failed)
    );
    assert_eq!(first.problem, Some(ProblemKind::Verification));
    let check = first.failed_check.as_ref().expect("the failed check");
    assert_eq!(
        (check.id.as_str(), check.status),
        ("visual", CheckStatus::Fail)
    );
    let error = first.error.as_deref().unwrap_or_default();
    assert!(
        error.starts_with("The new file doesn't look like the original. "),
        "{error}"
    );
    assert!(
        !error.contains("Try again"),
        "the job's advice isn't the attempt's: {error}"
    );
    assert_eq!(attempts[1].result, AttemptResult::Succeeded);
    let note = notes
        .iter()
        .find(|n| n.starts_with("Converting with Intel Quick Sync"))
        .unwrap_or_else(|| panic!("{notes:?}"));
    assert!(
        note.starts_with(&format!(
            "Converting with Intel Quick Sync made a file that failed a check ({}: ",
            check.label
        )) && note.ends_with("), so it was converted on the CPU"),
        "{note}"
    );
    assert!(!note.contains("% similar"), "{note}");
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
        error,
        validation,
        problem,
        ..
    } = outcome
    else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert_eq!(problem, ProblemKind::Verification);
    // Phrased as the failure it is, not with the check's pass-form label.
    assert!(
        error.starts_with("The new file doesn't look like the original. "),
        "{error}"
    );
    assert!(!error.contains("Looks like the original"), "{error}");
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
        let mut cancelled_at = None;
        while let Some(p) = rx.recv().await {
            if p.stage == JobStage::Transcoding && p.progress > 0.0 && cancelled_at.is_none() {
                cancelled_at = Some(std::time::Instant::now());
                trigger.cancel();
            }
        }
        cancelled_at
    });
    // The encode would take ten times the clip's length (40 s); a
    // cancelled one stops at once, so even a very slow machine stays far
    // below that.
    let outcome = tokio::time::timeout(
        Duration::from_secs(180),
        run_job_with(&cfg, &spec, &slow_plan, &transcode, tx, cancel),
    )
    .await
    .expect("job finished");
    assert_eq!(outcome, JobOutcome::Cancelled);
    let cancelled_at = watcher.await.unwrap().expect("cancelled mid-encode");
    let full_encode = Duration::from_secs(u64::from(support::CLIP_SECS) * 10);
    assert!(
        cancelled_at.elapsed() < full_encode / 2,
        "stopped {:?} after the cancel",
        cancelled_at.elapsed()
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

/// Replacing an MKV with an MP4 that can't hold its picture-based subtitles
/// or its subtitle fonts would lose them for good: the file is left as it
/// is. Converting anyway, or into a separate folder (the original stays),
/// goes ahead.
#[tokio::test]
async fn replacing_never_silently_loses_picture_subtitles_or_fonts() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MKV_1080P_SUBS, dir.path());
    let original = std::fs::read(&input).unwrap();
    let mp4 = TranscodeProfile {
        container: Container::Mp4,
        ..profile()
    };
    let mut s = spec(&input, dir.path(), mp4);
    let next = u32::try_from(s.probe.streams.len()).unwrap();
    s.probe.streams.push(chrysopoeia_core::StreamInfo {
        index: next,
        kind: Some(chrysopoeia_core::StreamKind::Subtitle),
        codec: "hdmv_pgs_subtitle".into(),
        ..Default::default()
    });
    s.probe.streams.push(chrysopoeia_core::StreamInfo {
        index: next + 1,
        kind: Some(chrysopoeia_core::StreamKind::Attachment),
        codec: "ttf".into(),
        ..Default::default()
    });
    let cfg = config(ValidationLevel::Quick);
    let (outcome, _) = run(&cfg, &s, &fake_plan).await;
    match outcome {
        JobOutcome::Skipped {
            reason,
            encoder: None,
            output_size: None,
            attempts,
        } if attempts.is_empty() => assert!(
            reason.starts_with(
                "MP4 can't hold this file's 1 picture-based subtitle and 1 subtitle font, so it \
                 was left unchanged."
            ),
            "{reason}"
        ),
        other => panic!("expected Skipped, got {other:?}"),
    }
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(support::artifacts_in(dir.path()).is_empty());

    // Past that check, planning fails here on purpose (nothing is encoded).
    let unplannable =
        |_: &PlanRequest<'_>| -> anyhow::Result<FfmpegPlan> { anyhow::bail!("not planned") };
    s.force = true;
    let (outcome, _) = run(&cfg, &s, &unplannable).await;
    assert!(matches!(outcome, JobOutcome::Failed { .. }), "{outcome:?}");
    s.force = false;
    let out = dir.path().join("converted");
    std::fs::create_dir_all(&out).unwrap();
    let folder = RunConfig {
        output_mode: OutputMode::Folder,
        output_folder: Some(out),
        ..cfg.clone()
    };
    let (outcome, _) = run(&folder, &s, &unplannable).await;
    assert!(matches!(outcome, JobOutcome::Failed { .. }), "{outcome:?}");
    assert_eq!(std::fs::read(&input).unwrap(), original);
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
            output_size: None,
            attempts: Vec::new(),
        }
    );

    // A file with the target name already exists next to the original.
    let blocker = input.with_extension("mkv");
    std::fs::write(&blocker, b"unrelated").unwrap();
    let (outcome, _) = run(&cfg, &spec_ok, &fake_plan).await;
    match outcome {
        JobOutcome::Failed { error, problem, .. } => {
            assert_eq!(
                error,
                "A file named \"Big Test (2020).mkv\" is already next to the original, so the \
                 new file can't take its name. Move or rename that file, then try again."
            );
            assert_eq!(problem, ProblemKind::Destination);
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(std::fs::read(&blocker).unwrap(), b"unrelated");
    std::fs::remove_file(&blocker).unwrap();

    // No encoders at all.
    let mut no_encoders = spec_ok.clone();
    no_encoders.candidates.clear();
    let (outcome, _) = run(&cfg, &no_encoders, &fake_plan).await;
    assert!(
        matches!(
            &outcome,
            JobOutcome::Failed { error, problem: ProblemKind::HardwareUnavailable, .. }
                if error.starts_with("Nothing on this server can make H.264 video right now")
        ),
        "{outcome:?}"
    );

    // Folder mode without a folder.
    let mut folder_cfg = cfg.clone();
    folder_cfg.output_mode = OutputMode::Folder;
    let (outcome, _) = run(&folder_cfg, &spec_ok, &fake_plan).await;
    assert!(
        matches!(
            outcome,
            JobOutcome::Failed {
                problem: ProblemKind::Destination,
                ..
            }
        ),
        "{outcome:?}"
    );

    // The file disappeared after it was queued.
    std::fs::remove_file(&input).unwrap();
    let (outcome, _) = run(&cfg, &spec_ok, &fake_plan).await;
    assert_eq!(
        outcome,
        JobOutcome::Failed {
            error: "The file is no longer there. It may have been moved or deleted. If it was \
                    moved, scan the library again to find it."
                .into(),
            problem: ProblemKind::SourceChanged,
            log_tail: None,
            command: None,
            encoder: None,
            attempt: 0,
            validation: None,
            attempts: Vec::new(),
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

/// Run `spec` with the real-time planner and call `during` once, as soon
/// as the encode is visibly under way.
async fn run_with_hook(
    cfg: &RunConfig,
    spec: &JobSpec,
    during: impl FnOnce() + Send + 'static,
) -> JobOutcome {
    let (tx, mut rx) = mpsc::channel::<JobProgress>(1024);
    let watcher = tokio::spawn(async move {
        let mut during = Some(during);
        while let Some(p) = rx.recv().await {
            if p.stage == JobStage::Transcoding
                && p.progress > 0.0
                && let Some(f) = during.take()
            {
                f();
            }
        }
        during.is_none()
    });
    let outcome = tokio::time::timeout(
        Duration::from_secs(120),
        run_job_with(
            cfg,
            spec,
            &realtime_plan,
            &transcode,
            tx,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("job finished");
    assert!(watcher.await.unwrap(), "the hook ran during the encode");
    outcome
}

#[tokio::test]
async fn an_original_replaced_during_the_encode_is_left_alone() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let spec = spec(&input, dir.path(), profile());
    let cfg = config(ValidationLevel::Standard);

    // Sonarr/Radarr upgrade: a newer release is renamed over the original
    // while it is being converted.
    let newer = support::test_media().join(support::MKV_HEVC_10BIT);
    let newer_bytes = std::fs::read(&newer).unwrap();
    let target = input.clone();
    let staging = dir.path().join("import.partial");
    let outcome = run_with_hook(&cfg, &spec, move || {
        std::fs::copy(&newer, &staging).unwrap();
        std::fs::rename(&staging, &target).unwrap();
    })
    .await;

    match outcome {
        JobOutcome::Skipped { reason, .. } => assert_eq!(
            reason,
            "The original changed while it was being converted, so it was left alone"
        ),
        other => panic!("expected Skipped, got {other:?}"),
    }
    assert_eq!(std::fs::read(&input).unwrap(), newer_bytes);
    assert_eq!(support::walk(dir.path()), [input]);
}

/// An original replaced while the new file is being checked (after the
/// last look before the checks) makes the checks compare the new file with
/// the replacement. That is "the original changed", not a failed check.
#[cfg(unix)]
#[tokio::test]
async fn an_original_replaced_during_the_checks_is_left_alone() {
    use std::os::unix::fs::PermissionsExt as _;
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let spec = spec(&input, dir.path(), profile());
    // A newer release, much shorter, waiting to be renamed over the
    // original the moment the checks first read it.
    let newer = dir.path().join("import.partial");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc=duration=1:size=320x240:rate=24",
        "-c:v",
        "libx264",
        "-f",
        "mp4",
        newer.to_str().unwrap(),
    ]);
    let newer_bytes = std::fs::read(&newer).unwrap();
    let ffprobe = dir.path().join("ffprobe-swapping");
    std::fs::write(
        &ffprobe,
        format!(
            "#!/bin/sh\nfor last; do :; done\nif [ \"$last\" = '{input}' ] && [ -f '{newer}' ]; then \
             mv '{newer}' '{input}'; fi\nexec ffprobe \"$@\"\n",
            input = input.display(),
            newer = newer.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&ffprobe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cfg = RunConfig {
        ffprobe: ffprobe.clone(),
        ..config(ValidationLevel::Standard)
    };

    let (outcome, _) = run(&cfg, &spec, &fake_plan).await;
    match outcome {
        JobOutcome::Skipped { reason, .. } => assert_eq!(
            reason,
            "The original changed while it was being converted, so it was left alone"
        ),
        other => panic!("expected Skipped, got {other:?}"),
    }
    assert!(!newer.exists(), "the swap happened during the checks");
    assert_eq!(std::fs::read(&input).unwrap(), newer_bytes);
    assert!(support::artifacts_in(dir.path()).is_empty());
}

/// The TRaSH-guides layout: the library file is a hard link of a seeding
/// torrent. Replacing it would keep the old data (through the torrent) and
/// add the new file, so it is left alone unless converted anyway; then the
/// job says no space was freed. Folder mode keeps the original anyway.
#[cfg(unix)]
#[tokio::test]
async fn a_hard_linked_original_is_not_replaced_unless_converted_anyway() {
    use chrysopoeia_worker::run::{SHARED_ORIGINAL, SHARED_ORIGINAL_NOTE};
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, &library);
    let torrent = dir.path().join("torrent.mp4");
    std::fs::hard_link(&input, &torrent).unwrap();
    let original = std::fs::read(&input).unwrap();
    let mut s = spec(&input, &library, profile());
    let cfg = config(ValidationLevel::Quick);

    let (outcome, _) = run(&cfg, &s, &fake_plan).await;
    match outcome {
        JobOutcome::Skipped {
            reason,
            encoder: None,
            ..
        } => assert_eq!(reason, SHARED_ORIGINAL),
        other => panic!("expected Skipped, got {other:?}"),
    }
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(support::artifacts_in(&library).is_empty());

    // Into a separate folder: the original stays, nothing to protect.
    let folder = RunConfig {
        output_mode: OutputMode::Folder,
        output_folder: Some(dir.path().join("converted")),
        ..cfg.clone()
    };
    let (outcome, _) = run(&folder, &s, &fake_plan).await;
    match outcome {
        JobOutcome::Done { notes, .. } => {
            assert!(
                !notes.iter().any(|n| n == SHARED_ORIGINAL_NOTE),
                "{notes:?}"
            );
        }
        other => panic!("expected Done, got {other:?}"),
    }

    // Converted anyway: replaced, and the note says nothing was freed.
    s.force = true;
    let (outcome, _) = run(&cfg, &s, &fake_plan).await;
    match outcome {
        JobOutcome::Done {
            notes, output_path, ..
        } => {
            assert!(notes.iter().any(|n| n == SHARED_ORIGINAL_NOTE), "{notes:?}");
            assert_eq!(output_path, input.with_extension("mkv"));
        }
        other => panic!("expected Done, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(&torrent).unwrap(),
        original,
        "the torrent is intact"
    );
}

#[tokio::test]
async fn folder_mode_without_a_temp_folder_encodes_into_the_output_folder() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, &library);
    let spec = spec(&input, &library, profile());
    let mut cfg = config(ValidationLevel::Quick);
    cfg.output_mode = OutputMode::Folder;
    cfg.output_folder = Some(dir.path().join("converted"));
    cfg.temp_dir = None;

    // The library may be read-only: nothing is written there, the encode
    // goes straight into the output folder.
    let (library_seen, output_seen) = (library.clone(), dir.path().join("converted"));
    let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
    let record = seen.clone();
    let outcome = run_with_hook(&cfg, &spec, move || {
        *record.lock().unwrap() = Some((
            support::artifacts_in(&library_seen),
            support::artifacts_in(&output_seen),
        ));
    })
    .await;
    let JobOutcome::Done { output_path, .. } = outcome else {
        panic!("expected Done, got {outcome:?}");
    };
    let (in_library, in_output) = seen.lock().unwrap().take().unwrap();
    assert!(in_library.is_empty(), "{in_library:?}");
    assert_eq!(in_output.len(), 1, "{in_output:?}");
    assert_eq!(
        output_path,
        dir.path().join("converted/Big Test (2020).mkv")
    );
    assert!(support::artifacts_in(dir.path()).is_empty());
    assert_eq!(support::walk(&library), [input]);
}

#[tokio::test]
async fn a_failed_folder_mode_job_leaves_no_empty_folders() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    let season = library.join("TV/Show");
    std::fs::create_dir_all(&season).unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, &season);
    let mut spec = spec(&input, &library, profile());
    spec.candidates = vec![candidate(BROKEN_HW, HwApi::Vaapi, false)];
    let mut cfg = config(ValidationLevel::Quick);
    cfg.output_mode = OutputMode::Folder;
    cfg.output_folder = Some(dir.path().join("converted"));

    let (outcome, _) = run(&cfg, &spec, &fake_plan).await;
    assert!(matches!(outcome, JobOutcome::Failed { .. }), "{outcome:?}");
    assert!(!dir.path().join("converted").exists());
    assert_eq!(support::walk(dir.path()), [input]);
}

/// A cut-off original (the truncated MKV from the test library claims the
/// full clip length but holds a fraction of a second) fails with a plain
/// explanation, with verification on and off, and is left as it was.
#[tokio::test]
async fn a_damaged_original_is_reported_as_damaged() {
    require_ffmpeg!();
    for validation in [ValidationLevel::Standard, ValidationLevel::Off] {
        let dir = tempfile::tempdir().unwrap();
        let input = support::copy_media("Broken/Truncated.mkv", dir.path());
        let before = std::fs::read(&input).unwrap();
        let spec = spec(&input, dir.path(), profile());
        let claimed = spec.probe.duration_secs.unwrap_or_default();
        assert!(claimed >= 3.0, "the sample claims {claimed} s");
        let (outcome, _) = run(&config(validation), &spec, &fake_plan).await;
        match outcome {
            JobOutcome::Failed { error, problem, .. } => {
                assert_eq!(problem, ProblemKind::UnreadableSource);
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
        assert_eq!(std::fs::read(&input).unwrap(), before);
        assert_eq!(support::walk(dir.path()), [input]);
    }
}

/// Like [`fake_plan`], but an attempt that decodes on the GPU stops after
/// half a second, as a hardware decoder that gives up early would.
fn gpu_stops_early_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let mut plan = fake_plan(req)?;
    if req.encoder.hw_decode {
        let at = plan.args.len() - 1;
        plan.args
            .splice(at..at, ["-t".to_string(), "0.5".to_string()]);
    }
    Ok(plan)
}

/// A GPU decode that stops early says nothing about the original: the job
/// moves on to CPU decoding instead of calling a good file damaged.
#[tokio::test]
async fn a_gpu_decode_that_stops_early_falls_back_instead_of_blaming_the_file() {
    require_ffmpeg!();
    for validation in [ValidationLevel::Standard, ValidationLevel::Off] {
        let dir = tempfile::tempdir().unwrap();
        let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
        let mut spec = spec(&input, dir.path(), profile());
        spec.candidates = vec![candidate("h264_qsv", HwApi::Qsv, true), software()];
        let (outcome, _) = run(&config(validation), &spec, &gpu_stops_early_plan).await;
        match outcome {
            JobOutcome::Done {
                encoder, attempt, ..
            } => {
                // The same encoder, decoding on the CPU.
                assert_eq!(
                    (encoder.as_str(), attempt),
                    ("h264_qsv", 2),
                    "{validation:?}"
                );
            }
            other => panic!("{validation:?}: expected Done, got {other:?}"),
        }
        assert!(support::artifacts_in(dir.path()).is_empty());
    }
}

/// A really cut-off original is still reported as damaged, once an attempt
/// decoding on the CPU has confirmed it.
#[tokio::test]
async fn a_damaged_original_is_confirmed_with_cpu_decoding() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media("Broken/Truncated.mkv", dir.path());
    let before = std::fs::read(&input).unwrap();
    let mut spec = spec(&input, dir.path(), profile());
    spec.candidates = vec![candidate("h264_qsv", HwApi::Qsv, true), software()];
    let (outcome, _) = run(&config(ValidationLevel::Standard), &spec, &fake_plan).await;
    match outcome {
        JobOutcome::Failed { error, attempt, .. } => {
            assert!(
                error.starts_with("The original file appears damaged or incomplete"),
                "{error}"
            );
            assert_eq!(attempt, 2, "decided by the CPU-decoding attempt");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(std::fs::read(&input).unwrap(), before);
}

/// A work folder that can't be used fails the job before anything is
/// encoded, with a plain reason and the `work_folder` kind: here a file is
/// in the way of the folder's name.
#[tokio::test]
async fn a_blocked_work_folder_is_a_work_folder_problem() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, &library);
    let before = std::fs::read(&input).unwrap();
    let blocker = dir.path().join("work");
    std::fs::write(&blocker, b"not a folder").unwrap();
    let mut cfg = config(ValidationLevel::Quick);
    cfg.temp_dir = Some(blocker.clone());
    let (outcome, _) = run(&cfg, &spec(&input, &library, profile()), &fake_plan).await;
    match outcome {
        JobOutcome::Failed {
            error,
            problem,
            attempt,
            ..
        } => {
            assert_eq!(problem, ProblemKind::WorkFolder);
            assert_eq!(
                error,
                format!(
                    "The work folder {} can't be used because a file with that name is in the \
                     way. Fix it, or choose another work folder, in Settings › Output.",
                    blocker.display()
                )
            );
            assert_eq!(attempt, 0, "nothing was encoded");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(std::fs::read(&input).unwrap(), before);
    assert_eq!(std::fs::read(&blocker).unwrap(), b"not a folder");
}

/// An encode that can't be planned for a reason with the file itself (none
/// of its audio can be read) is an `unreadable_source` problem, in the
/// planner's own words.
#[tokio::test]
async fn unreadable_audio_is_a_problem_with_the_original() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let silent = |_: &PlanRequest<'_>| -> anyhow::Result<FfmpegPlan> {
        Err(chrysopoeia_worker::plan::SourceProblem(
            "None of this file's audio tracks can be read.".into(),
        )
        .into())
    };
    let (outcome, _) = run(
        &config(ValidationLevel::Quick),
        &spec(&input, dir.path(), profile()),
        &silent,
    )
    .await;
    assert!(
        matches!(
            &outcome,
            JobOutcome::Failed {
                problem: ProblemKind::UnreadableSource,
                error,
                ..
            } if error == "None of this file's audio tracks can be read."
        ),
        "{outcome:?}"
    );
}

/// A stand-in for ffmpeg: a shell script that prints `stderr` and fails.
#[cfg(unix)]
fn failing_ffmpeg(dir: &Path, stderr: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-ffmpeg");
    std::fs::write(&path, format!("#!/bin/sh\necho '{stderr}' >&2\nexit 1\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// A disk that fills up during an encode fails every encoder alike: the job
/// stops after the first attempt instead of starting the next one, and says
/// what happened without "None of the 2 ways …".
#[cfg(unix)]
#[tokio::test]
async fn a_full_disk_stops_the_job_instead_of_trying_the_next_encoder() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, &library);
    let before = std::fs::read(&input).unwrap();
    let mut cfg = config(ValidationLevel::Quick);
    cfg.ffmpeg = failing_ffmpeg(
        dir.path(),
        "[out#0/matroska @ 0x1] [error] Error writing trailer: No space left on device",
    );
    let mut spec = spec(&input, &library, profile());
    spec.candidates = vec![candidate("h264_nvenc", HwApi::Nvenc, false), software()];
    let (outcome, updates) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Failed {
        error,
        problem,
        attempt,
        encoder,
        ..
    } = outcome
    else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert_eq!(problem, ProblemKind::DiskFull);
    assert_eq!(attempt, 1);
    assert_eq!(encoder.as_deref(), Some("h264_nvenc"));
    assert!(
        error.starts_with(&format!(
            "The disk ran out of space while the new file was being written in the original's \
             folder {},",
            library.display()
        )),
        "{error}"
    );
    let started = updates
        .iter()
        .filter(|p| p.stage == JobStage::Transcoding && p.progress == 0.0)
        .count();
    assert_eq!(started, 1, "only one encode was started");
    assert_eq!(std::fs::read(&input).unwrap(), before);
    assert!(support::artifacts_in(dir.path()).is_empty());
}

/// A stand-in for ffmpeg 7 writing a file with a track that has had no
/// packet yet (a subtitle track without a line so far): its progress blocks
/// give frames and fps but no output time, to the end. It copies the input
/// to the output.
#[cfg(unix)]
const FRAMES_ONLY_FFMPEG: &str = r#"#!/bin/sh
input=""; prev=""; out=""
for arg in "$@"; do
  if [ "$prev" = "-i" ] && [ -z "$input" ]; then input="$arg"; fi
  prev="$arg"; out="$arg"
done
for frame in 0 24 48 72; do
  printf 'frame=%s\nfps=24.00\nout_time_us=N/A\nout_time_ms=N/A\nout_time=N/A\nspeed=N/A\nprogress=continue\n' "$frame"
  sleep 0.6
done
cp "$input" "$out"
printf 'frame=96\nfps=24.00\nout_time_us=N/A\nout_time=N/A\nspeed=N/A\nprogress=end\n'
"#;

/// The Atlas CPU fallback sat at 0 % with no time left for minutes while
/// ffmpeg 7 encoded frames without reporting an output time. Progress now
/// comes from the frames over the frames the original should have, labelled
/// as an estimate, with the frames and the time spent; and with checks off
/// the encode isn't taken for a cut-off original because no time came.
#[cfg(unix)]
#[tokio::test]
async fn progress_without_output_time_is_estimated_from_frames() {
    use std::os::unix::fs::PermissionsExt;
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let library = dir.path().join("library");
    std::fs::create_dir_all(&library).unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, &library);
    let script = dir.path().join("ffmpeg-7-frames-only");
    std::fs::write(&script, FRAMES_ONLY_FFMPEG).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = config(ValidationLevel::Off);
    cfg.ffmpeg = script;
    let spec = spec(&input, &library, profile());
    let video = spec.probe.primary_video().unwrap();
    let expected = spec.probe.duration_secs.unwrap() * video.frame_rate.unwrap();

    let (outcome, updates) = run(&cfg, &spec, &fake_plan).await;
    assert!(matches!(outcome, JobOutcome::Done { .. }), "{outcome:?}");
    assert_eq!(outcome.attempts().len(), 1, "{outcome:?}");
    assert_eq!(outcome.attempts()[0].result, AttemptResult::Succeeded);
    let encoding: Vec<&JobProgress> = updates
        .iter()
        .filter(|p| p.stage == JobStage::Transcoding && p.progress_basis.is_some())
        .collect();
    assert!(!encoding.is_empty(), "{updates:?}");
    // Never "unknown" here (the frame count can be turned into a share),
    // never a percentage from an output time that never came; only ffmpeg's
    // final report is a measured 100 %.
    let (last, estimated) = encoding.split_last().unwrap();
    assert!(
        estimated
            .iter()
            .all(|p| p.progress_basis == Some(ProgressBasis::Frames) && p.progress < 100.0),
        "{encoding:?}"
    );
    assert!(
        last.progress_basis == Some(ProgressBasis::Frames)
            || (last.progress_basis == Some(ProgressBasis::Time) && last.progress == 100.0),
        "{last:?}"
    );
    let half = encoding
        .iter()
        .find(|p| p.frames == Some(48))
        .unwrap_or_else(|| panic!("{encoding:?}"));
    let share = 48.0 / expected * 100.0;
    assert!((f64::from(half.progress) - share).abs() < 0.5, "{half:?}");
    assert!(half.progress > 40.0 && half.progress < 60.0, "{half:?}");
    assert!(half.eta_secs.is_some_and(|s| s <= 3), "{half:?}");
    assert!(half.elapsed_secs.is_some(), "{half:?}");
    assert!(
        encoding.windows(2).all(|w| w[0].progress <= w[1].progress),
        "{encoding:?}"
    );
}

/// With low priority on (the default), ffmpeg runs under `nice`. A missing
/// ffmpeg is still reported as not started (not as an encoder error quoting
/// `nice`), once, however many encoders there are.
#[tokio::test]
async fn a_missing_ffmpeg_is_reported_plainly_under_low_priority() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let input = support::copy_media(support::MP4_TWO_AUDIO, dir.path());
    let mut cfg = config(ValidationLevel::Quick);
    assert!(cfg.low_priority);
    cfg.ffmpeg = PathBuf::from("/nonexistent/ffmpeg");
    let mut spec = spec(&input, dir.path(), profile());
    spec.candidates = vec![candidate("h264_nvenc", HwApi::Nvenc, false), software()];
    let (outcome, _) = run(&cfg, &spec, &fake_plan).await;
    let JobOutcome::Failed {
        error,
        problem,
        attempt,
        ..
    } = outcome
    else {
        panic!("expected Failed, got {outcome:?}");
    };
    assert_eq!(problem, ProblemKind::Other);
    assert_eq!(attempt, 1);
    assert_eq!(
        error,
        "The converter couldn't be started, so nothing was converted. ffmpeg wasn't found at \
         \"/nonexistent/ffmpeg\". Install ffmpeg, or set FFMPEG_PATH to where it is."
    );
}

/// An MKV with a cover image (a JPEG attachment), made in `dir`; returns
/// the file and the cover's bytes.
fn mkv_with_cover(dir: &Path) -> (PathBuf, Vec<u8>) {
    let jpg = dir.join("art.jpg");
    let input = dir.join("Covered.mkv");
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=orange:size=160x160",
        "-frames:v",
        "1",
        jpg.to_str().unwrap(),
    ]);
    support::ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=size=320x240:rate=25:duration=3",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=500:duration=3",
        "-attach",
        jpg.to_str().unwrap(),
        "-metadata:s:t:0",
        "mimetype=image/jpeg",
        "-metadata:s:t:0",
        "filename=cover.jpg",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-pix_fmt",
        "yuv420p",
        "-c:a",
        "aac",
        input.to_str().unwrap(),
    ]);
    let bytes = std::fs::read(&jpg).unwrap();
    std::fs::remove_file(&jpg).unwrap();
    (input, bytes)
}

/// The cover image of `path`, byte for byte.
fn cover_bytes(path: &Path, index: u32, dir: &Path) -> Vec<u8> {
    let out = dir.join("cover-check.bin");
    support::ffmpeg(&[
        "-i",
        path.to_str().unwrap(),
        "-map",
        &format!("0:{index}"),
        "-c",
        "copy",
        "-frames:v",
        "1",
        "-f",
        "rawvideo",
        out.to_str().unwrap(),
    ]);
    let bytes = std::fs::read(&out).unwrap();
    std::fs::remove_file(&out).unwrap();
    bytes
}

/// With the real planner: an MKV's cover image is copied out of the
/// original, attached to the new MKV under its own name, and the copy is
/// cleaned up. The new file passes verification with the extra picture.
#[tokio::test]
async fn an_mkv_cover_image_survives_the_conversion() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let (input, cover) = mkv_with_cover(dir.path());
    let s = spec(&input, dir.path(), profile());
    assert!(s.probe.streams.iter().any(|st| st.is_attached_pic));
    let cfg = config(ValidationLevel::Standard);
    let (outcome, _) = run(&cfg, &s, &chrysopoeia_worker::build_plan).await;
    let JobOutcome::Done {
        output_path,
        validation,
        command,
        ..
    } = outcome
    else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!(output_path, input);
    assert!(validation.is_some_and(|v| v.passed));
    assert!(command.contains("-attach"), "{command}");
    let probe = support::probe(&input);
    let covers: Vec<_> = probe
        .streams
        .iter()
        .filter(|st| st.is_attached_pic)
        .collect();
    assert_eq!(covers.len(), 1, "{:?}", probe.streams);
    assert_eq!(covers[0].filename.as_deref(), Some("cover.jpg"));
    assert_eq!(covers[0].mimetype.as_deref(), Some("image/jpeg"));
    assert_eq!(cover_bytes(&input, covers[0].index, dir.path()), cover);
    assert_eq!(support::counts(&probe).video, 1);
    assert!(
        support::artifacts_in(dir.path()).is_empty(),
        "{:?}",
        support::artifacts_in(dir.path())
    );
}

/// A cover image that can't be copied out fails the job: the original is
/// left exactly as it was, and nothing is left behind.
#[tokio::test]
async fn a_cover_that_cannot_be_copied_out_keeps_the_original() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let (input, _) = mkv_with_cover(dir.path());
    let original = std::fs::read(&input).unwrap();
    let mut s = spec(&input, dir.path(), profile());
    // The probe names a cover the file doesn't have (it changed since).
    for st in &mut s.probe.streams {
        if st.is_attached_pic {
            st.index = 40;
        }
    }
    let cfg = config(ValidationLevel::Quick);
    let (outcome, _) = run(&cfg, &s, &chrysopoeia_worker::build_plan).await;
    match outcome {
        JobOutcome::Failed {
            error,
            problem,
            command,
            ..
        } => {
            assert_eq!(problem, ProblemKind::Other);
            assert!(
                error.starts_with("The file's cover image couldn't be copied for the new file"),
                "{error}"
            );
            assert!(command.is_some_and(|c| c.contains("0:40")));
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(
        support::artifacts_in(dir.path()).is_empty(),
        "{:?}",
        support::artifacts_in(dir.path())
    );
}

/// In MP4 the cover is kept as cover art, and styled subtitles that would
/// lose their styling keep a Replace-mode job from replacing the original.
#[tokio::test]
async fn mp4_keeps_the_cover_and_replace_mode_keeps_styled_subtitles() {
    require_ffmpeg!();
    let dir = tempfile::tempdir().unwrap();
    let (input, cover) = mkv_with_cover(dir.path());
    let mp4 = TranscodeProfile {
        container: Container::Mp4,
        ..profile()
    };
    let cfg = config(ValidationLevel::Standard);
    let s = spec(&input, dir.path(), mp4.clone());
    let (outcome, _) = run(&cfg, &s, &chrysopoeia_worker::build_plan).await;
    let JobOutcome::Done { output_path, .. } = outcome else {
        panic!("expected Done, got {outcome:?}");
    };
    assert_eq!(output_path, dir.path().join("Covered.mp4"));
    assert!(!input.exists());
    let probe = support::probe(&output_path);
    let art: Vec<_> = probe
        .streams
        .iter()
        .filter(|st| st.is_attached_pic)
        .collect();
    assert_eq!(art.len(), 1, "{:?}", probe.streams);
    assert_eq!(cover_bytes(&output_path, art[0].index, dir.path()), cover);
    assert!(support::artifacts_in(dir.path()).is_empty());

    // A styled ASS track would become plain text: left unchanged.
    let styled_dir = tempfile::tempdir().unwrap();
    let (styled, _) = mkv_with_cover(styled_dir.path());
    let mut s = spec(&styled, styled_dir.path(), mp4);
    let next = u32::try_from(s.probe.streams.len()).unwrap();
    s.probe.streams.push(chrysopoeia_core::StreamInfo {
        index: next,
        kind: Some(chrysopoeia_core::StreamKind::Subtitle),
        codec: "ass".into(),
        ..Default::default()
    });
    let before = std::fs::read(&styled).unwrap();
    let (outcome, _) = run(&cfg, &s, &chrysopoeia_worker::build_plan).await;
    match outcome {
        JobOutcome::Skipped { reason, .. } => assert!(
            reason.starts_with(
                "MP4 can't hold this file's 1 styled subtitle, so it was left unchanged."
            ),
            "{reason}"
        ),
        other => panic!("expected Skipped, got {other:?}"),
    }
    assert_eq!(std::fs::read(&styled).unwrap(), before);
}
