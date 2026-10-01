//! A share that stops answering while a job runs: the job ends within a
//! bounded time with `NotResponding` (never "failed"), Cancel ends it at
//! once, and a new file that was being put in place is neither abandoned
//! half way nor left out of step with what is reported.
//!
//! `slow_fs::hang` stands in for the share (every bounded check under a
//! hung path blocks until it is lifted) and `finalize::hold` for a rename
//! that waits for it.

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrysopoeia_core::{
    Container, EncoderCandidate, Goal, HwApi, OutputMode, ProbeInfo, TranscodeProfile,
    ValidationLevel, VideoCodec,
};
use chrysopoeia_worker::finalize::hold::{self, Step};
use chrysopoeia_worker::run::{JobOutcome, JobSpec, RunConfig, run_job_with, take_unfinished};
use chrysopoeia_worker::slow_fs::{FOLDER_CHECK_TIMEOUT, hang};
use chrysopoeia_worker::{Decision, FfmpegPlan, PlanRequest};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Far more than any step here should take, even on a loaded machine.
const PATIENCE: Duration = Duration::from_secs(120);

fn config() -> RunConfig {
    RunConfig {
        ffmpeg: PathBuf::from("ffmpeg"),
        ffprobe: PathBuf::from("ffprobe"),
        temp_dir: None,
        validation: ValidationLevel::Off,
        output_mode: OutputMode::Replace,
        output_folder: None,
        keep_file_dates: false,
        low_priority: false,
    }
}

fn spec(input: &Path, library_root: &Path) -> JobSpec {
    JobSpec {
        job_id: Uuid::new_v4(),
        file_id: Uuid::new_v4(),
        input: input.to_path_buf(),
        library_root: library_root.to_path_buf(),
        probe: support::probe(input),
        profile: TranscodeProfile {
            video_codec: VideoCodec::H264,
            container: Container::Mkv,
            min_savings_pct: None,
            ..TranscodeProfile::from_goal(Goal::Compatible)
        },
        candidates: vec![EncoderCandidate {
            name: "libx264".into(),
            codec: VideoCodec::H264,
            api: HwApi::Software,
            device: None,
            hw_decode: false,
        }],
        force: false,
    }
}

/// libx264 ultrafast into Matroska.
fn plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let args = [
        "-y",
        "-i",
        &req.input.to_string_lossy(),
        "-map",
        "0:v:0",
        "-map",
        "0:a?",
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-c:a",
        "aac",
        "-progress",
        "pipe:1",
        "-nostats",
        "-f",
        "matroska",
        &req.output.to_string_lossy(),
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    Ok(FfmpegPlan {
        args,
        notes: vec![],
        expected: support::counts(req.probe),
        covers: Vec::new(),
    })
}

/// [`plan`] reading the input at its native speed (a job as long as the
/// clip).
fn realtime_plan(req: &PlanRequest<'_>) -> anyhow::Result<FfmpegPlan> {
    let mut p = plan(req)?;
    p.args.insert(1, "-re".into());
    Ok(p)
}

fn transcode(_: &ProbeInfo, _: &TranscodeProfile) -> Decision {
    Decision::Transcode
}

/// A library folder with one clip in it (Matroska, so the new file takes
/// the original's name).
fn library() -> (tempfile::TempDir, PathBuf, PathBuf, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("share");
    std::fs::create_dir(&lib).unwrap();
    let input = support::copy_media(support::MKV_1080P_SUBS, &lib);
    let original = std::fs::read(&input).unwrap();
    (dir, lib, input, original)
}

async fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + PATIENCE;
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The original stopped answering before the job started: the job ends
/// with `NotResponding` after the check's timeout (not "failed", and not
/// "preparing" for good), and Cancel ends such a job at once.
#[tokio::test]
async fn a_share_that_hangs_before_the_job_starts() {
    require_ffmpeg!();
    let (_dir, lib, input, original) = library();
    let spec = spec(&input, &lib);
    let hung = hang::hang(&lib);

    let (tx, _rx) = mpsc::channel(1024);
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        PATIENCE,
        run_job_with(
            &config(),
            &spec,
            &plan,
            &transcode,
            tx,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("the job ends");
    assert_eq!(
        outcome,
        JobOutcome::NotResponding {
            path: input.clone()
        }
    );
    assert!(started.elapsed() >= FOLDER_CHECK_TIMEOUT);

    // Cancel while it waits for the share.
    let (tx, _rx) = mpsc::channel(1024);
    let cancel = CancellationToken::new();
    let job = tokio::spawn({
        let (cfg, spec, cancel) = (config(), spec.clone(), cancel.clone());
        async move { run_job_with(&cfg, &spec, &plan, &transcode, tx, cancel).await }
    });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let cancelled_at = Instant::now();
    cancel.cancel();
    let outcome = tokio::time::timeout(PATIENCE, job)
        .await
        .expect("cancel ends it")
        .unwrap();
    assert_eq!(outcome, JobOutcome::Cancelled);
    assert!(
        cancelled_at.elapsed() < FOLDER_CHECK_TIMEOUT,
        "ended at once, not after the check's timeout"
    );
    drop(hung);
    assert_eq!(std::fs::read(&input).unwrap(), original);
    assert!(take_unfinished(spec.job_id).is_none());
}

/// The share stops answering in the middle of the encode: the job notices
/// within a bounded time (well before ffmpeg's 10 minute stall timeout),
/// ends with `NotResponding`, and leaves the original alone; the temp file
/// goes once the share answers again.
#[tokio::test]
async fn a_share_that_hangs_mid_encode() {
    require_ffmpeg!();
    let (_dir, lib, input, original) = library();
    let spec = spec(&input, &lib);
    let (tx, mut rx) = mpsc::channel(1024);
    let job = tokio::spawn({
        let (cfg, spec) = (config(), spec.clone());
        async move {
            run_job_with(
                &cfg,
                &spec,
                &realtime_plan,
                &transcode,
                tx,
                CancellationToken::new(),
            )
            .await
        }
    });
    // Hang once the encode is under way.
    let mut hung = None;
    while let Some(p) = rx.recv().await {
        if p.stage == chrysopoeia_core::JobStage::Transcoding && p.progress > 0.0 {
            hung = Some(hang::hang(&lib));
            break;
        }
    }
    let hung = hung.expect("the encode started");
    let noticed = Instant::now();
    let outcome = tokio::time::timeout(PATIENCE, job)
        .await
        .expect("the job ends")
        .unwrap();
    assert!(
        matches!(&outcome, JobOutcome::NotResponding { path } if path.starts_with(&lib)),
        "{outcome:?}"
    );
    assert!(noticed.elapsed() < PATIENCE);
    assert_eq!(std::fs::read(&input).unwrap(), original);
    drop(hung);
    wait_for("the temp file to go", || {
        support::artifacts_in(&lib).is_empty()
    })
    .await;
    assert_eq!(support::walk(&lib), [input]);
}

/// The share stops answering while the new file is being put in place: the
/// job ends with `NotResponding` without waiting for it, and the step goes
/// on by itself (it can't be called back half way); once the share answers
/// it finishes, and says so.
#[tokio::test]
async fn a_share_that_hangs_while_the_new_file_moves_in() {
    require_ffmpeg!();
    let (_dir, lib, input, original) = library();
    let spec = spec(&input, &lib);
    // The rename that would put the original aside waits for the share.
    let held = hold::hold(spec.job_id, Step::MovedAside);
    let (tx, _rx) = mpsc::channel(1024);
    let job = tokio::spawn({
        let (cfg, spec) = (config(), spec.clone());
        async move { run_job_with(&cfg, &spec, &plan, &transcode, tx, CancellationToken::new()).await }
    });
    wait_for("the original to be moved aside", || held.reached()).await;
    let hung = hang::hang(&lib);
    let outcome = tokio::time::timeout(PATIENCE, job)
        .await
        .expect("the job ends")
        .unwrap();
    assert!(
        matches!(&outcome, JobOutcome::NotResponding { .. }),
        "{outcome:?}"
    );
    let unfinished = take_unfinished(spec.job_id).expect("still being put in place");
    assert!(!unfinished.is_stopped());

    // The share answers again: the step finishes.
    drop(hung);
    drop(held);
    let placed = tokio::time::timeout(PATIENCE, unfinished.outcome())
        .await
        .expect("it ends");
    assert!(
        matches!(&placed, JobOutcome::Done { output_path, .. } if *output_path == input),
        "{placed:?}"
    );
    assert_ne!(std::fs::read(&input).unwrap(), original, "replaced");
    assert!(support::artifacts_in(&lib).is_empty(), "no backup left");
}

/// Cancel while the new file is being put in place on a share that stopped
/// answering: the job ends within seconds, and the step under way is undone
/// at its next safe point once the share answers (the original moved back,
/// nothing left behind). Too late to undo, it finishes.
#[tokio::test]
async fn cancel_while_the_new_file_moves_in() {
    require_ffmpeg!();
    for (step, undone) in [(Step::MovedAside, true), (Step::Committed, false)] {
        let (_dir, lib, input, original) = library();
        let spec = spec(&input, &lib);
        let held = hold::hold(spec.job_id, step);
        let (tx, _rx) = mpsc::channel(1024);
        let cancel = CancellationToken::new();
        let job = tokio::spawn({
            let (cfg, spec, cancel) = (config(), spec.clone(), cancel.clone());
            async move { run_job_with(&cfg, &spec, &plan, &transcode, tx, cancel).await }
        });
        wait_for("the held step", || held.reached()).await;
        let cancelled_at = Instant::now();
        cancel.cancel();
        let outcome = tokio::time::timeout(PATIENCE, job)
            .await
            .expect("the job ends")
            .unwrap();
        assert_eq!(outcome, JobOutcome::Cancelled, "{step:?}");
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(30),
            "{step:?}: ended within seconds"
        );
        let unfinished = take_unfinished(spec.job_id).expect("still being put in place");
        assert!(unfinished.is_stopped());

        drop(held);
        let ended = tokio::time::timeout(PATIENCE, unfinished.outcome())
            .await
            .expect("it ends");
        if undone {
            assert_eq!(ended, JobOutcome::Cancelled, "{step:?}");
            assert_eq!(std::fs::read(&input).unwrap(), original, "original back");
            wait_for("the temp file to go", || {
                support::artifacts_in(&lib).is_empty()
            })
            .await;
            assert_eq!(support::walk(&lib), std::slice::from_ref(&input));
        } else {
            assert!(
                matches!(ended, JobOutcome::Done { .. }),
                "{step:?}: {ended:?}"
            );
            assert_ne!(std::fs::read(&input).unwrap(), original);
            assert!(support::artifacts_in(&lib).is_empty(), "no backup left");
        }
    }
}
