//! How many jobs to run at once, and which encoders to try for a job.
//!
//! CPU encodes scale with cores (about four per encode keeps each ffmpeg
//! busy without thrashing) and memory (about 1.5 GB per encode). GPU encodes
//! are limited by the GPU's encoder blocks and, for consumer NVIDIA cards,
//! by the driver's session limit, so they get a small fixed number per GPU.
//! They are capped by memory too: a job whose codec the GPU can't encode
//! falls back to a software encoder and needs as much memory as a CPU job.

use std::collections::BTreeSet;

use chrysopoeia_core::{
    CpuInfo, EncoderCandidate, GpuDevice, GpuVendor, HardwareInfo, HwApi, HwPreference,
    JobRecommendation, MemoryInfo, VIDEO_ENCODERS, VideoCodec,
};

/// CPU cores one software encode keeps busy.
const CORES_PER_CPU_JOB: u32 = 4;
/// More CPU encodes than this rarely finish sooner.
const MAX_CPU_JOBS: u32 = 8;
/// Memory one encode needs (1.5 GiB), including decode buffers and headroom.
pub(crate) const BYTES_PER_JOB: u64 = 3 * 512 * 1024 * 1024;
/// Parallel encodes per NVIDIA GPU (consumer cards limit NVENC sessions).
const NVENC_JOBS_PER_GPU: u32 = 3;
/// Parallel encodes per Intel or AMD GPU.
const INTEL_AMD_JOBS_PER_GPU: u32 = 2;
/// Parallel encodes on Apple's media engine.
const APPLE_JOBS: u32 = 2;
/// Parallel encodes on a Rockchip video engine.
const RKMPP_JOBS: u32 = 2;
/// Parallel encodes on a V4L2 encoder (usually one hardware block).
const V4L2_JOBS: u32 = 1;

/// Hardware APIs in the order they are preferred.
pub const HW_API_ORDER: [HwApi; 7] = [
    HwApi::Nvenc,
    HwApi::Qsv,
    HwApi::Vaapi,
    HwApi::Amf,
    HwApi::VideoToolbox,
    HwApi::Rkmpp,
    HwApi::V4l2m2m,
];

/// Whether the worker should try decoding on the GPU with this API first.
pub fn hw_decode_for(api: HwApi) -> bool {
    matches!(
        api,
        HwApi::Nvenc | HwApi::Qsv | HwApi::Vaapi | HwApi::VideoToolbox
    )
}

/// Cores this process may use: logical cores, lowered by a container CPU
/// limit (rounded up, so 1.5 CPUs counts as 2). At least 1.
pub fn effective_cores(cpu: &CpuInfo) -> u32 {
    let logical = cpu.logical_cores.max(1);
    match cpu.cgroup_limit {
        Some(limit) if limit.is_finite() && limit > 0.0 => {
            // Saturating float-to-int conversion; the limit is positive here.
            let limit = limit.ceil().min(f64::from(u32::MAX)) as u32;
            logical.min(limit).max(1)
        }
        _ => logical,
    }
}

/// Memory encodes may use: the container limit when set (and smaller than
/// the machine), else total memory. `None` when unknown.
pub fn memory_budget(memory: &MemoryInfo) -> Option<u64> {
    let total = (memory.total_bytes > 0).then_some(memory.total_bytes);
    match (memory.cgroup_limit_bytes, total) {
        (Some(limit), Some(total)) => Some(limit.min(total)),
        (Some(limit), None) => Some(limit),
        (None, total) => total,
    }
}

/// "8 GB", "1.5 GB".
pub(crate) fn format_gb(bytes: u64) -> String {
    let gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
    if gb >= 10.0 || (gb - gb.round()).abs() < 0.05 {
        format!("{gb:.0} GB")
    } else {
        format!("{gb:.1} GB")
    }
}

fn plural(n: u32, one: &str, many: &str) -> String {
    if n == 1 {
        format!("{n} {one}")
    } else {
        format!("{n} {many}")
    }
}

/// "1 CPU core", "2.5 CPU cores", "6 CPU cores".
fn cpu_limit_text(limit: f64) -> String {
    if (limit - 1.0).abs() < 0.05 {
        "1 CPU core".to_string()
    } else if (limit - limit.round()).abs() < 0.05 {
        format!("{limit:.0} CPU cores")
    } else {
        format!("{limit:.1} CPU cores")
    }
}

/// Who a limit applies to: the container, or (a cgroup limit outside a
/// container, e.g. a systemd service) Chrysopoeia.
fn limited_subject(hw: &HardwareInfo) -> &'static str {
    if hw.in_container {
        "the container"
    } else {
        "Chrysopoeia"
    }
}

/// Encodes that fit in memory, at least 1; `None` when memory is unknown.
fn memory_jobs(memory: &MemoryInfo) -> Option<u32> {
    memory_budget(memory).map(|bytes| {
        u32::try_from(bytes / BYTES_PER_JOB)
            .unwrap_or(u32::MAX)
            .max(1)
    })
}

/// "the container is limited to 3 GB of memory, and each encode needs about
/// 1.5 GB" or "this server has 4 GB of memory, ...".
fn memory_reason(hw: &HardwareInfo) -> String {
    let budget = memory_budget(&hw.memory).unwrap_or(0);
    let amount = format_gb(budget);
    let has = if hw.memory.cgroup_limit_bytes == Some(budget) {
        format!("{} is limited to {amount} of memory", limited_subject(hw))
    } else {
        format!("this server has {amount} of memory")
    };
    format!("{has}, and each encode needs about 1.5 GB")
}

/// CPU job count and the reason for it (without the "N at once:" prefix).
fn cpu_plan(hw: &HardwareInfo, cores: u32) -> (u32, String) {
    let limit = hw
        .cpu
        .cgroup_limit
        .filter(|l| l.is_finite() && *l > 0.0 && cores < hw.cpu.logical_cores.max(1));
    let cores_n = plural(cores, "CPU core", "CPU cores");
    let cores_text = match limit {
        Some(limit) => format!(
            "{} is limited to {}",
            limited_subject(hw),
            cpu_limit_text(limit)
        ),
        None => cores_n.clone(),
    };
    let limited = limit.is_some();

    let by_cores = (cores / CORES_PER_CPU_JOB).clamp(1, MAX_CPU_JOBS);
    match memory_jobs(&hw.memory) {
        Some(mem_jobs) if mem_jobs < by_cores => (mem_jobs, memory_reason(hw)),
        _ if cores < CORES_PER_CPU_JOB && limited => (by_cores, cores_text),
        _ if cores < CORES_PER_CPU_JOB => (
            by_cores,
            format!("only {cores_n}, and each encode uses about {CORES_PER_CPU_JOB}"),
        ),
        _ if cores / CORES_PER_CPU_JOB > MAX_CPU_JOBS => (
            by_cores,
            format!(
                "{cores_text}; more than {MAX_CPU_JOBS} encodes at once rarely finishes sooner"
            ),
        ),
        _ => (
            by_cores,
            format!("{cores_text}, about {CORES_PER_CPU_JOB} cores per encode"),
        ),
    }
}

/// Hardware job count (before the core cap) and its reason.
struct GpuPlan {
    jobs: u32,
    reason: String,
}

fn verified(hw: &HardwareInfo, api: HwApi) -> bool {
    hw.encoders.iter().any(|e| e.api == api && e.verified)
}

/// "your NVIDIA GeForce RTX 3060 can run 3 encodes in parallel" or
/// "your 2 NVIDIA GPUs can run 3 encodes each".
fn per_gpu_reason(gpus: &[&GpuDevice], vendor_word: &str, per_gpu: u32) -> String {
    match gpus {
        [one] => format!("your {} can run {per_gpu} encodes in parallel", one.name),
        [] => format!("your {vendor_word} GPU can run {per_gpu} encodes in parallel"),
        many => format!(
            "your {} {vendor_word} GPUs can run {per_gpu} encodes each",
            many.len()
        ),
    }
}

/// GPU jobs for one API family, assuming an encoder of `api` is verified.
/// `allowed` limits which APIs count (a pinned preference allows one).
fn family_plan(hw: &HardwareInfo, api: HwApi, allowed: &[HwApi]) -> GpuPlan {
    let count = |gpus: &[&GpuDevice]| u32::try_from(gpus.len()).unwrap_or(u32::MAX).max(1);
    match api {
        HwApi::Nvenc => {
            let gpus: Vec<&GpuDevice> = hw
                .gpus
                .iter()
                .filter(|g| g.vendor == GpuVendor::Nvidia)
                .collect();
            GpuPlan {
                jobs: count(&gpus).saturating_mul(NVENC_JOBS_PER_GPU),
                reason: per_gpu_reason(&gpus, "NVIDIA", NVENC_JOBS_PER_GPU),
            }
        }
        HwApi::Qsv | HwApi::Vaapi => {
            // Count the GPUs whose render node a verified encoder uses.
            let nodes: BTreeSet<&str> = hw
                .encoders
                .iter()
                .filter(|e| {
                    e.verified
                        && matches!(e.api, HwApi::Qsv | HwApi::Vaapi)
                        && allowed.contains(&e.api)
                })
                .filter_map(|e| e.device.as_deref())
                .collect();
            let gpus: Vec<&GpuDevice> = hw
                .gpus
                .iter()
                .filter(|g| g.render_node.as_deref().is_some_and(|n| nodes.contains(n)))
                .collect();
            let vendor_word = match gpus.first().map(|g| g.vendor) {
                Some(GpuVendor::Amd) => "AMD",
                Some(GpuVendor::Intel) => "Intel",
                _ => "",
            };
            let reason = if gpus.is_empty() {
                format!("your GPU can run {INTEL_AMD_JOBS_PER_GPU} encodes in parallel")
            } else {
                per_gpu_reason(&gpus, vendor_word, INTEL_AMD_JOBS_PER_GPU)
            };
            GpuPlan {
                jobs: count(&gpus).saturating_mul(INTEL_AMD_JOBS_PER_GPU),
                reason,
            }
        }
        HwApi::Amf => {
            let gpus: Vec<&GpuDevice> = hw
                .gpus
                .iter()
                .filter(|g| g.vendor == GpuVendor::Amd)
                .collect();
            GpuPlan {
                jobs: count(&gpus).saturating_mul(INTEL_AMD_JOBS_PER_GPU),
                reason: per_gpu_reason(&gpus, "AMD", INTEL_AMD_JOBS_PER_GPU),
            }
        }
        HwApi::VideoToolbox => GpuPlan {
            jobs: APPLE_JOBS,
            reason: format!("your Mac's media engine can run {APPLE_JOBS} encodes in parallel"),
        },
        HwApi::Rkmpp => GpuPlan {
            jobs: RKMPP_JOBS,
            reason: format!("the Rockchip video engine can run {RKMPP_JOBS} encodes in parallel"),
        },
        HwApi::V4l2m2m => GpuPlan {
            jobs: V4L2_JOBS,
            reason: "the board's hardware encoder runs one encode at a time".to_string(),
        },
        HwApi::Software => GpuPlan {
            jobs: 0,
            reason: String::new(),
        },
    }
}

/// GPU plan for a preference: the pinned API, or the first verified API in
/// [`HW_API_ORDER`] for `Auto` (and for `Cpu`, where it is informational).
fn gpu_plan(hw: &HardwareInfo, preference: HwPreference) -> Option<GpuPlan> {
    let allowed: Vec<HwApi> = match preference.api() {
        Some(api) if api.is_hardware() => vec![api],
        _ => HW_API_ORDER.to_vec(),
    };
    let primary = allowed.iter().copied().find(|api| verified(hw, *api))?;
    Some(family_plan(hw, primary, &allowed))
}

/// Recommend concurrent jobs. See [`crate::recommend_jobs`].
pub fn recommend_jobs(hw: &HardwareInfo, preference: HwPreference) -> JobRecommendation {
    let cores = effective_cores(&hw.cpu);
    let (cpu_jobs, cpu_reason) = cpu_plan(hw, cores);
    let gpu = gpu_plan(hw, preference);
    let mem_cap = memory_jobs(&hw.memory).unwrap_or(u32::MAX);
    let gpu_jobs = gpu
        .as_ref()
        .map_or(0, |g| g.jobs.min(cores).min(mem_cap).max(1));

    let (total, reason) = match (&gpu, preference) {
        (Some(plan), pref) if pref != HwPreference::Cpu => {
            let reason = if mem_cap < plan.jobs && mem_cap <= cores {
                format!(
                    "{gpu_jobs} at once: {}, but {}.",
                    plan.reason,
                    memory_reason(hw)
                )
            } else if plan.jobs > cores {
                let cores_text = plural(cores, "CPU core", "CPU cores");
                let verb = if cores == 1 { "is" } else { "are" };
                format!(
                    "{gpu_jobs} at once: {}, but only {cores_text} {verb} available to feed it.",
                    plan.reason
                )
            } else {
                format!("{gpu_jobs} at once: {}.", plan.reason)
            };
            (gpu_jobs, reason)
        }
        (None, pref) if pref != HwPreference::Cpu && pref != HwPreference::Auto => {
            let label = pref.api().map_or("The chosen hardware", HwApi::label);
            (
                cpu_jobs,
                format!(
                    "{cpu_jobs} at once: {cpu_reason} ({label} isn't working, so encoding uses the CPU)."
                ),
            )
        }
        _ => (cpu_jobs, format!("{cpu_jobs} at once: {cpu_reason}.")),
    };

    JobRecommendation {
        cpu_jobs,
        gpu_jobs,
        total: total.max(1),
        reason,
    }
}

/// Ordered encoders to try. See [`crate::encoder_candidates`].
pub fn encoder_candidates(
    hw: &HardwareInfo,
    codec: VideoCodec,
    preference: HwPreference,
    cpu_fallback: bool,
) -> Vec<EncoderCandidate> {
    let mut candidates = Vec::new();

    if preference != HwPreference::Cpu {
        let pinned = preference.api();
        for api in HW_API_ORDER {
            if pinned.is_some_and(|p| p != api) {
                continue;
            }
            for status in hw
                .encoders
                .iter()
                .filter(|e| e.codec == codec && e.api == api && e.available && e.verified)
            {
                candidates.push(EncoderCandidate {
                    name: status.name.clone(),
                    codec,
                    api,
                    device: status.device.clone(),
                    hw_decode: hw_decode_for(api),
                });
            }
        }
    }

    if preference == HwPreference::Cpu || candidates.is_empty() || cpu_fallback {
        // First software encoder this ffmpeg has, in registry order. With no
        // detection results at all, assume the preferred one is there so the
        // job fails with a clear ffmpeg error rather than "no encoder".
        let software = VIDEO_ENCODERS
            .iter()
            .filter(|e| e.codec == codec && e.api == HwApi::Software)
            .find(|e| {
                if hw.encoders.is_empty() {
                    return true;
                }
                hw.encoders.iter().any(|s| s.name == e.name && s.available)
            });
        if let Some(encoder) = software {
            candidates.push(EncoderCandidate {
                name: encoder.name.to_string(),
                codec,
                api: HwApi::Software,
                device: None,
                hw_decode: false,
            });
        }
    }
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use chrysopoeia_core::{EncoderStatus, FfmpegInfo};

    const GB: u64 = 1024 * 1024 * 1024;

    fn status(name: &str, verified: bool, device: Option<&str>) -> EncoderStatus {
        let info = chrysopoeia_core::encoder::find_encoder(name).unwrap();
        EncoderStatus {
            name: name.into(),
            codec: info.codec,
            api: info.api,
            available: true,
            verified,
            device: device.map(String::from),
            error: (!verified).then(|| "failed".into()),
        }
    }

    fn software() -> Vec<EncoderStatus> {
        [
            "libsvtav1",
            "libaom-av1",
            "libx265",
            "libx264",
            "libvpx-vp9",
        ]
        .iter()
        .map(|n| status(n, true, None))
        .collect()
    }

    fn gpu(vendor: GpuVendor, name: &str, node: Option<&str>) -> GpuDevice {
        GpuDevice {
            vendor,
            name: name.into(),
            render_node: node.map(String::from),
            driver: None,
        }
    }

    fn hw(cores: u32, limit: Option<f64>, mem_gb: u64, mem_limit_gb: Option<u64>) -> HardwareInfo {
        HardwareInfo {
            cpu: CpuInfo {
                model: "Test CPU".into(),
                logical_cores: cores,
                physical_cores: None,
                cgroup_limit: limit,
            },
            memory: MemoryInfo {
                total_bytes: mem_gb * GB,
                available_bytes: mem_gb * GB,
                cgroup_limit_bytes: mem_limit_gb.map(|g| g * GB),
            },
            gpus: Vec::new(),
            encoders: software(),
            audio_encoders: Vec::new(),
            filters: Vec::new(),
            ffmpeg: FfmpegInfo {
                ffmpeg_path: "ffmpeg".into(),
                ffprobe_path: "ffprobe".into(),
                found: true,
                ffprobe_found: true,
                version: None,
            },
            recommended_jobs: JobRecommendation {
                cpu_jobs: 1,
                gpu_jobs: 0,
                total: 1,
                reason: String::new(),
            },
            hints: Vec::new(),
            // Limits come from a container runtime in these cases.
            in_container: limit.is_some() || mem_limit_gb.is_some(),
            detected_at: Utc::now(),
        }
    }

    fn with_nvidia(mut h: HardwareInfo, gpus: usize) -> HardwareInfo {
        for i in 0..gpus {
            h.gpus.push(gpu(
                GpuVendor::Nvidia,
                &format!("NVIDIA GeForce RTX 306{i}"),
                None,
            ));
        }
        h.encoders.push(status("h264_nvenc", true, None));
        h.encoders.push(status("hevc_nvenc", true, None));
        h.encoders.push(status("av1_nvenc", false, None));
        h
    }

    fn with_intel(mut h: HardwareInfo) -> HardwareInfo {
        let node = "/dev/dri/renderD128";
        h.gpus
            .push(gpu(GpuVendor::Intel, "Intel UHD Graphics 770", Some(node)));
        h.encoders.push(status("hevc_qsv", true, Some(node)));
        h.encoders.push(status("hevc_vaapi", true, Some(node)));
        h.encoders.push(status("av1_vaapi", false, Some(node)));
        h
    }

    #[test]
    fn job_recommendations() {
        struct Case {
            name: &'static str,
            hw: HardwareInfo,
            pref: HwPreference,
            cpu: u32,
            gpu: u32,
            total: u32,
            reason: &'static str,
        }
        let cases = [
            Case {
                name: "1-core container",
                hw: hw(16, Some(1.0), 32, None),
                pref: HwPreference::Auto,
                cpu: 1,
                gpu: 0,
                total: 1,
                reason: "1 at once: the container is limited to 1 CPU core.",
            },
            Case {
                name: "16 cores + 8 GB cgroup",
                hw: hw(16, None, 64, Some(8)),
                pref: HwPreference::Auto,
                cpu: 4,
                gpu: 0,
                total: 4,
                reason: "4 at once: 16 CPU cores, about 4 cores per encode.",
            },
            Case {
                name: "32 cores + 4 GB cgroup: memory bound",
                hw: hw(32, None, 64, Some(4)),
                pref: HwPreference::Auto,
                cpu: 2,
                gpu: 0,
                total: 2,
                reason: "2 at once: the container is limited to 4 GB of memory, and each encode needs about 1.5 GB.",
            },
            Case {
                name: "8 cores",
                hw: hw(8, None, 16, None),
                pref: HwPreference::Auto,
                cpu: 2,
                gpu: 0,
                total: 2,
                reason: "2 at once: 8 CPU cores, about 4 cores per encode.",
            },
            Case {
                name: "64 cores caps at 8",
                hw: hw(64, None, 256, None),
                pref: HwPreference::Auto,
                cpu: 8,
                gpu: 0,
                total: 8,
                reason: "8 at once: 64 CPU cores; more than 8 encodes at once rarely finishes sooner.",
            },
            Case {
                name: "fractional cgroup rounds up",
                hw: hw(16, Some(5.5), 32, None),
                pref: HwPreference::Auto,
                cpu: 1,
                gpu: 0,
                total: 1,
                reason: "1 at once: the container is limited to 5.5 CPU cores, about 4 cores per encode.",
            },
            Case {
                name: "2.5 CPUs on a 4-CPU machine",
                hw: hw(4, Some(2.5), 16, None),
                pref: HwPreference::Auto,
                cpu: 1,
                gpu: 0,
                total: 1,
                reason: "1 at once: the container is limited to 2.5 CPU cores.",
            },
            Case {
                name: "3 GB container",
                hw: hw(16, None, 32, Some(3)),
                pref: HwPreference::Auto,
                cpu: 2,
                gpu: 0,
                total: 2,
                reason: "2 at once: the container is limited to 3 GB of memory, and each encode needs about 1.5 GB.",
            },
            Case {
                name: "small server",
                hw: hw(8, None, 1, None),
                pref: HwPreference::Auto,
                cpu: 1,
                gpu: 0,
                total: 1,
                reason: "1 at once: this server has 1 GB of memory, and each encode needs about 1.5 GB.",
            },
            Case {
                name: "NVIDIA in a 1 GB container",
                hw: with_nvidia(hw(16, None, 32, Some(1)), 1),
                pref: HwPreference::Auto,
                cpu: 1,
                gpu: 1,
                total: 1,
                reason: "1 at once: your NVIDIA GeForce RTX 3060 can run 3 encodes in parallel, but the container is \
                         limited to 1 GB of memory, and each encode needs about 1.5 GB.",
            },
            Case {
                name: "NVIDIA in a 4 GB container",
                hw: with_nvidia(hw(16, None, 32, Some(4)), 1),
                pref: HwPreference::Auto,
                cpu: 2,
                gpu: 2,
                total: 2,
                reason: "2 at once: your NVIDIA GeForce RTX 3060 can run 3 encodes in parallel, but the container is \
                         limited to 4 GB of memory, and each encode needs about 1.5 GB.",
            },
            Case {
                name: "one NVIDIA GPU",
                hw: with_nvidia(hw(8, None, 16, None), 1),
                pref: HwPreference::Auto,
                cpu: 2,
                gpu: 3,
                total: 3,
                reason: "3 at once: your NVIDIA GeForce RTX 3060 can run 3 encodes in parallel.",
            },
            Case {
                name: "two NVIDIA GPUs",
                hw: with_nvidia(hw(16, None, 32, None), 2),
                pref: HwPreference::Auto,
                cpu: 4,
                gpu: 6,
                total: 6,
                reason: "6 at once: your 2 NVIDIA GPUs can run 3 encodes each.",
            },
            Case {
                name: "pinned CPU ignores the GPU",
                hw: with_nvidia(hw(16, None, 32, None), 2),
                pref: HwPreference::Cpu,
                cpu: 4,
                gpu: 6,
                total: 4,
                reason: "4 at once: 16 CPU cores, about 4 cores per encode.",
            },
            Case {
                name: "NVIDIA on a 2-core container",
                hw: with_nvidia(hw(16, Some(2.0), 32, None), 1),
                pref: HwPreference::Auto,
                cpu: 1,
                gpu: 2,
                total: 2,
                reason: "2 at once: your NVIDIA GeForce RTX 3060 can run 3 encodes in parallel, but only 2 CPU cores are available to feed it.",
            },
            Case {
                name: "Intel iGPU",
                hw: with_intel(hw(4, None, 8, None)),
                pref: HwPreference::Auto,
                cpu: 1,
                gpu: 2,
                total: 2,
                reason: "2 at once: your Intel UHD Graphics 770 can run 2 encodes in parallel.",
            },
            Case {
                name: "NVIDIA wins over Intel in auto",
                hw: with_intel(with_nvidia(hw(16, None, 32, None), 1)),
                pref: HwPreference::Auto,
                cpu: 4,
                gpu: 3,
                total: 3,
                reason: "3 at once: your NVIDIA GeForce RTX 3060 can run 3 encodes in parallel.",
            },
            Case {
                name: "pinned QSV uses the Intel GPU",
                hw: with_intel(with_nvidia(hw(16, None, 32, None), 1)),
                pref: HwPreference::Qsv,
                cpu: 4,
                gpu: 2,
                total: 2,
                reason: "2 at once: your Intel UHD Graphics 770 can run 2 encodes in parallel.",
            },
            Case {
                name: "pinned NVENC without NVIDIA falls back to CPU",
                hw: with_intel(hw(8, None, 16, None)),
                pref: HwPreference::Nvenc,
                cpu: 2,
                gpu: 0,
                total: 2,
                reason: "2 at once: 8 CPU cores, about 4 cores per encode (NVIDIA NVENC isn't working, so encoding uses the CPU).",
            },
        ];
        for case in cases {
            let rec = recommend_jobs(&case.hw, case.pref);
            assert_eq!(rec.cpu_jobs, case.cpu, "{}: cpu", case.name);
            assert_eq!(rec.gpu_jobs, case.gpu, "{}: gpu", case.name);
            assert_eq!(rec.total, case.total, "{}: total", case.name);
            assert_eq!(rec.reason, case.reason, "{}: reason", case.name);
        }
    }

    #[test]
    fn unknown_memory_does_not_cap() {
        let rec = recommend_jobs(&hw(16, None, 0, None), HwPreference::Auto);
        assert_eq!(rec.cpu_jobs, 4);
    }

    #[test]
    fn candidates_prefer_hardware_in_order() {
        let h = with_intel(with_nvidia(hw(16, None, 32, None), 1));
        let names = |c: Vec<EncoderCandidate>| c.into_iter().map(|c| c.name).collect::<Vec<_>>();

        let auto = encoder_candidates(&h, VideoCodec::Hevc, HwPreference::Auto, true);
        assert_eq!(
            names(auto.clone()),
            ["hevc_nvenc", "hevc_qsv", "hevc_vaapi", "libx265"]
        );
        assert!(auto[0].hw_decode && auto[1].hw_decode && auto[2].hw_decode);
        assert!(!auto[3].hw_decode);
        assert_eq!(auto[0].device, None);
        assert_eq!(auto[1].device.as_deref(), Some("/dev/dri/renderD128"));
        assert_eq!(auto[3].api, HwApi::Software);

        // No CPU fallback: hardware only.
        assert_eq!(
            names(encoder_candidates(
                &h,
                VideoCodec::Hevc,
                HwPreference::Auto,
                false
            )),
            ["hevc_nvenc", "hevc_qsv", "hevc_vaapi"]
        );
        // Pinned API.
        assert_eq!(
            names(encoder_candidates(
                &h,
                VideoCodec::Hevc,
                HwPreference::Vaapi,
                false
            )),
            ["hevc_vaapi"]
        );
        // Pinned CPU.
        assert_eq!(
            names(encoder_candidates(
                &h,
                VideoCodec::Hevc,
                HwPreference::Cpu,
                false
            )),
            ["libx265"]
        );
        // AV1 hardware failed everywhere: software even without fallback,
        // SVT-AV1 before libaom.
        assert_eq!(
            names(encoder_candidates(
                &h,
                VideoCodec::Av1,
                HwPreference::Auto,
                false
            )),
            ["libsvtav1"]
        );
        // Pinned API that can't do this codec: software.
        assert_eq!(
            names(encoder_candidates(
                &h,
                VideoCodec::Vp9,
                HwPreference::Nvenc,
                false
            )),
            ["libvpx-vp9"]
        );
        // Pinned AMF with no AMF encoders at all.
        assert_eq!(
            names(encoder_candidates(
                &h,
                VideoCodec::H264,
                HwPreference::Amf,
                false
            )),
            ["libx264"]
        );
    }

    #[test]
    fn candidates_skip_missing_software_encoders() {
        let mut h = hw(8, None, 16, None);
        for e in h.encoders.iter_mut().filter(|e| e.name == "libsvtav1") {
            e.available = false;
            e.verified = false;
        }
        let c = encoder_candidates(&h, VideoCodec::Av1, HwPreference::Auto, true);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "libaom-av1");

        // Nothing for AV1 at all: empty.
        for e in h.encoders.iter_mut().filter(|e| e.codec == VideoCodec::Av1) {
            e.available = false;
            e.verified = false;
        }
        assert!(encoder_candidates(&h, VideoCodec::Av1, HwPreference::Auto, true).is_empty());

        // Detection never ran: assume the preferred software encoder.
        h.encoders.clear();
        let c = encoder_candidates(&h, VideoCodec::Av1, HwPreference::Auto, false);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "libsvtav1");
    }

    #[test]
    fn formats_cpu_limits() {
        assert_eq!(cpu_limit_text(1.0), "1 CPU core");
        assert_eq!(cpu_limit_text(2.0), "2 CPU cores");
        assert_eq!(cpu_limit_text(2.5), "2.5 CPU cores");
        assert_eq!(cpu_limit_text(0.5), "0.5 CPU cores");
    }

    #[test]
    fn formats_memory() {
        assert_eq!(format_gb(8 * GB), "8 GB");
        assert_eq!(format_gb(3 * GB / 2), "1.5 GB");
        assert_eq!(format_gb(64 * GB), "64 GB");
    }

    #[test]
    fn effective_cores_honours_limits() {
        let cpu = |logical, limit| CpuInfo {
            model: String::new(),
            logical_cores: logical,
            physical_cores: None,
            cgroup_limit: limit,
        };
        assert_eq!(effective_cores(&cpu(16, None)), 16);
        assert_eq!(effective_cores(&cpu(16, Some(2.0))), 2);
        assert_eq!(effective_cores(&cpu(16, Some(0.5))), 1);
        assert_eq!(effective_cores(&cpu(4, Some(64.0))), 4);
        assert_eq!(effective_cores(&cpu(0, None)), 1);
        assert_eq!(effective_cores(&cpu(8, Some(f64::NAN))), 8);
    }
}
