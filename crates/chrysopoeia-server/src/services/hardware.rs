//! Hardware state: detection at start-up and on demand, and the job
//! recommendation for the current preference.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use chrono::Utc;
use chrysopoeia_core::encoder::VIDEO_ENCODERS;
use chrysopoeia_core::{
    ActivityLevel, CpuInfo, EncoderStatus, Event, FfmpegInfo, HardwareInfo, HwApi, HwPreference,
    JobRecommendation, MemoryInfo, SetupHint, SetupHintLevel,
};
use chrysopoeia_hwdetect::DetectOptions;

use crate::config::Config;
use crate::db::activity::ActivityRefs;
use crate::state::{AppState, lock, read, write};

/// Title of the hint shown while the first detection runs.
pub const CHECKING_TITLE: &str = "Checking your hardware…";

/// How long after a detection that found the NVIDIA GPU busy it is checked
/// again by itself.
pub const BUSY_RECHECK_DELAY: Duration = Duration::from_secs(180);

/// Automatic re-checks of a busy GPU in a row before leaving it to the
/// user's "Check again" (about half an hour).
pub const BUSY_RECHECK_LIMIT: u32 = 10;

/// How often [`recheck_loop`] looks for a due re-check.
const RECHECK_TICK: Duration = Duration::from_secs(15);

/// Detected hardware. `None` until the first detection finishes.
#[derive(Default)]
pub struct HardwareState {
    info: RwLock<Option<Arc<HardwareInfo>>>,
    detect_lock: tokio::sync::Mutex<()>,
    /// When to check a busy GPU again, if it was busy.
    recheck_at: Mutex<Option<Instant>>,
    /// Automatic re-checks of a busy GPU so far, in a row.
    busy_rechecks: AtomicU32,
}

impl HardwareState {
    /// The last detection result, if any.
    pub fn current(&self) -> Option<Arc<HardwareInfo>> {
        read(&self.info).clone()
    }

    /// Whether detection has finished at least once.
    pub fn is_ready(&self) -> bool {
        read(&self.info).is_some()
    }

    fn set(&self, info: HardwareInfo) -> Arc<HardwareInfo> {
        let info = Arc::new(info);
        *write(&self.info) = Some(Arc::clone(&info));
        info
    }
}

fn logical_cores() -> u32 {
    std::thread::available_parallelism()
        .map(|n| u32::try_from(n.get()).unwrap_or(1))
        .unwrap_or(1)
}

/// Whether we run in a container. Checked once (a stat of two marker files)
/// and cached, so later calls never touch the disk.
pub fn in_container() -> bool {
    static IN_CONTAINER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *IN_CONTAINER.get_or_init(|| {
        std::path::Path::new("/.dockerenv").exists()
            || std::path::Path::new("/run/.containerenv").exists()
    })
}

fn ffmpeg_info(config: &Config) -> FfmpegInfo {
    FfmpegInfo {
        ffmpeg_path: config.ffmpeg.display().to_string(),
        ffprobe_path: config.ffprobe.display().to_string(),
        found: false,
        ffprobe_found: false,
        version: None,
    }
}

/// Conservative CPU job count used before (or without) detection.
fn cpu_jobs_guess(cores: u32) -> u32 {
    (cores / 4).clamp(1, 8)
}

/// The job count "automatic" means before detection has finished.
pub fn jobs_before_detection() -> u32 {
    cpu_jobs_guess(logical_cores())
}

/// What `GET /api/hardware` returns while the first detection is running.
pub fn placeholder(config: &Config) -> HardwareInfo {
    let cores = logical_cores();
    HardwareInfo {
        detecting: true,
        cpu: CpuInfo {
            model: "Unknown CPU".into(),
            logical_cores: cores,
            physical_cores: None,
            cgroup_limit: None,
        },
        memory: MemoryInfo::default(),
        gpus: Vec::new(),
        encoders: Vec::new(),
        audio_encoders: Vec::new(),
        filters: Vec::new(),
        ffmpeg: ffmpeg_info(config),
        recommended_jobs: JobRecommendation {
            cpu_jobs: cpu_jobs_guess(cores),
            gpu_jobs: 0,
            total: cpu_jobs_guess(cores),
            reason: "Checking your hardware…".into(),
        },
        hints: vec![SetupHint {
            level: SetupHintLevel::Info,
            title: CHECKING_TITLE.into(),
            detail: "Chrysopoeia is testing which encoders work on this machine. \
                     This takes a few seconds; conversions start right after."
                .into(),
            fix: None,
        }],
        in_container: in_container(),
        detected_at: Utc::now(),
    }
}

/// Stand-in used when detection itself failed: CPU encoders only, with a
/// hint explaining what happened.
pub fn fallback(config: &Config, problem: &str) -> HardwareInfo {
    let mut info = placeholder(config);
    info.detecting = false;
    info.audio_encoders = chrysopoeia_core::AudioCodec::ALL
        .into_iter()
        .filter(|a| *a != chrysopoeia_core::AudioCodec::Copy)
        .map(|a| a.ffmpeg_encoder().to_string())
        .collect();
    info.encoders = VIDEO_ENCODERS
        .iter()
        .filter(|e| e.api == HwApi::Software)
        .map(|e| EncoderStatus {
            name: e.name.to_string(),
            codec: e.codec,
            api: e.api,
            available: true,
            verified: false,
            device: None,
            error: None,
        })
        .collect();
    info.recommended_jobs.reason =
        "Hardware detection failed, so a conservative CPU job count is used.".into();
    info.hints = vec![SetupHint {
        level: SetupHintLevel::Error,
        title: "Hardware check failed".into(),
        detail: format!(
            "Chrysopoeia couldn't check this machine's hardware ({problem}). It will try to \
             convert on the CPU. Use \"Check again\" in Settings to retry."
        ),
        fix: None,
    }];
    info
}

/// The hardware to report: the detected one, or the placeholder.
pub fn current_or_placeholder(state: &AppState) -> HardwareInfo {
    match state.hardware.current() {
        Some(info) => (*info).clone(),
        None => placeholder(&state.config),
    }
}

/// Run detection now (serialized with other detections), store the result,
/// publish it and wake the dispatcher. Never fails: problems become hints.
///
/// A GPU whose encoding sessions are all taken can't be tested. When that
/// happens to an encoder that worked before (typically because
/// Chrysopoeia's own jobs are using the sessions), the earlier result is
/// kept; when the GPU stays busy, detection runs again by itself after
/// [`BUSY_RECHECK_DELAY`].
pub async fn detect(state: &AppState) -> Arc<HardwareInfo> {
    state.hardware.busy_rechecks.store(0, Ordering::SeqCst);
    run_detection(state).await
}

async fn run_detection(state: &AppState) -> Arc<HardwareInfo> {
    let _guard = state.hardware.detect_lock.lock().await;
    let settings = state.settings();
    let opts = DetectOptions {
        ffmpeg: state.config.ffmpeg.clone(),
        ffprobe: state.config.ffprobe.clone(),
        verify_encoders: true,
        system_root: PathBuf::from("/"),
        preference: settings.hardware,
    };
    let started = std::time::Instant::now();
    let mut info = match state.toolkit.detect_hardware(opts).await {
        Ok(info) => info,
        Err(panic) => {
            state
                .activity(
                    ActivityLevel::Error,
                    "Hardware detection failed; converting on the CPU for now.",
                    ActivityRefs::default(),
                )
                .await;
            fallback(&state.config, &panic.message)
        }
    };
    if let Some(previous) = state.hardware.current() {
        let kept = keep_busy_verifications(&mut info, &previous);
        if !kept.is_empty() {
            tracing::info!(
                encoders = ?kept,
                "the GPU was busy during the check; keeping its earlier successful test"
            );
            if let Ok(r) = state.toolkit.recommend_jobs(&info, settings.hardware) {
                info.recommended_jobs = r;
            }
        }
    }
    let busy = info
        .encoders
        .iter()
        .any(chrysopoeia_hwdetect::is_busy_failure);
    let verified_hw: Vec<&str> = info
        .encoders
        .iter()
        .filter(|e| e.verified && e.api.is_hardware())
        .map(|e| e.name.as_str())
        .collect();
    tracing::info!(
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        gpus = info.gpus.len(),
        hardware_encoders = ?verified_hw,
        recommended_jobs = info.recommended_jobs.total,
        "hardware detection finished"
    );
    let info = state.hardware.set(info);
    state.emit(Event::HardwareUpdated {
        hardware: Box::new((*info).clone()),
    });
    state.dispatcher.wake();
    state.broadcast_queue_state().await;
    if busy {
        schedule_busy_recheck(state);
    } else {
        state.hardware.busy_rechecks.store(0, Ordering::SeqCst);
        *lock(&state.hardware.recheck_at) = None;
    }
    info
}

/// Where the new detection found an encoder unverified only because its GPU
/// was busy, and the previous detection had verified it, keep the previous
/// result: a busy GPU (all NVENC sessions taken, often by our own running
/// jobs) says nothing about whether the encoder works. The busy hint goes
/// too when no busy encoder is left. Returns the names of the encoders kept.
pub fn keep_busy_verifications(info: &mut HardwareInfo, previous: &HardwareInfo) -> Vec<String> {
    let mut kept = Vec::new();
    for status in &mut info.encoders {
        if !chrysopoeia_hwdetect::is_busy_failure(status) {
            continue;
        }
        if let Some(before) = previous
            .encoders
            .iter()
            .find(|e| e.name == status.name && e.verified)
        {
            *status = before.clone();
            kept.push(status.name.clone());
        }
    }
    if !kept.is_empty()
        && !info
            .encoders
            .iter()
            .any(chrysopoeia_hwdetect::is_busy_failure)
    {
        info.hints
            .retain(|h| h.title != chrysopoeia_hwdetect::NVIDIA_BUSY_TITLE);
    }
    kept
}

/// Check the hardware again in a few minutes because a GPU was busy (at most
/// [`BUSY_RECHECK_LIMIT`] times in a row). [`recheck_loop`] runs it.
fn schedule_busy_recheck(state: &AppState) {
    let hw = &state.hardware;
    if hw.busy_rechecks.fetch_add(1, Ordering::SeqCst) >= BUSY_RECHECK_LIMIT {
        return;
    }
    *lock(&hw.recheck_at) = Some(Instant::now() + BUSY_RECHECK_DELAY);
}

/// Runs the re-checks [`schedule_busy_recheck`] asks for, until shutdown.
pub async fn recheck_loop(state: AppState) {
    let mut tick = tokio::time::interval(RECHECK_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            () = state.shutdown.cancelled() => return,
            _ = tick.tick() => {}
        }
        let due = {
            let mut at = lock(&state.hardware.recheck_at);
            match *at {
                Some(t) if t <= Instant::now() => {
                    *at = None;
                    true
                }
                _ => false,
            }
        };
        if due {
            tracing::info!("checking the busy GPU again");
            run_detection(&state).await;
        }
    }
}

/// Recompute `recommended_jobs` after the hardware preference changed.
pub async fn apply_preference(state: &AppState, preference: HwPreference) {
    let Some(current) = state.hardware.current() else {
        return;
    };
    let recommendation = match state.toolkit.recommend_jobs(&current, preference) {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut info = (*current).clone();
    info.recommended_jobs = recommendation;
    let info = state.hardware.set(info);
    state.emit(Event::HardwareUpdated {
        hardware: Box::new((*info).clone()),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_says_checking() {
        let info = placeholder(&Config::default());
        assert_eq!(info.hints.len(), 1);
        assert_eq!(info.hints[0].title, CHECKING_TITLE);
        assert_eq!(info.hints[0].level, SetupHintLevel::Info);
        assert!(info.recommended_jobs.total >= 1);
    }

    #[test]
    fn fallback_offers_cpu_encoders() {
        let info = fallback(&Config::default(), "boom");
        assert!(info.encoders.iter().all(|e| e.api == HwApi::Software));
        assert!(!info.encoders.is_empty());
        assert_eq!(info.hints[0].level, SetupHintLevel::Error);
    }
}
