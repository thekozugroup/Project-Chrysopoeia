//! Hardware and encoder detection.
//!
//! Finds CPUs, memory limits and GPUs, checks which ffmpeg encoders are
//! compiled in, and runs a tiny test encode with each hardware encoder so the
//! UI only offers what actually works inside this container.
//!
//! Public API (fixed; see docs/ARCHITECTURE.md):
//! - [`detect`] — full detection, never fails (problems become `hints`).
//! - [`recommend_jobs`] — concurrent job count for the hardware.
//! - [`encoder_candidates`] — ordered encoders to try for a job.

use std::path::PathBuf;
use std::time::Instant;

use chrono::Utc;
use chrysopoeia_core::{
    EncoderCandidate, HardwareInfo, HwPreference, JobRecommendation, VideoCodec,
};

pub mod devices;
pub mod encoders;
pub mod hints;
mod process;
pub mod recommend;

/// Inputs for [`detect`].
#[derive(Debug, Clone)]
pub struct DetectOptions {
    /// ffmpeg to run (a name looked up on `PATH`, or a path).
    pub ffmpeg: PathBuf,
    /// ffprobe to run (a name looked up on `PATH`, or a path).
    pub ffprobe: PathBuf,
    /// Run a short test encode per hardware encoder (recommended). When
    /// false, hardware encoders listed by ffmpeg are reported as unverified
    /// (software encoders never need a test).
    pub verify_encoders: bool,
    /// Root for `/proc`, `/sys` and `/dev` lookups. `/` in production; tests
    /// point it at a fake tree.
    pub system_root: PathBuf,
    /// Preference used to compute `recommended_jobs.total`.
    pub preference: HwPreference,
}

impl Default for DetectOptions {
    fn default() -> Self {
        Self {
            ffmpeg: PathBuf::from("ffmpeg"),
            ffprobe: PathBuf::from("ffprobe"),
            verify_encoders: true,
            system_root: PathBuf::from("/"),
            preference: HwPreference::Auto,
        }
    }
}

/// Detect everything. Never returns an error: missing ffmpeg, missing GPUs and
/// failing encoders are reported through `HardwareInfo::hints`.
///
/// Software encoders listed by ffmpeg are always `verified` (they need no
/// device); `verify_encoders: false` only skips the hardware test encodes.
/// Test encodes take at most 15 s each and 30 s together, NVENC is tested
/// one encoder at a time, and a GPU that hangs isn't tested again.
/// External tools (`lspci`, `nvidia-smi`) are only used when `system_root`
/// is `/`, so a fake tree is never mixed with the real machine.
pub async fn detect(opts: &DetectOptions) -> HardwareInfo {
    let started = Instant::now();
    let platform = devices::Platform::current();
    let real_root = devices::is_real_root(&opts.system_root);

    let (found_devices, probe) = tokio::join!(
        devices::scan(&opts.system_root, real_root, platform),
        encoders::probe_ffmpeg(&opts.ffmpeg, &opts.ffprobe),
    );

    let checks = if probe.info.found {
        let plans = encoders::plan_checks(
            &probe.encoders.video,
            &found_devices,
            platform,
            opts.verify_encoders,
        );
        let context = encoders::FailureContext::from_devices(&found_devices);
        encoders::run_checks(&opts.ffmpeg, plans, encoders::CheckSettings::new(context)).await
    } else {
        Vec::new()
    };
    let audio_encoders = encoders::available_audio_encoders(&probe.encoders.audio);
    let filters = encoders::available_filters(&probe.filters);

    let hints = hints::build_hints(&hints::HintInput {
        ffmpeg: &probe.info,
        devices: &found_devices,
        checks: &checks,
        filters: &filters,
        platform,
    });

    let mut info = HardwareInfo {
        detecting: false,
        cpu: found_devices.cpu.clone(),
        memory: found_devices.memory.clone(),
        gpus: found_devices.gpu_devices(),
        encoders: checks.into_iter().map(|c| c.status).collect(),
        audio_encoders,
        filters,
        ffmpeg: probe.info,
        recommended_jobs: JobRecommendation {
            cpu_jobs: 1,
            gpu_jobs: 0,
            total: 1,
            reason: String::new(),
        },
        hints,
        in_container: found_devices.in_container,
        detected_at: Utc::now(),
    };
    info.recommended_jobs = recommend_jobs(&info, opts.preference);

    tracing::info!(
        cpu = %info.cpu.model,
        cores = info.cpu.logical_cores,
        gpus = info.gpus.len(),
        verified_hw = info
            .encoders
            .iter()
            .filter(|e| e.verified && e.api.is_hardware())
            .count(),
        jobs = info.recommended_jobs.total,
        hints = info.hints.len(),
        elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "hardware detection finished"
    );
    info
}

/// Recommend concurrent jobs for this hardware and preference.
///
/// CPU jobs: one per four effective cores (container CPU limits honoured),
/// between 1 and 8, and at most one per 1.5 GB of memory (container memory
/// limit honoured). GPU jobs: 3 per NVIDIA GPU this container can use, 2 per
/// Intel or AMD GPU, 2 on Apple, capped by effective cores and by memory
/// (1.5 GB per job, since a job whose codec the GPU can't encode runs on the
/// CPU). `total` is the GPU number when the preference isn't `Cpu` and a
/// verified hardware encoder exists (for the pinned API, when one is
/// pinned), else the CPU number.
pub fn recommend_jobs(hw: &HardwareInfo, preference: HwPreference) -> JobRecommendation {
    recommend::recommend_jobs(hw, preference)
}

/// Ordered list of encoders to try for `codec`.
///
/// Verified hardware encoders matching `preference` come first (each with
/// `hw_decode: true`; the worker retries the same encoder with CPU decoding
/// before moving on). The software encoder is appended when `preference` is
/// `Cpu`, when there is no usable hardware encoder, or when `cpu_fallback` is
/// true. Never returns an empty list if a software encoder is available.
///
/// `hw_decode` is true for NVENC, Quick Sync, VA-API and VideoToolbox; AMF,
/// Rockchip and V4L2 encoders take frames decoded on the CPU.
pub fn encoder_candidates(
    hw: &HardwareInfo,
    codec: VideoCodec,
    preference: HwPreference,
    cpu_fallback: bool,
) -> Vec<EncoderCandidate> {
    recommend::encoder_candidates(hw, codec, preference, cpu_fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrysopoeia_core::{GpuVendor, HwApi, SetupHintLevel};
    use std::path::Path;
    use std::time::Duration;

    fn ffmpeg_available() -> bool {
        std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    /// Real detection against the installed ffmpeg and this machine.
    #[tokio::test]
    async fn detect_smoke_test() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not found; skipping");
            return;
        }
        let started = Instant::now();
        let hw = detect(&DetectOptions::default()).await;
        let elapsed = started.elapsed();
        assert!(elapsed < Duration::from_secs(20), "took {elapsed:?}");

        assert!(hw.ffmpeg.found);
        assert!(hw.ffmpeg.ffprobe_found);
        assert!(
            hw.ffmpeg
                .version
                .as_deref()
                .is_some_and(|v| v.starts_with("ffmpeg version"))
        );
        assert!(hw.cpu.logical_cores >= 1);
        assert_ne!(hw.cpu.model, "");
        assert!(hw.memory.total_bytes > 0);

        for name in ["libsvtav1", "libx265", "libx264", "libvpx-vp9"] {
            let status = hw.encoders.iter().find(|e| e.name == name).unwrap();
            assert!(status.available && status.verified, "{status:?}");
            assert_eq!(status.api, HwApi::Software);
        }
        // Every registry encoder is reported, in registry order.
        let names: Vec<&str> = hw.encoders.iter().map(|e| e.name.as_str()).collect();
        let registry: Vec<&str> = chrysopoeia_core::VIDEO_ENCODERS
            .iter()
            .map(|e| e.name)
            .collect();
        assert_eq!(names, registry);
        // Unverified encoders always say why.
        assert!(hw.encoders.iter().filter(|e| !e.verified).all(|e| {
            e.error
                .as_deref()
                .is_some_and(|m| m.ends_with('.') || m.contains("Details:"))
        }));
        assert!(hw.audio_encoders.contains(&"libopus".to_string()));
        assert!(hw.audio_encoders.contains(&"aac".to_string()));
        assert!(hw.filters.contains(&"ssim".to_string()));
        assert!(
            !hw.hints.iter().any(|h| h.level == SetupHintLevel::Error),
            "{:#?}",
            hw.hints
        );

        let rec = &hw.recommended_jobs;
        assert!(rec.total >= 1 && rec.cpu_jobs >= 1);
        assert!(rec.reason.contains("at once"));

        // Without a GPU this machine encodes on the CPU.
        if hw.gpus.is_empty() {
            assert_eq!(rec.gpu_jobs, 0);
            assert!(
                hw.encoders
                    .iter()
                    .filter(|e| e.api.is_hardware())
                    .all(|e| !e.verified)
            );
            assert!(
                hw.hints.iter().any(|h| h.title == "Encoding on the CPU"),
                "{:#?}",
                hw.hints
            );
            let c = encoder_candidates(&hw, VideoCodec::Av1, HwPreference::Auto, true);
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].name, "libsvtav1");
        }

        // The JSON shape round-trips (it is the API contract).
        let json = serde_json::to_string(&hw).unwrap();
        let back: HardwareInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(back, hw);
    }

    #[tokio::test]
    async fn detect_without_ffmpeg_reports_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let hw = detect(&DetectOptions {
            ffmpeg: PathBuf::from("/nonexistent/ffmpeg"),
            ffprobe: PathBuf::from("/nonexistent/ffprobe"),
            verify_encoders: true,
            system_root: tmp.path().to_path_buf(),
            preference: HwPreference::Auto,
        })
        .await;
        assert!(!hw.ffmpeg.found);
        assert!(hw.encoders.is_empty());
        assert!(hw.audio_encoders.is_empty());
        assert_eq!(hw.hints.len(), 1);
        assert_eq!(hw.hints[0].level, SetupHintLevel::Error);
        assert_eq!(hw.cpu.model, "Unknown CPU");
        assert_eq!(hw.recommended_jobs.total, 1);
        // Even so, the worker gets a software encoder to try (and a clear
        // ffmpeg error when it runs).
        assert_eq!(
            encoder_candidates(&hw, VideoCodec::Hevc, HwPreference::Auto, true)[0].name,
            "libx265"
        );
    }

    /// Full detection against a fake machine: Intel iGPU + NVIDIA dGPU in a
    /// CPU- and memory-limited container, with the real ffmpeg.
    #[cfg(unix)]
    #[tokio::test]
    async fn detect_fake_machine() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not found; skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        devices::tests::intel_nvidia_container(tmp.path());
        let hw = detect(&DetectOptions {
            system_root: tmp.path().to_path_buf(),
            verify_encoders: false,
            ..DetectOptions::default()
        })
        .await;

        assert!(hw.in_container);
        assert_eq!(hw.cpu.cgroup_limit, Some(4.0));
        assert_eq!(hw.gpus.len(), 2);
        assert_eq!(hw.gpus[0].vendor, GpuVendor::Intel);
        assert_eq!(
            hw.gpus[0].render_node.as_deref(),
            Some("/dev/dri/renderD128")
        );
        assert_eq!(hw.gpus[1].name, "NVIDIA GeForce RTX 3060");
        // Hardware checks were off: listed hardware encoders are unverified,
        // so jobs follow the CPU: 4 effective cores -> 1 job.
        assert!(
            hw.encoders
                .iter()
                .filter(|e| e.api.is_hardware())
                .all(|e| !e.verified)
        );
        assert_eq!(hw.recommended_jobs.cpu_jobs, 1);
        assert_eq!(hw.recommended_jobs.total, 1);
        assert!(Path::new(&hw.ffmpeg.ffmpeg_path).file_name().is_some());
    }

    /// The same fake machine with real test encodes. On a machine without
    /// these GPUs the tests fail the way a misconfigured container does, and
    /// the hints must say how to fix it.
    #[cfg(unix)]
    #[tokio::test]
    async fn detect_fake_machine_explains_failing_gpus() {
        if !ffmpeg_available() {
            eprintln!("ffmpeg not found; skipping");
            return;
        }
        if Path::new("/dev/nvidia0").exists() || Path::new("/dev/dri").exists() {
            eprintln!("this machine has real GPUs; skipping");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        devices::tests::intel_nvidia_container(tmp.path());
        let started = Instant::now();
        let hw = detect(&DetectOptions {
            system_root: tmp.path().to_path_buf(),
            ..DetectOptions::default()
        })
        .await;
        assert!(started.elapsed() < Duration::from_secs(20));

        let listed = |api: HwApi| {
            hw.encoders
                .iter()
                .filter(move |e| e.api == api && e.available)
        };
        if listed(HwApi::Nvenc).next().is_some() {
            for e in listed(HwApi::Nvenc) {
                assert!(!e.verified);
                let error = e.error.as_deref().unwrap();
                assert!(
                    error.starts_with("The NVIDIA driver isn't available inside the container."),
                    "{error}"
                );
            }
            let nvidia = hw
                .hints
                .iter()
                .find(|h| h.title == "NVIDIA GPU found, but it can't be used")
                .expect("NVIDIA hint");
            assert_eq!(nvidia.fix.as_deref(), Some(hints::NVIDIA_DOCKER_FIX));
        }
        if listed(HwApi::Vaapi).next().is_some() {
            for e in listed(HwApi::Vaapi) {
                assert!(!e.verified);
                assert_eq!(e.device.as_deref(), Some("/dev/dri/renderD128"));
            }
            assert!(
                hw.hints
                    .iter()
                    .any(|h| h.title == "Intel GPU found, but encoding failed"),
                "{:#?}",
                hw.hints
            );
        }
        // Nothing verified in hardware, so the CPU decides: 4 cores -> 1 job.
        assert_eq!(hw.recommended_jobs.total, 1);
        let c = encoder_candidates(&hw, VideoCodec::Hevc, HwPreference::Auto, true);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "libx265");
    }
}
