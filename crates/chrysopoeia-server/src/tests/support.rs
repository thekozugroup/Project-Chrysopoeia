//! Test harness: a fake media toolkit and an in-process app.
//!
//! Fake "media files" are small text files of `key=value` lines that the fake
//! prober reads, e.g. `video=h264\naudio=aac\nwidth=1920\nheight=1080`. A
//! file containing `broken` fails to probe.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use chrysopoeia_core::paths::{ARTIFACT_MARKER, is_backup, original_name_from_backup};
use chrysopoeia_core::{
    CheckStatus, CpuInfo, EncoderCandidate, EncoderStatus, FfmpegInfo, HardwareInfo, HwApi,
    HwPreference, JobProgress, JobRecommendation, JobStage, MemoryInfo, OutputMode, ProbeInfo,
    ProblemKind, StreamInfo, StreamKind, TranscodeProfile, ValidationCheck, ValidationLevel,
    ValidationReport, VideoCodec,
};
use chrysopoeia_hwdetect::DetectOptions;
use chrysopoeia_scanner::{DiscoveredFile, ProbeError, ScanOptions, WalkResult, WatchEvent};
use chrysopoeia_worker::finalize::Recovery;
use chrysopoeia_worker::run::Unfinished;
use chrysopoeia_worker::{Decision, JobOutcome, JobSpec, RunConfig};
use futures::future::BoxFuture;
use serde_json::Value;
use tempfile::TempDir;
use tokio::sync::{Semaphore, mpsc};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use uuid::Uuid;

use crate::app::{self, Startup};
use crate::config::Config;
use crate::state::AppState;
use crate::toolkit::{FolderWatcher, MediaToolkit, Toolkit};

/// What a fake job does.
#[derive(Debug, Clone, PartialEq)]
pub enum Behavior {
    /// Write an output `ratio` times the input size (in the profile's
    /// container) and succeed.
    Done { ratio: f64 },
    /// Nothing written; skipped with this reason.
    Skip(String),
    /// Fail with this error (an encoder problem).
    Fail(String),
    /// Fail with this kind of problem and error.
    FailWith(ProblemKind, String),
    /// Wait until cancelled, or until released (then behave like `Done`).
    Hold,
    /// Wait until cancelled, or until released (then fail with this error).
    HoldFail(String),
    /// Panic inside the transcoder.
    Panic,
    /// The file stopped answering: `NotResponding` at once.
    NotResponding,
    /// Like a job stuck in a step that ignores Cancel until it is released
    /// (a system call waiting for a share): wait for a release, cancelled
    /// or not, then end with `NotResponding`.
    StuckThenNotResponding,
    /// The new file was being put in place when the share stopped
    /// answering: `NotResponding`, with the placing left to the test (see
    /// [`FakeToolkit::finish_placing`]).
    StuckPlacing,
}

/// Holds library walks after they have listed the files, so a test can
/// change things while a scan is in progress.
#[derive(Default)]
pub struct WalkGate {
    closed: AtomicBool,
    reached: AtomicBool,
    open: Mutex<bool>,
    cv: Condvar,
}

impl WalkGate {
    /// Hold the next walks.
    pub fn close(&self) {
        *self.open.lock().unwrap() = false;
        self.reached.store(false, Ordering::SeqCst);
        self.closed.store(true, Ordering::SeqCst);
    }

    /// Let held and future walks through.
    pub fn release(&self) {
        self.closed.store(false, Ordering::SeqCst);
        *self.open.lock().unwrap() = true;
        self.cv.notify_all();
    }

    /// Whether a walk is being held.
    pub fn reached(&self) -> bool {
        self.reached.load(Ordering::SeqCst)
    }

    /// Wait until a walk is being held.
    pub async fn wait_reached(self: &Arc<Self>) {
        let me = Arc::clone(self);
        wait_until("walk to be held", move || {
            let me = Arc::clone(&me);
            async move { me.reached() }
        })
        .await;
    }

    fn pass(&self) {
        if !self.closed.load(Ordering::SeqCst) {
            return;
        }
        self.reached.store(true, Ordering::SeqCst);
        let open = self.open.lock().unwrap();
        // Bounded, so a test that forgets to release can't hang forever.
        let _ = self
            .cv
            .wait_timeout_while(open, Duration::from_secs(30), |open| !*open)
            .unwrap();
    }
}

/// Recorded `watch_with_options` calls: root, ignore patterns, minimum size.
pub type WatchCalls = Arc<Mutex<Vec<(PathBuf, Vec<String>, u64)>>>;

/// Fake media tools with knobs for tests.
pub struct FakeToolkit {
    pub walk_gate: Arc<WalkGate>,
    pub behaviors: Mutex<HashMap<String, Behavior>>,
    pub default_behavior: Mutex<Behavior>,
    pub release: Arc<Semaphore>,
    pub detect_gate: Arc<Semaphore>,
    pub recommended_total: AtomicUsize,
    pub probes: AtomicUsize,
    pub started: Mutex<Vec<String>>,
    pub running: AtomicUsize,
    pub max_running: AtomicUsize,
    pub run_configs: Mutex<Vec<RunConfig>>,
    /// Every job the fake ran, as the dispatcher described it.
    pub run_specs: Mutex<Vec<JobSpec>>,
    pub recovered: Mutex<Vec<PathBuf>>,
    pub walks: Mutex<Vec<PathBuf>>,
    pub watch_tx: Mutex<Option<mpsc::Sender<WatchEvent>>>,
    pub watched: Arc<Mutex<Vec<PathBuf>>>,
    /// Every `watch_with_options` call: root, ignore patterns, minimum size.
    pub watch_calls: WatchCalls,
    /// Hardware to report instead of the default CPU-only machine.
    pub hardware: Mutex<Option<HardwareInfo>>,
    /// Notes every finished fake conversion reports.
    pub done_notes: Mutex<Vec<String>>,
    /// Hardware detections run so far.
    pub detections: AtomicUsize,
    /// File names whose probe waits until they are taken out (a share
    /// that stopped answering while ffprobe reads).
    pub probe_hold: Mutex<HashSet<String>>,
    /// Placings left unfinished by `StuckPlacing` jobs, for the server.
    pub unfinished: Mutex<HashMap<Uuid, Unfinished>>,
    /// How each of them ends, by job id (see [`FakeToolkit::finish_placing`]).
    pub placing_ends: Mutex<HashMap<Uuid, tokio::sync::oneshot::Sender<JobOutcome>>>,
    /// Their stop flags, by job id.
    pub placing_stops: Mutex<HashMap<Uuid, Arc<AtomicBool>>>,
    /// What each `StuckPlacing` job would have put in place, by job id.
    pub placing_specs: Mutex<HashMap<Uuid, (RunConfig, JobSpec)>>,
}

impl Default for FakeToolkit {
    fn default() -> Self {
        Self {
            walk_gate: Arc::new(WalkGate::default()),
            behaviors: Mutex::new(HashMap::new()),
            default_behavior: Mutex::new(Behavior::Done { ratio: 0.5 }),
            release: Arc::new(Semaphore::new(0)),
            detect_gate: Arc::new(Semaphore::new(Semaphore::MAX_PERMITS)),
            recommended_total: AtomicUsize::new(2),
            probes: AtomicUsize::new(0),
            started: Mutex::new(Vec::new()),
            running: AtomicUsize::new(0),
            max_running: AtomicUsize::new(0),
            run_configs: Mutex::new(Vec::new()),
            run_specs: Mutex::new(Vec::new()),
            recovered: Mutex::new(Vec::new()),
            walks: Mutex::new(Vec::new()),
            watch_tx: Mutex::new(None),
            watched: Arc::new(Mutex::new(Vec::new())),
            watch_calls: Arc::new(Mutex::new(Vec::new())),
            hardware: Mutex::new(None),
            done_notes: Mutex::new(Vec::new()),
            detections: AtomicUsize::new(0),
            probe_hold: Mutex::new(HashSet::new()),
            unfinished: Mutex::new(HashMap::new()),
            placing_ends: Mutex::new(HashMap::new()),
            placing_stops: Mutex::new(HashMap::new()),
            placing_specs: Mutex::new(HashMap::new()),
        }
    }
}

impl FakeToolkit {
    pub fn set_behavior(&self, file_name: &str, b: Behavior) {
        self.behaviors
            .lock()
            .unwrap()
            .insert(file_name.to_string(), b);
    }

    pub fn set_default(&self, b: Behavior) {
        *self.default_behavior.lock().unwrap() = b;
    }

    pub fn started(&self) -> Vec<String> {
        self.started.lock().unwrap().clone()
    }

    /// End the placing a `StuckPlacing` job left: `placed` puts the new
    /// file in place (as a finished conversion does) and reports `Done`;
    /// otherwise it reports `Cancelled` (undone, nothing changed).
    pub async fn finish_placing(&self, job_id: Uuid, placed: bool) {
        let end = self.placing_ends.lock().unwrap().remove(&job_id);
        let spec = self.placing_specs.lock().unwrap().remove(&job_id);
        let (Some(end), Some((cfg, spec))) = (end, spec) else {
            panic!("no placing left for job {job_id}");
        };
        let outcome = if placed {
            fake_done(&cfg, &spec, 0.5).await
        } else {
            JobOutcome::Cancelled
        };
        let _ = end.send(outcome);
    }

    /// Whether the server asked job `job_id`'s unfinished placing to stop.
    pub fn placing_stopped(&self, job_id: Uuid) -> bool {
        self.placing_stops
            .lock()
            .unwrap()
            .get(&job_id)
            .is_some_and(|s| s.load(Ordering::SeqCst))
    }

    pub fn watch_sender(&self) -> mpsc::Sender<WatchEvent> {
        self.watch_tx
            .lock()
            .unwrap()
            .clone()
            .expect("watcher not started")
    }

    fn behavior_for(&self, name: &str) -> Behavior {
        self.behaviors
            .lock()
            .unwrap()
            .get(name)
            .cloned()
            .unwrap_or_else(|| self.default_behavior.lock().unwrap().clone())
    }
}

pub const MEDIA_EXTS: &[&str] = &["mkv", "mp4", "avi", "ts", "m4v", "flac", "webm"];

fn is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| MEDIA_EXTS.contains(&e.to_ascii_lowercase().as_str()))
}

fn walk(root: &Path, opts: &ScanOptions) -> anyhow::Result<WalkResult> {
    let meta = std::fs::metadata(root)?;
    anyhow::ensure!(meta.is_dir(), "not a folder");
    let mut out = WalkResult::default();
    // Like the real walker: unusable patterns are noted on the root.
    for problem in chrysopoeia_scanner::IgnoreRules::new(&opts.ignore_patterns).invalid_patterns() {
        out.notes.push((root.to_path_buf(), problem.clone()));
    }
    let ignore = crate::services::library::compile_ignore(&opts.ignore_patterns);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match std::fs::read_dir(&dir) {
            Ok(rd) => rd,
            Err(e) => {
                out.errors.push((dir.clone(), e.to_string()));
                continue;
            }
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                stack.push(path);
                continue;
            }
            if name.contains(ARTIFACT_MARKER) {
                out.artifacts.push(path);
                continue;
            }
            if name.starts_with('.') || !is_media(&path) {
                continue;
            }
            if let Ok(rel) = path.strip_prefix(root)
                && ignore.is_match(rel)
            {
                continue;
            }
            let Ok(m) = entry.metadata() else { continue };
            if m.len() < opts.min_size_bytes {
                continue;
            }
            out.files.push(DiscoveredFile {
                path,
                size: m.len(),
                modified: m
                    .modified()
                    .map(DateTime::<Utc>::from)
                    .unwrap_or_else(|_| Utc::now()),
            });
        }
    }
    Ok(out)
}

/// Parse the fake media format.
pub fn fake_probe(content: &str, size: u64) -> Result<ProbeInfo, ProbeError> {
    if content.contains("broken") {
        return Err(ProbeError::Unreadable(
            "This file can't be read as a video: its data is invalid or cut short.".into(),
        ));
    }
    let kv: HashMap<&str, &str> = content
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    let mut streams = Vec::new();
    if let Some(v) = kv.get("video") {
        streams.push(StreamInfo {
            index: 0,
            kind: Some(StreamKind::Video),
            codec: (*v).to_string(),
            width: kv.get("width").and_then(|w| w.parse().ok()).or(Some(1920)),
            height: kv.get("height").and_then(|h| h.parse().ok()).or(Some(1080)),
            ..Default::default()
        });
    }
    if let Some(a) = kv.get("audio") {
        streams.push(StreamInfo {
            index: 1,
            kind: Some(StreamKind::Audio),
            codec: (*a).to_string(),
            channels: Some(2),
            ..Default::default()
        });
    }
    Ok(ProbeInfo {
        container: kv
            .get("container")
            .copied()
            .unwrap_or("matroska")
            .to_string(),
        duration_secs: Some(
            kv.get("duration")
                .and_then(|d| d.parse().ok())
                .unwrap_or(60.0),
        ),
        bit_rate: Some(4_000_000),
        size_bytes: size,
        streams,
        ..Default::default()
    })
}

struct FakeWatcher {
    watched: Arc<Mutex<Vec<PathBuf>>>,
    calls: WatchCalls,
}

impl FolderWatcher for FakeWatcher {
    fn watch(&self, root: &Path) -> anyhow::Result<()> {
        let mut w = self.watched.lock().unwrap();
        if !w.iter().any(|p| p == root) {
            w.push(root.to_path_buf());
        }
        Ok(())
    }

    fn watch_with_options(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push((
            root.to_path_buf(),
            opts.ignore_patterns.clone(),
            opts.min_size_bytes,
        ));
        self.watch(root)
    }

    fn unwatch(&self, root: &Path) -> anyhow::Result<()> {
        self.watched.lock().unwrap().retain(|p| p != root);
        Ok(())
    }
}

impl Drop for FakeWatcher {
    fn drop(&mut self) {
        self.watched.lock().unwrap().clear();
    }
}

fn passed_report() -> ValidationReport {
    ValidationReport {
        passed: true,
        level: ValidationLevel::Standard,
        checks: vec![ValidationCheck {
            id: "visual".into(),
            label: "Looks the same".into(),
            status: CheckStatus::Pass,
            detail: "SSIM 0.98".into(),
            value: Some(0.98),
        }],
        ssim_min: Some(0.97),
        ssim_avg: Some(0.98),
        psnr_avg: Some(42.0),
        elapsed_secs: 0.1,
    }
}

fn final_path(cfg: &RunConfig, spec: &JobSpec) -> PathBuf {
    let ext = spec.profile.container.extension();
    match (cfg.output_mode, &cfg.output_folder) {
        (OutputMode::Folder, Some(folder)) => {
            let rel = spec
                .input
                .strip_prefix(&spec.library_root)
                .unwrap_or(&spec.input);
            folder.join(rel).with_extension(ext)
        }
        _ => spec.input.with_extension(ext),
    }
}

async fn fake_done(cfg: &RunConfig, spec: &JobSpec, ratio: f64) -> JobOutcome {
    let original = tokio::fs::read(&spec.input).await.unwrap_or_default();
    let original_size = original.len() as u64;
    let out_len = ((original_size as f64) * ratio).round().max(1.0) as usize;
    let out = final_path(cfg, spec);
    if let Some(parent) = out.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    // Keep it probe-able: same text with the new codec, resized.
    let body = String::from_utf8_lossy(&original).replace(
        &format!("video={}", spec.probe.video_codec().unwrap_or("")),
        &format!("video={}", spec.profile.video_codec.ffprobe_name()),
    );
    let mut bytes = body.into_bytes();
    bytes.resize(out_len, b'#');
    if let Err(e) = tokio::fs::write(&out, &bytes).await {
        return JobOutcome::Failed {
            error: format!("write failed: {e}"),
            problem: ProblemKind::Other,
            log_tail: None,
            command: None,
            encoder: None,
            attempt: 1,
            validation: None,
        };
    }
    if cfg.output_mode == OutputMode::Replace && out != spec.input {
        let _ = tokio::fs::remove_file(&spec.input).await;
    }
    let cand = spec.candidates.first();
    JobOutcome::Done {
        output_path: out,
        output_size: bytes.len() as u64,
        original_size,
        encoder: cand.map_or_else(|| "libx265".into(), |c| c.name.clone()),
        hw_api: cand.map_or(HwApi::Software, |c| c.api),
        attempt: 1,
        validation: Some(passed_report()),
        command: format!("ffmpeg -i {}", spec.input.display()),
        notes: vec![],
    }
}

struct RunningCount<'a>(&'a FakeToolkit);

impl Drop for RunningCount<'_> {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

impl MediaToolkit for Arc<FakeToolkit> {
    fn detect_hardware(&self, opts: DetectOptions) -> BoxFuture<'static, HardwareInfo> {
        let me = Arc::clone(self);
        Box::pin(async move {
            let _permit = me.detect_gate.acquire().await;
            me.detections.fetch_add(1, Ordering::SeqCst);
            if let Some(hw) = me.hardware.lock().unwrap().clone() {
                return hw;
            }
            let total = me.recommended_total.load(Ordering::SeqCst) as u32;
            HardwareInfo {
                detecting: false,
                cpu: CpuInfo {
                    model: "Fake CPU".into(),
                    logical_cores: 8,
                    physical_cores: Some(4),
                    cgroup_limit: None,
                },
                memory: MemoryInfo {
                    total_bytes: 16 << 30,
                    available_bytes: 8 << 30,
                    cgroup_limit_bytes: None,
                },
                gpus: vec![],
                encoders: ["libsvtav1", "libx265", "libx264", "libvpx-vp9"]
                    .iter()
                    .filter_map(|n| chrysopoeia_core::encoder::find_encoder(n))
                    .map(|e| EncoderStatus {
                        name: e.name.into(),
                        codec: e.codec,
                        api: e.api,
                        available: true,
                        verified: true,
                        device: None,
                        error: None,
                    })
                    .collect(),
                audio_encoders: vec!["libopus".into(), "aac".into()],
                filters: vec!["ssim".into(), "psnr".into()],
                ffmpeg: FfmpegInfo {
                    ffmpeg_path: opts.ffmpeg.display().to_string(),
                    ffprobe_path: opts.ffprobe.display().to_string(),
                    found: true,
                    ffprobe_found: true,
                    version: Some("ffmpeg version 6.1-fake".into()),
                },
                recommended_jobs: JobRecommendation {
                    cpu_jobs: total,
                    gpu_jobs: 0,
                    total,
                    reason: "fake".into(),
                },
                hints: vec![],
                in_container: false,
                detected_at: Utc::now(),
            }
        })
    }

    fn recommend_jobs(&self, hw: &HardwareInfo, preference: HwPreference) -> JobRecommendation {
        let _ = preference;
        hw.recommended_jobs.clone()
    }

    fn encoder_candidates(
        &self,
        hw: &HardwareInfo,
        codec: VideoCodec,
        _preference: HwPreference,
        _cpu_fallback: bool,
    ) -> Vec<EncoderCandidate> {
        // Verified hardware encoders first, then the CPU, like hwdetect.
        let hardware = hw
            .verified_encoders(codec)
            .filter(|e| e.api.is_hardware())
            .map(|e| EncoderCandidate {
                name: e.name.clone(),
                codec,
                api: e.api,
                device: e.device.clone(),
                hw_decode: true,
            });
        let software = chrysopoeia_core::encoder::encoders_for(codec)
            .filter(|e| e.api == HwApi::Software)
            .take(1)
            .map(|e| EncoderCandidate {
                name: e.name.into(),
                codec,
                api: HwApi::Software,
                device: None,
                hw_decode: false,
            });
        hardware.chain(software).collect()
    }

    fn walk_library(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<WalkResult> {
        self.walks.lock().unwrap().push(root.to_path_buf());
        let result = walk(root, opts);
        self.walk_gate.pass();
        result
    }

    fn probe_file(
        &self,
        path: PathBuf,
        _timeout: Duration,
    ) -> BoxFuture<'static, Result<ProbeInfo, ProbeError>> {
        let me = Arc::clone(self);
        Box::pin(async move {
            me.probes.fetch_add(1, Ordering::SeqCst);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            while me.probe_hold.lock().unwrap().contains(&name) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|e| ProbeError::Unreadable(e.to_string()))?;
            fake_probe(&String::from_utf8_lossy(&bytes), bytes.len() as u64)
        })
    }

    fn decide(&self, probe: &ProbeInfo, profile: &TranscodeProfile) -> Decision {
        let Some(v) = probe.video_codec() else {
            return Decision::Skip {
                reason: "Audio-only files are left as they are".into(),
            };
        };
        if VideoCodec::from_probe_name(v) == Some(profile.video_codec) {
            return Decision::Skip {
                reason: format!("Already {}", profile.video_codec.label()),
            };
        }
        Decision::Transcode
    }

    fn run_job(
        &self,
        cfg: RunConfig,
        spec: JobSpec,
        progress: mpsc::Sender<JobProgress>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, JobOutcome> {
        let me = Arc::clone(self);
        Box::pin(async move {
            let name = spec
                .input
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            me.started.lock().unwrap().push(name.clone());
            me.run_configs.lock().unwrap().push(cfg.clone());
            me.run_specs.lock().unwrap().push(spec.clone());
            let now = me.running.fetch_add(1, Ordering::SeqCst) + 1;
            me.max_running.fetch_max(now, Ordering::SeqCst);
            let _count = RunningCount(&me);
            let _ = progress
                .send(JobProgress {
                    job_id: spec.job_id,
                    file_id: spec.file_id,
                    stage: JobStage::Transcoding,
                    progress: 50.0,
                    fps: Some(100.0),
                    speed: Some(4.0),
                    eta_secs: Some(10),
                    encoder: spec.candidates.first().map(|c| c.name.clone()),
                    hw_api: Some(HwApi::Software),
                    attempt: 1,
                })
                .await;
            let notes = me.done_notes.lock().unwrap().clone();
            let with_notes = |outcome: JobOutcome| match outcome {
                JobOutcome::Done {
                    output_path,
                    output_size,
                    original_size,
                    encoder,
                    hw_api,
                    attempt,
                    validation,
                    command,
                    ..
                } => JobOutcome::Done {
                    output_path,
                    output_size,
                    original_size,
                    encoder,
                    hw_api,
                    attempt,
                    validation,
                    command,
                    notes: notes.clone(),
                },
                other => other,
            };
            match me.behavior_for(&name) {
                Behavior::Done { ratio } => with_notes(fake_done(&cfg, &spec, ratio).await),
                Behavior::Skip(reason) => JobOutcome::Skipped {
                    reason,
                    encoder: Some("libx265".into()),
                    output_size: Some(10),
                },
                Behavior::Fail(error) => JobOutcome::Failed {
                    error,
                    problem: ProblemKind::Encoder,
                    log_tail: Some("Error while decoding stream #0:0".into()),
                    command: Some("ffmpeg -i in out".into()),
                    encoder: Some("libx265".into()),
                    attempt: 2,
                    validation: None,
                },
                Behavior::FailWith(problem, error) => JobOutcome::Failed {
                    error,
                    problem,
                    log_tail: None,
                    command: Some("ffmpeg -i in out".into()),
                    encoder: Some("libx265".into()),
                    attempt: 2,
                    validation: None,
                },
                Behavior::HoldFail(error) => {
                    tokio::select! {
                        () = cancel.cancelled() => JobOutcome::Cancelled,
                        permit = me.release.acquire() => {
                            if let Ok(p) = permit { p.forget(); }
                            JobOutcome::Failed {
                                error,
                                problem: ProblemKind::Encoder,
                                log_tail: None,
                                command: None,
                                encoder: None,
                                attempt: 0,
                                validation: None,
                            }
                        }
                    }
                }
                Behavior::Hold => {
                    tokio::select! {
                        () = cancel.cancelled() => JobOutcome::Cancelled,
                        permit = me.release.acquire() => {
                            if let Ok(p) = permit { p.forget(); }
                            fake_done(&cfg, &spec, 0.5).await
                        }
                    }
                }
                Behavior::Panic => panic!("fake transcoder exploded"),
                Behavior::NotResponding => JobOutcome::NotResponding { path: spec.input },
                Behavior::StuckThenNotResponding => {
                    if let Ok(p) = me.release.acquire().await {
                        p.forget();
                    }
                    JobOutcome::NotResponding { path: spec.input }
                }
                Behavior::StuckPlacing => {
                    let stop = Arc::new(AtomicBool::new(cancel.is_cancelled()));
                    let (unfinished, end) = Unfinished::new(Arc::clone(&stop));
                    me.unfinished
                        .lock()
                        .unwrap()
                        .insert(spec.job_id, unfinished);
                    me.placing_ends.lock().unwrap().insert(spec.job_id, end);
                    me.placing_stops.lock().unwrap().insert(spec.job_id, stop);
                    me.placing_specs
                        .lock()
                        .unwrap()
                        .insert(spec.job_id, (cfg.clone(), spec.clone()));
                    JobOutcome::NotResponding {
                        path: spec
                            .input
                            .parent()
                            .map(Path::to_path_buf)
                            .unwrap_or_default(),
                    }
                }
            }
        })
    }

    fn take_unfinished(&self, job_id: Uuid) -> Option<Unfinished> {
        self.unfinished.lock().unwrap().remove(&job_id)
    }

    fn recover_artifact(&self, path: PathBuf) -> BoxFuture<'static, anyhow::Result<Recovery>> {
        let me = Arc::clone(self);
        Box::pin(async move {
            me.recovered.lock().unwrap().push(path.clone());
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if is_backup(&name) {
                let original =
                    path.with_file_name(original_name_from_backup(&name).unwrap_or_default());
                if tokio::fs::metadata(&original).await.is_ok() {
                    tokio::fs::remove_file(&path).await?;
                    Ok(Recovery::DeletedBackup)
                } else {
                    tokio::fs::rename(&path, &original).await?;
                    Ok(Recovery::RestoredBackup(original))
                }
            } else {
                tokio::fs::remove_file(&path).await?;
                Ok(Recovery::DeletedTemp)
            }
        })
    }

    fn start_watcher(
        &self,
        _settle: Duration,
    ) -> anyhow::Result<(Box<dyn FolderWatcher>, mpsc::Receiver<WatchEvent>)> {
        let (tx, rx) = mpsc::channel(64);
        *self.watch_tx.lock().unwrap() = Some(tx);
        Ok((
            Box::new(FakeWatcher {
                watched: Arc::clone(&self.watched),
                calls: Arc::clone(&self.watch_calls),
            }),
            rx,
        ))
    }
}

/// Adjusts the config before start (gets the temp root).
pub type Configure = Box<dyn FnOnce(&mut Config, &Path) + Send>;

/// Options for [`TestApp::start`].
pub struct TestOptions {
    pub fake: Arc<FakeToolkit>,
    pub configure: Configure,
    pub background: bool,
    /// Wait for hardware detection before returning.
    pub wait_ready: bool,
    /// Reuse an existing temp dir (for restart tests).
    pub dir: Option<TempDir>,
    /// Use the real media crates instead of the fake.
    pub real_toolkit: bool,
}

impl Default for TestOptions {
    fn default() -> Self {
        Self {
            fake: Arc::new(FakeToolkit::default()),
            configure: Box::new(|_, _| {}),
            background: true,
            wait_ready: true,
            dir: None,
            real_toolkit: false,
        }
    }
}

/// An app with a fake toolkit, a temp data dir and a temp media folder.
pub struct TestApp {
    pub dir: TempDir,
    pub media: PathBuf,
    pub state: AppState,
    pub router: Router,
    pub fake: Arc<FakeToolkit>,
    pub startup: Startup,
}

/// Response of a test request.
pub struct Resp {
    pub status: StatusCode,
    pub headers: axum::http::HeaderMap,
    pub json: Value,
    pub text: String,
}

impl TestApp {
    pub async fn new() -> Self {
        Self::start(TestOptions::default()).await
    }

    pub async fn start(opts: TestOptions) -> Self {
        let dir = opts.dir.unwrap_or_else(|| tempfile::tempdir().unwrap());
        let root = dir.path().to_path_buf();
        let media = root.join("media");
        std::fs::create_dir_all(&media).unwrap();
        let mut config = Config {
            data_dir: root.join("data"),
            web_dir: root.join("web"),
            browse_roots: vec![media.clone()],
            // Test files are written right before they are scanned.
            settle: Duration::ZERO,
            ..Config::default()
        };
        (opts.configure)(&mut config, &root);
        let toolkit = if opts.real_toolkit {
            Toolkit::new(Arc::new(crate::toolkit::RealToolkit::new(
                config.ffprobe.clone(),
            )))
        } else {
            Toolkit::new(Arc::new(Arc::clone(&opts.fake)))
        };
        let (state, startup) = app::build(config, toolkit).await.unwrap();
        if opts.background {
            app::start_background(&state, startup);
        }
        let router = crate::web::app(state.clone()).await;
        let app = Self {
            dir,
            media,
            state,
            router,
            fake: opts.fake,
            startup,
        };
        if opts.background && opts.wait_ready {
            app.wait_ready().await;
        }
        app
    }

    /// Wait until hardware detection and recovery are done.
    pub async fn wait_ready(&self) {
        let state = self.state.clone();
        wait_until("startup", move || {
            let state = state.clone();
            async move { state.hardware.is_ready() && state.dispatcher.is_ready() }
        })
        .await;
    }

    /// Shut down like the binary does (without closing the process).
    pub async fn stop(self) -> TempDir {
        app::begin_shutdown(&self.state).await;
        app::finish_shutdown(&self.state).await;
        self.dir
    }

    pub async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Resp {
        let mut req = Request::builder().method(method).uri(path);
        let body = match body {
            Some(v) => {
                req = req.header("content-type", "application/json");
                Body::from(v.to_string())
            }
            None => Body::empty(),
        };
        let resp = self
            .router
            .clone()
            .oneshot(req.body(body).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let bytes = axum::body::to_bytes(resp.into_body(), 16 << 20)
            .await
            .unwrap();
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        Resp {
            status,
            headers,
            json,
            text,
        }
    }

    pub async fn get(&self, path: &str) -> Resp {
        self.request(Method::GET, path, None).await
    }

    pub async fn post(&self, path: &str, body: Value) -> Resp {
        self.request(Method::POST, path, Some(body)).await
    }

    pub async fn post_empty(&self, path: &str) -> Resp {
        self.request(Method::POST, path, None).await
    }

    pub async fn patch(&self, path: &str, body: Value) -> Resp {
        self.request(Method::PATCH, path, Some(body)).await
    }

    pub async fn delete(&self, path: &str) -> Resp {
        self.request(Method::DELETE, path, None).await
    }

    /// Write a fake media file under the media folder.
    pub fn write(&self, rel: &str, content: &str) -> PathBuf {
        let p = self.media.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, content).unwrap();
        p
    }

    /// Create a library for `rel` (under the media folder) and wait for its
    /// first scan.
    pub async fn add_library(&self, rel: &str, extra: Value) -> Value {
        let dir = self.media.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = serde_json::json!({ "path": dir.to_str().unwrap() });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        let r = self.post("/api/libraries", body).await;
        assert_eq!(r.status, StatusCode::CREATED, "{}", r.text);
        let id = r.json["id"].as_str().unwrap().to_string();
        self.wait_scan(&id).await;
        self.get(&format!("/api/libraries/{id}")).await.json
    }

    /// Wait until a library is no longer scanning.
    pub async fn wait_scan(&self, id: &str) {
        let uuid = uuid::Uuid::parse_str(id).unwrap();
        let state = self.state.clone();
        wait_until("scan to finish", move || {
            let state = state.clone();
            async move { !state.library.is_scanning(uuid) }
        })
        .await;
    }

    /// Rescan a library and wait for it. A scan the server started by itself
    /// (after a job, say) may still be running on a busy machine: that one
    /// is waited for first, so the rescan sees everything written before.
    pub async fn rescan(&self, id: &str) {
        for _ in 0..50 {
            let r = self.post_empty(&format!("/api/libraries/{id}/scan")).await;
            if r.status == StatusCode::CONFLICT {
                self.wait_scan(id).await;
                continue;
            }
            assert_eq!(r.status, StatusCode::ACCEPTED, "{}", r.text);
            self.wait_scan(id).await;
            return;
        }
        panic!("the library never stopped scanning long enough to be rescanned");
    }

    /// Files of a library, keyed by file name.
    pub async fn files_by_name(&self, library: &str) -> HashMap<String, Value> {
        let r = self
            .get(&format!("/api/files?library={library}&limit=500"))
            .await;
        r.json["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["file_name"].as_str().unwrap().to_string(), f.clone()))
            .collect()
    }

    /// Wait until the queue has no running or queued jobs.
    pub async fn wait_queue_idle(&self) {
        let state = self.state.clone();
        wait_until("queue to drain", move || {
            let state = state.clone();
            async move {
                crate::db::jobs::counts(state.db.pool())
                    .await
                    .map(|(r, q)| r == 0 && q == 0)
                    .unwrap_or(false)
                    && state.dispatcher.running_count() == 0
            }
        })
        .await;
    }

    /// Pause the queue.
    pub async fn pause(&self) {
        let r = self.post_empty("/api/queue/pause").await;
        assert_eq!(r.status, StatusCode::OK);
    }

    /// Resume the queue.
    pub async fn resume(&self) {
        let r = self.post_empty("/api/queue/resume").await;
        assert_eq!(r.status, StatusCode::OK);
    }
}

/// Poll `f` until it returns true (10 s limit).
pub async fn wait_until<F, Fut>(what: &str, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    wait_until_for(what, Duration::from_secs(10), f).await;
}

/// Poll `f` until it returns true, up to `limit`.
pub async fn wait_until_for<F, Fut>(what: &str, limit: Duration, f: F)
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + limit;
    loop {
        if f().await {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Fake media content helpers.
pub fn h264() -> &'static str {
    "video=h264\naudio=aac\nwidth=1920\nheight=1080\npadding=0000000000000000000000000000000000000000\n"
}

pub fn av1() -> &'static str {
    "video=av1\naudio=opus\nwidth=3840\nheight=2160\n"
}

pub fn audio_only() -> &'static str {
    "audio=flac\n"
}
