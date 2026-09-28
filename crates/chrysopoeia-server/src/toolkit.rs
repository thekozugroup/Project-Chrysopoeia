//! The media tools the server drives: hardware detection, library walking,
//! probing, skip decisions, transcoding, crash recovery and folder watching.
//!
//! Everything goes through [`MediaToolkit`] so the server can be tested end
//! to end with a fake implementation. [`RealToolkit`] delegates to the
//! `chrysopoeia-hwdetect`, `chrysopoeia-scanner` and `chrysopoeia-worker`
//! crates.
//!
//! The server never calls a [`MediaToolkit`] directly: [`Toolkit`] wraps it and
//! turns a panic inside a tool into an ordinary error, so one misbehaving file
//! or a bug in a tool can never take the server down.

use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrysopoeia_core::{
    EncoderCandidate, HardwareInfo, HwPreference, JobProgress, JobRecommendation, ProbeInfo,
    TranscodeProfile, VideoCodec,
};
use chrysopoeia_hwdetect::DetectOptions;
use chrysopoeia_scanner::{ProbeError, ScanOptions, WalkResult, WatchEvent};
use chrysopoeia_worker::finalize::{Interrupted, Recovery};
use chrysopoeia_worker::{Decision, JobOutcome, JobSpec, RunConfig};
use futures::FutureExt;
use futures::future::BoxFuture;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// A running folder watcher. Dropping it stops watching. Its methods take
/// `&self` and may block for seconds (they walk the folder tree), so the
/// server calls them from blocking threads and never under a lock.
pub trait FolderWatcher: Send + Sync {
    /// Start watching a root recursively.
    fn watch(&self, root: &Path) -> anyhow::Result<()>;
    /// Start watching a root, filtering events with the ignore patterns and
    /// minimum size in `opts` like a scan does. Calling it again for a
    /// watched root applies new options.
    fn watch_with_options(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<()> {
        let _ = opts;
        self.watch(root)
    }
    /// Stop watching a root.
    fn unwatch(&self, root: &Path) -> anyhow::Result<()>;
}

/// The operations the server needs from the media crates.
///
/// Blocking methods (`walk_library`, `is_media_path`) are only called from
/// blocking threads. Async methods return `'static` futures that own their
/// inputs.
pub trait MediaToolkit: Send + Sync + 'static {
    /// Full hardware detection. Never fails; problems become hints.
    fn detect_hardware(&self, opts: DetectOptions) -> BoxFuture<'static, HardwareInfo>;

    /// Concurrent job recommendation for a preference.
    fn recommend_jobs(&self, hw: &HardwareInfo, preference: HwPreference) -> JobRecommendation;

    /// Ordered encoders to try for a job.
    fn encoder_candidates(
        &self,
        hw: &HardwareInfo,
        codec: VideoCodec,
        preference: HwPreference,
        cpu_fallback: bool,
    ) -> Vec<EncoderCandidate>;

    /// Whether a path has a media extension. Blocking-safe, pure.
    fn is_media_path(&self, path: &Path) -> bool;

    /// Whether a path is a video file (media, but not audio only).
    /// Blocking-safe, pure.
    fn is_video_path(&self, path: &Path) -> bool {
        chrysopoeia_scanner::is_video_path(path)
    }

    /// Walk a library root. Blocking.
    fn walk_library(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<WalkResult>;

    /// Probe one file with ffprobe.
    fn probe_file(
        &self,
        path: PathBuf,
        timeout: Duration,
    ) -> BoxFuture<'static, Result<ProbeInfo, ProbeError>>;

    /// Whether a file needs work under a profile.
    fn decide(&self, probe: &ProbeInfo, profile: &TranscodeProfile) -> Decision;

    /// Run one job to completion.
    fn run_job(
        &self,
        cfg: RunConfig,
        spec: JobSpec,
        progress: mpsc::Sender<JobProgress>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, JobOutcome>;

    /// Clean up a temp or backup file left behind by a crash.
    fn recover_artifact(&self, path: PathBuf) -> BoxFuture<'static, anyhow::Result<Recovery>>;

    /// After a crash, finish a replacement whose new file was already in
    /// place (see `chrysopoeia_worker::finalize::resume_replace`). Only
    /// touches the files a job of this id left, so fakes use it as is.
    fn resume_replace(
        &self,
        input: PathBuf,
        final_path: PathBuf,
        job_id: Uuid,
    ) -> BoxFuture<'static, anyhow::Result<Interrupted>> {
        Box::pin(async move {
            chrysopoeia_worker::finalize::resume_replace(&input, &final_path, job_id).await
        })
    }

    /// Start a debounced folder watcher.
    #[allow(clippy::type_complexity)]
    fn start_watcher(
        &self,
        settle: Duration,
    ) -> anyhow::Result<(Box<dyn FolderWatcher>, mpsc::Receiver<WatchEvent>)>;
}

/// Production toolkit backed by the media crates.
#[derive(Debug, Clone)]
pub struct RealToolkit {
    ffprobe: PathBuf,
}

impl RealToolkit {
    /// Toolkit using `ffprobe` for probing.
    pub fn new(ffprobe: PathBuf) -> Self {
        Self { ffprobe }
    }
}

struct RealWatcher(chrysopoeia_scanner::LibraryWatcher);

impl FolderWatcher for RealWatcher {
    fn watch(&self, root: &Path) -> anyhow::Result<()> {
        self.0.watch(root)
    }

    fn watch_with_options(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<()> {
        self.0.watch_with_options(root, opts)
    }

    fn unwatch(&self, root: &Path) -> anyhow::Result<()> {
        self.0.unwatch(root)
    }
}

impl MediaToolkit for RealToolkit {
    fn detect_hardware(&self, opts: DetectOptions) -> BoxFuture<'static, HardwareInfo> {
        Box::pin(async move { chrysopoeia_hwdetect::detect(&opts).await })
    }

    fn recommend_jobs(&self, hw: &HardwareInfo, preference: HwPreference) -> JobRecommendation {
        chrysopoeia_hwdetect::recommend_jobs(hw, preference)
    }

    fn encoder_candidates(
        &self,
        hw: &HardwareInfo,
        codec: VideoCodec,
        preference: HwPreference,
        cpu_fallback: bool,
    ) -> Vec<EncoderCandidate> {
        chrysopoeia_hwdetect::encoder_candidates(hw, codec, preference, cpu_fallback)
    }

    fn is_media_path(&self, path: &Path) -> bool {
        chrysopoeia_scanner::is_media_path(path)
    }

    fn walk_library(&self, root: &Path, opts: &ScanOptions) -> anyhow::Result<WalkResult> {
        chrysopoeia_scanner::walk_library(root, opts)
    }

    fn probe_file(
        &self,
        path: PathBuf,
        timeout: Duration,
    ) -> BoxFuture<'static, Result<ProbeInfo, ProbeError>> {
        let ffprobe = self.ffprobe.clone();
        Box::pin(async move { chrysopoeia_scanner::probe_file(&ffprobe, &path, timeout).await })
    }

    fn decide(&self, probe: &ProbeInfo, profile: &TranscodeProfile) -> Decision {
        chrysopoeia_worker::decide(probe, profile)
    }

    fn run_job(
        &self,
        cfg: RunConfig,
        spec: JobSpec,
        progress: mpsc::Sender<JobProgress>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, JobOutcome> {
        Box::pin(async move { chrysopoeia_worker::run_job(&cfg, &spec, progress, cancel).await })
    }

    fn recover_artifact(&self, path: PathBuf) -> BoxFuture<'static, anyhow::Result<Recovery>> {
        Box::pin(async move { chrysopoeia_worker::finalize::recover_artifact(&path).await })
    }

    fn start_watcher(
        &self,
        settle: Duration,
    ) -> anyhow::Result<(Box<dyn FolderWatcher>, mpsc::Receiver<WatchEvent>)> {
        let (watcher, rx) = chrysopoeia_scanner::LibraryWatcher::start(settle)?;
        Ok((Box::new(RealWatcher(watcher)), rx))
    }
}

/// Extra time a prober gets past its own timeout before it is abandoned.
const PROBE_GRACE: Duration = Duration::from_secs(10);

/// A tool panicked. The panic message has already been logged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{tool} stopped unexpectedly: {message}")]
pub struct ToolPanic {
    /// Which operation panicked.
    pub tool: &'static str,
    /// The panic payload, when it was a string.
    pub message: String,
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

fn to_panic(tool: &'static str, payload: Box<dyn Any + Send>) -> ToolPanic {
    let message = panic_message(payload.as_ref());
    tracing::error!(tool, "a media tool panicked: {message}");
    ToolPanic { tool, message }
}

/// Panic-safe handle to a [`MediaToolkit`]. Cheap to clone.
#[derive(Clone)]
pub struct Toolkit {
    inner: Arc<dyn MediaToolkit>,
}

impl std::fmt::Debug for Toolkit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Toolkit")
    }
}

impl Toolkit {
    /// Wrap a toolkit implementation.
    pub fn new(inner: Arc<dyn MediaToolkit>) -> Self {
        Self { inner }
    }

    fn sync_call<T>(&self, tool: &'static str, f: impl FnOnce() -> T) -> Result<T, ToolPanic> {
        std::panic::catch_unwind(AssertUnwindSafe(f)).map_err(|p| to_panic(tool, p))
    }

    async fn async_call<T>(
        &self,
        tool: &'static str,
        make: impl FnOnce() -> BoxFuture<'static, T>,
    ) -> Result<T, ToolPanic> {
        let fut = self.sync_call(tool, make)?;
        AssertUnwindSafe(fut)
            .catch_unwind()
            .await
            .map_err(|p| to_panic(tool, p))
    }

    /// See [`MediaToolkit::detect_hardware`].
    pub async fn detect_hardware(&self, opts: DetectOptions) -> Result<HardwareInfo, ToolPanic> {
        let inner = Arc::clone(&self.inner);
        self.async_call("hardware detection", move || inner.detect_hardware(opts))
            .await
    }

    /// See [`MediaToolkit::recommend_jobs`].
    pub fn recommend_jobs(
        &self,
        hw: &HardwareInfo,
        preference: HwPreference,
    ) -> Result<JobRecommendation, ToolPanic> {
        self.sync_call("job recommendation", || {
            self.inner.recommend_jobs(hw, preference)
        })
    }

    /// See [`MediaToolkit::encoder_candidates`].
    pub fn encoder_candidates(
        &self,
        hw: &HardwareInfo,
        codec: VideoCodec,
        preference: HwPreference,
        cpu_fallback: bool,
    ) -> Result<Vec<EncoderCandidate>, ToolPanic> {
        self.sync_call("encoder selection", || {
            self.inner
                .encoder_candidates(hw, codec, preference, cpu_fallback)
        })
    }

    /// See [`MediaToolkit::is_media_path`]. Call from a blocking thread.
    pub fn is_media_path_blocking(&self, path: &Path) -> Result<bool, ToolPanic> {
        self.sync_call("media file check", || self.inner.is_media_path(path))
    }

    /// See [`MediaToolkit::is_video_path`]. Call from a blocking thread.
    pub fn is_video_path_blocking(&self, path: &Path) -> Result<bool, ToolPanic> {
        self.sync_call("video file check", || self.inner.is_video_path(path))
    }

    /// Walk a library root on a blocking thread.
    pub async fn walk_library(
        &self,
        root: PathBuf,
        opts: ScanOptions,
    ) -> Result<anyhow::Result<WalkResult>, ToolPanic> {
        let inner = Arc::clone(&self.inner);
        let joined = tokio::task::spawn_blocking(move || {
            std::panic::catch_unwind(AssertUnwindSafe(|| inner.walk_library(&root, &opts)))
                .map_err(|p| to_panic("library walk", p))
        })
        .await;
        match joined {
            Ok(result) => result,
            Err(e) => Err(ToolPanic {
                tool: "library walk",
                message: e.to_string(),
            }),
        }
    }

    /// See [`MediaToolkit::probe_file`]. Panics become [`ProbeError::Spawn`],
    /// and a prober that ignores its timeout is cut off shortly after it.
    pub async fn probe_file(
        &self,
        path: PathBuf,
        timeout: Duration,
    ) -> Result<ProbeInfo, ProbeError> {
        let inner = Arc::clone(&self.inner);
        let call = self.async_call("ffprobe", move || inner.probe_file(path, timeout));
        match tokio::time::timeout(timeout + PROBE_GRACE, call).await {
            Ok(Ok(result)) => result,
            Ok(Err(p)) => Err(ProbeError::Spawn(p.to_string())),
            Err(_) => Err(ProbeError::Timeout(timeout)),
        }
    }

    /// See [`MediaToolkit::decide`].
    pub fn decide(
        &self,
        probe: &ProbeInfo,
        profile: &TranscodeProfile,
    ) -> Result<Decision, ToolPanic> {
        self.sync_call("skip decision", || self.inner.decide(probe, profile))
    }

    /// See [`MediaToolkit::run_job`].
    pub async fn run_job(
        &self,
        cfg: RunConfig,
        spec: JobSpec,
        progress: mpsc::Sender<JobProgress>,
        cancel: CancellationToken,
    ) -> Result<JobOutcome, ToolPanic> {
        let inner = Arc::clone(&self.inner);
        self.async_call("transcoder", move || {
            inner.run_job(cfg, spec, progress, cancel)
        })
        .await
    }

    /// See [`MediaToolkit::recover_artifact`].
    pub async fn recover_artifact(&self, path: PathBuf) -> anyhow::Result<Recovery> {
        let inner = Arc::clone(&self.inner);
        self.async_call("crash recovery", move || inner.recover_artifact(path))
            .await?
    }

    /// See [`MediaToolkit::resume_replace`].
    pub async fn resume_replace(
        &self,
        input: PathBuf,
        final_path: PathBuf,
        job_id: Uuid,
    ) -> anyhow::Result<Interrupted> {
        let inner = Arc::clone(&self.inner);
        self.async_call("crash recovery", move || {
            inner.resume_replace(input, final_path, job_id)
        })
        .await?
    }

    /// See [`MediaToolkit::start_watcher`].
    #[allow(clippy::type_complexity)]
    pub fn start_watcher(
        &self,
        settle: Duration,
    ) -> anyhow::Result<(Box<dyn FolderWatcher>, mpsc::Receiver<WatchEvent>)> {
        self.sync_call("folder watcher", || self.inner.start_watcher(settle))?
    }

    /// Run a watcher call, converting a panic into an error.
    pub fn watcher_call(&self, f: impl FnOnce() -> anyhow::Result<()>) -> anyhow::Result<()> {
        self.sync_call("folder watcher", f)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn panics_in_the_real_stubs_become_errors() {
        // In a checkout where a media crate is unfinished its functions
        // panic; either way the wrapper must not unwind into the caller.
        let tk = Toolkit::new(Arc::new(RealToolkit::new(PathBuf::from("ffprobe-missing"))));
        let _ = tk
            .walk_library(
                PathBuf::from("/nonexistent-chrysopoeia"),
                ScanOptions::default(),
            )
            .await;
        let _ = tk.decide(&ProbeInfo::default(), &TranscodeProfile::default());
    }
}
