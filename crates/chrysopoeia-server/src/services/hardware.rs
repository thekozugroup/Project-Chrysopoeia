//! Hardware state: detection at start-up and on demand, and the job
//! recommendation for the current preference.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use chrono::Utc;
use chrysopoeia_core::encoder::VIDEO_ENCODERS;
use chrysopoeia_core::{
    ActivityLevel, CpuInfo, EncoderStatus, Event, FfmpegInfo, HardwareInfo, HwApi, HwPreference,
    JobRecommendation, MemoryInfo, SetupHint, SetupHintLevel,
};
use chrysopoeia_hwdetect::DetectOptions;

use crate::config::Config;
use crate::db::activity::ActivityRefs;
use crate::state::{AppState, read, write};

/// Title of the hint shown while the first detection runs.
pub const CHECKING_TITLE: &str = "Checking your hardware…";

/// Detected hardware. `None` until the first detection finishes.
#[derive(Default)]
pub struct HardwareState {
    info: RwLock<Option<Arc<HardwareInfo>>>,
    detect_lock: tokio::sync::Mutex<()>,
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
fn in_container() -> bool {
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
             convert on the CPU. Use \"Detect again\" in Settings to retry."
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
pub async fn detect(state: &AppState) -> Arc<HardwareInfo> {
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
    let info = match state.toolkit.detect_hardware(opts).await {
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
    info
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
