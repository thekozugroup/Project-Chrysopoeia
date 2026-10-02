//! CPU, memory, container and GPU discovery.
//!
//! Every file lookup is made relative to a system root (`/` in production) so
//! tests can describe a whole machine as a small fake `/proc` + `/sys` + `/dev`
//! tree. External tools (`lspci`, `nvidia-smi`, `sysctl`) are only consulted
//! for the real root, never for a fake one, so tests stay deterministic.
//!
//! GPUs are found from three places, merged by PCI slot:
//! 1. DRM render nodes (`/sys/class/drm/renderD*`), which is what VA-API and
//!    Quick Sync use.
//! 2. PCI display controllers (`/sys/bus/pci/devices/*/class`), which also
//!    finds GPUs whose driver isn't loaded or that aren't passed into the
//!    container, so the hints can say why.
//! 3. NVIDIA's own reporting (`nvidia-smi`, `/proc/driver/nvidia/gpus`,
//!    `/dev/nvidiaN`), which also says which NVIDIA GPUs the container may
//!    use: the host's PCI bus is visible in every container, but the NVIDIA
//!    runtime only exposes the GPUs picked by `NVIDIA_VISIBLE_DEVICES`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrysopoeia_core::{CpuInfo, GpuDevice, GpuVendor, MemoryInfo};

use crate::process::{RunError, run_capture};

/// PCI vendor id of Intel.
pub const PCI_VENDOR_INTEL: u16 = 0x8086;
/// PCI vendor id of AMD (ATI).
pub const PCI_VENDOR_AMD: u16 = 0x1002;
/// PCI vendor id of NVIDIA.
pub const PCI_VENDOR_NVIDIA: u16 = 0x10de;

/// cgroup memory limits at or above this mean "unlimited".
const UNLIMITED_BYTES: u64 = 1 << 60;

/// How long `lspci`, `nvidia-smi` and `sysctl` may take.
const TOOL_TIMEOUT: Duration = Duration::from_secs(5);

/// PCI drivers that reserve a device for a virtual machine. Such GPUs are
/// not usable by the host (or its containers) and are not reported.
const VM_PASSTHROUGH_DRIVERS: &[&str] = &["vfio-pci", "pci-stub"];

/// `EPERM`: returned by `open` when the container's device rules (the
/// devices cgroup) forbid a device file, whatever its file permissions.
const EPERM: i32 = 1;

/// The operating system and CPU family Chrysopoeia runs on.
///
/// A plain value rather than scattered `cfg!` checks, so tests can exercise
/// the decisions made for every platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    /// Running on Linux.
    pub linux: bool,
    /// Running on macOS.
    pub macos: bool,
    /// 32- or 64-bit ARM (Raspberry Pi, Rockchip boards, Apple silicon).
    pub arm: bool,
}

impl Platform {
    /// The platform this binary was built for.
    pub fn current() -> Self {
        Self {
            linux: cfg!(target_os = "linux"),
            macos: cfg!(target_os = "macos"),
            arm: cfg!(any(target_arch = "aarch64", target_arch = "arm")),
        }
    }
}

/// Whether this process can use a GPU render node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeAccess {
    /// The device opened for reading and writing.
    Ok,
    /// The GPU is listed in sysfs but the device file doesn't exist, which in
    /// a container means `/dev/dri` wasn't passed in.
    Missing,
    /// The device exists but its file permissions don't allow this process
    /// to open it (`EACCES`): a group problem.
    PermissionDenied,
    /// The device exists and its file permissions would allow it, but the
    /// system still refuses to open it. In Docker this means `/dev/dri` was
    /// mounted as a folder instead of being passed with `--device`, so the
    /// container's device rules forbid it (`EPERM`).
    Blocked,
    /// Any other error, with the system's message.
    Failed(String),
}

/// A DRM render node and whether it can be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderNode {
    /// Logical path such as `/dev/dri/renderD128`, never joined with the
    /// system root.
    pub path: String,
    /// Whether this process can open it.
    pub access: NodeAccess,
    /// Group owning the device file, when it exists. Used for the
    /// `--group-add` hint.
    pub gid: Option<u32>,
}

/// A GPU found on this machine, with the details hints need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedGpu {
    /// Who made it.
    pub vendor: GpuVendor,
    /// Marketing name from `lspci`, `nvidia-smi` or the NVIDIA driver.
    pub name: Option<String>,
    /// Fallback name from sysfs (`product_name` or a GPU-looking `label`).
    pub sysfs_name: Option<String>,
    /// PCI address such as `0000:00:02.0`.
    pub pci_slot: Option<String>,
    /// PCI device id.
    pub device_id: Option<u16>,
    /// Kernel driver bound to the device (`i915`, `amdgpu`, `nvidia`...).
    pub driver: Option<String>,
    /// DRM render node, when the kernel created one.
    pub render_node: Option<RenderNode>,
    /// An NVIDIA GPU on the host that isn't given to this container (the
    /// NVIDIA runtime exposes other GPUs, or it isn't bound to the NVIDIA
    /// driver). Kept for hints; not reported or counted for jobs.
    pub hidden: bool,
}

impl DetectedGpu {
    fn new(vendor: GpuVendor) -> Self {
        Self {
            vendor,
            name: None,
            sysfs_name: None,
            pci_slot: None,
            device_id: None,
            driver: None,
            render_node: None,
            hidden: false,
        }
    }

    /// Placeholder for an NVIDIA GPU known only from `/dev/nvidiaN`.
    fn is_nvidia_placeholder(&self) -> bool {
        self.vendor == GpuVendor::Nvidia && self.pci_slot.is_none() && self.name.is_none()
    }

    /// Name shown to the user: the marketing name when known, else the
    /// vendor plus the render node, e.g. "Intel GPU (renderD128)".
    pub fn display_name(&self) -> String {
        if let Some(name) = self.name.as_deref().or(self.sysfs_name.as_deref()) {
            return name.to_string();
        }
        let vendor = vendor_label(self.vendor);
        let base = if vendor.is_empty() {
            "GPU".to_string()
        } else {
            format!("{vendor} GPU")
        };
        match self
            .render_node
            .as_ref()
            .and_then(|n| n.path.rsplit('/').next())
        {
            Some(node) if self.vendor != GpuVendor::Nvidia => format!("{base} ({node})"),
            _ => base,
        }
    }

    /// Render node path when this process can open it.
    pub fn usable_node(&self) -> Option<&str> {
        self.render_node
            .as_ref()
            .filter(|n| n.access == NodeAccess::Ok)
            .map(|n| n.path.as_str())
    }

    /// The public, serializable description.
    pub fn to_device(&self) -> GpuDevice {
        GpuDevice {
            vendor: self.vendor,
            name: self.display_name(),
            // NVIDIA encoding goes through CUDA, never the render node.
            render_node: match self.vendor {
                GpuVendor::Nvidia => None,
                _ => self.render_node.as_ref().map(|n| n.path.clone()),
            },
            driver: self.driver.clone(),
        }
    }
}

/// Everything found about the machine, before ffmpeg is consulted.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Devices {
    /// CPU model, core counts and container CPU limit.
    pub cpu: CpuInfo,
    /// Memory size and container memory limit.
    pub memory: MemoryInfo,
    /// Running inside a container (Docker, Podman, LXC, Kubernetes).
    pub in_container: bool,
    /// `/dev/dri` exists.
    pub dri_present: bool,
    /// Number of `/dev/nvidiaN` device files (present when the NVIDIA
    /// container runtime or a native driver exposes GPUs).
    pub nvidia_device_nodes: u32,
    /// A Rockchip video engine exists on this board (its sysfs entry or a
    /// Rockchip device tree), whether or not it is passed in.
    pub rockchip_vpu: bool,
    /// `/dev/mpp_service` exists, so Rockchip MPP encoders can be tried.
    pub mpp_present: bool,
    /// `/sys/bus/pci/devices` lists devices, so a machine without GPUs in
    /// it really has none (rather than the PCI bus being hidden).
    pub pci_listed: bool,
    /// Every GPU found, in discovery order, including NVIDIA GPUs that are
    /// [`DetectedGpu::hidden`] from this container.
    pub gpus: Vec<DetectedGpu>,
}

impl Devices {
    /// The GPUs this process can use, in their public form.
    pub fn gpu_devices(&self) -> Vec<GpuDevice> {
        self.gpus
            .iter()
            .filter(|g| !g.hidden)
            .map(DetectedGpu::to_device)
            .collect()
    }

    /// Any GPU from `vendor`.
    pub fn has_vendor(&self, vendor: GpuVendor) -> bool {
        self.gpus.iter().any(|g| g.vendor == vendor)
    }

    /// GPUs from `vendor`, in discovery order.
    pub fn gpus_from(&self, vendor: GpuVendor) -> impl Iterator<Item = &DetectedGpu> {
        self.gpus.iter().filter(move |g| g.vendor == vendor)
    }

    /// An NVIDIA GPU is known, or NVIDIA device files exist.
    pub fn nvidia_visible(&self) -> bool {
        self.nvidia_device_nodes > 0 || self.has_vendor(GpuVendor::Nvidia)
    }

    /// When nothing better is known about NVIDIA GPUs, report one generic
    /// "NVIDIA GPU" per `/dev/nvidiaN`.
    fn add_nvidia_placeholders(&mut self) {
        if self.nvidia_device_nodes > 0 && !self.has_vendor(GpuVendor::Nvidia) {
            for _ in 0..self.nvidia_device_nodes {
                let mut gpu = DetectedGpu::new(GpuVendor::Nvidia);
                gpu.driver = Some("nvidia".into());
                self.gpus.push(gpu);
            }
        }
    }
}

/// Short vendor name used in GPU names ("Intel", "AMD", "NVIDIA", "Apple").
pub fn vendor_label(vendor: GpuVendor) -> &'static str {
    match vendor {
        GpuVendor::Intel => "Intel",
        GpuVendor::Amd => "AMD",
        GpuVendor::Nvidia => "NVIDIA",
        GpuVendor::Apple => "Apple",
        GpuVendor::Other => "",
    }
}

/// Map a PCI vendor id to a GPU vendor.
pub fn vendor_from_pci_id(id: u16) -> GpuVendor {
    match id {
        PCI_VENDOR_INTEL => GpuVendor::Intel,
        PCI_VENDOR_AMD => GpuVendor::Amd,
        PCI_VENDOR_NVIDIA => GpuVendor::Nvidia,
        _ => GpuVendor::Other,
    }
}

/// Whether `root` is the real filesystem root, where external tools describe
/// the same machine as the files.
pub fn is_real_root(root: &Path) -> bool {
    root == Path::new("/")
}

// ---------------------------------------------------------------------------
// Entry points
// ---------------------------------------------------------------------------

/// Scan the machine. File reads run on the blocking pool; external tools run
/// only when `use_system_tools` is true (the real root).
pub async fn scan(root: &Path, use_system_tools: bool, platform: Platform) -> Devices {
    let root_owned = root.to_path_buf();
    let scanned = tokio::task::spawn_blocking(move || {
        // Only a fallback for systems without `/proc` (macOS): on Linux it
        // already rounds a container CPU limit down, which `recommend`
        // applies itself (rounded up).
        let fallback_logical = if use_system_tools {
            std::thread::available_parallelism()
                .ok()
                .and_then(|n| u32::try_from(n.get()).ok())
        } else {
            None
        };
        scan_system(&root_owned, fallback_logical)
    })
    .await;
    let mut devices = match scanned {
        Ok(devices) => devices,
        Err(err) => {
            tracing::warn!(error = %err, "hardware scan stopped unexpectedly");
            Devices {
                cpu: CpuInfo {
                    model: "Unknown CPU".into(),
                    logical_cores: 1,
                    ..CpuInfo::default()
                },
                ..Devices::default()
            }
        }
    };

    if use_system_tools {
        if platform.macos {
            apply_macos_details(&mut devices).await;
        }
        if platform.linux {
            if devices.nvidia_visible() {
                merge_nvidia_smi(&mut devices).await;
            }
            name_gpus_with_lspci(&mut devices).await;
        }
    }
    devices
}

/// Read everything that comes from files under `root`. Blocking; call it
/// from `spawn_blocking`.
///
/// `fallback_logical` (`available_parallelism` in production) is only used
/// when `root` doesn't say how many CPUs there are; see [`read_cpu`].
pub fn scan_system(root: &Path, fallback_logical: Option<u32>) -> Devices {
    let status = read_proc_status(root);
    let mut gpus = scan_drm(root, &status);
    let pci_listed = scan_pci(root, &mut gpus);
    let proc_nvidia = merge_nvidia_proc(root, &mut gpus);
    let minors = nvidia_minors(root);
    if let Some(visible) = visible_nvidia_slots(&gpus, &proc_nvidia, &minors) {
        apply_nvidia_visibility(&mut gpus, &visible);
    }
    let mut devices = Devices {
        cpu: read_cpu(root, fallback_logical),
        memory: read_memory(root),
        in_container: detect_container(root),
        dri_present: root.join("dev/dri").is_dir(),
        nvidia_device_nodes: u32::try_from(minors.len()).unwrap_or(u32::MAX),
        rockchip_vpu: detect_rockchip_vpu(root),
        mpp_present: root.join("dev/mpp_service").exists(),
        pci_listed,
        gpus,
    };
    devices.add_nvidia_placeholders();
    devices
}

// ---------------------------------------------------------------------------
// Small file helpers
// ---------------------------------------------------------------------------

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Trimmed file contents; `None` when missing or empty. Device-tree strings
/// end with a NUL, which is trimmed too.
fn read_trimmed(path: &Path) -> Option<String> {
    let text = read_text(path)?;
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\0');
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Parse `0x8086`-style hex ids.
fn read_hex_u16(path: &Path) -> Option<u16> {
    let text = read_trimmed(path)?;
    let digits = text.strip_prefix("0x").unwrap_or(&text);
    u16::from_str_radix(digits, 16).ok()
}

/// Basename of a sysfs device's `driver` symlink.
fn driver_name(device_dir: &Path) -> Option<String> {
    let target = fs::read_link(device_dir.join("driver")).ok()?;
    target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
}

/// Directory entry names under `dir`, sorted. Empty when unreadable.
fn sorted_entries(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    out.sort_by_key(|e| natural_key(&e.0));
    out
}

/// Sort key that orders `renderD129` before `renderD1000`.
fn natural_key(name: &str) -> (String, u64) {
    let digits_at = name
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let (prefix, digits) = name.split_at(digits_at);
    (prefix.to_string(), digits.parse().unwrap_or(0))
}

// ---------------------------------------------------------------------------
// CPU
// ---------------------------------------------------------------------------

/// Fields of interest from `/proc/cpuinfo`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CpuInfoText {
    /// `model name` (x86 and some ARM kernels).
    pub model_name: Option<String>,
    /// `Model` (Raspberry Pi and other boards).
    pub board_model: Option<String>,
    /// `Hardware` (older ARM kernels; often the SoC name).
    pub hardware: Option<String>,
    /// `Processor` (old 32-bit ARM kernels).
    pub processor_name: Option<String>,
    /// `CPU implementer` of the first processor (ARM).
    pub arm_implementer: Option<u32>,
    /// `CPU part` of the first processor (ARM).
    pub arm_part: Option<u32>,
    /// Number of `processor` entries.
    pub processors: u32,
    /// Distinct (`physical id`, `core id`) pairs, when listed.
    pub physical_cores: Option<u32>,
}

fn parse_hex_u32(text: &str) -> Option<u32> {
    let t = text.trim();
    let digits = t.strip_prefix("0x").unwrap_or(t);
    u32::from_str_radix(digits, 16).ok()
}

/// Parse `/proc/cpuinfo`. Pure, for tests.
pub fn parse_cpuinfo(text: &str) -> CpuInfoText {
    let mut info = CpuInfoText::default();
    let mut cores: BTreeSet<(String, String)> = BTreeSet::new();
    let mut physical_id: Option<String> = None;
    let mut core_id: Option<String> = None;

    let mut flush = |physical_id: &mut Option<String>, core_id: &mut Option<String>| {
        if let (Some(p), Some(c)) = (physical_id.take(), core_id.take()) {
            cores.insert((p, c));
        }
    };

    for line in text.lines() {
        if line.trim().is_empty() {
            flush(&mut physical_id, &mut core_id);
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let set = |slot: &mut Option<String>| {
            if slot.is_none() && !value.is_empty() {
                *slot = Some(value.to_string());
            }
        };
        match key {
            "processor" => {
                // A new block; count it. (Old ARM kernels also use a
                // "Processor" key for the name, which is matched below.)
                info.processors += 1;
            }
            "model name" => set(&mut info.model_name),
            "Model" => set(&mut info.board_model),
            "Hardware" => set(&mut info.hardware),
            "Processor" => set(&mut info.processor_name),
            "CPU implementer" if info.arm_implementer.is_none() => {
                info.arm_implementer = parse_hex_u32(value);
            }
            "CPU part" if info.arm_part.is_none() => info.arm_part = parse_hex_u32(value),
            "physical id" => physical_id = Some(value.to_string()),
            "core id" => core_id = Some(value.to_string()),
            _ => {}
        }
    }
    flush(&mut physical_id, &mut core_id);
    if !cores.is_empty() {
        info.physical_cores = u32::try_from(cores.len()).ok();
    }
    info
}

/// Name of a common ARM Ltd. core from its `CPU part` number.
fn arm_core_name(implementer: Option<u32>, part: u32) -> String {
    let name = match (implementer, part) {
        (Some(0x41) | None, 0xd03) => Some("Cortex-A53"),
        (Some(0x41) | None, 0xd04) => Some("Cortex-A35"),
        (Some(0x41) | None, 0xd05) => Some("Cortex-A55"),
        (Some(0x41) | None, 0xd07) => Some("Cortex-A57"),
        (Some(0x41) | None, 0xd08) => Some("Cortex-A72"),
        (Some(0x41) | None, 0xd09) => Some("Cortex-A73"),
        (Some(0x41) | None, 0xd0a) => Some("Cortex-A75"),
        (Some(0x41) | None, 0xd0b) => Some("Cortex-A76"),
        (Some(0x41) | None, 0xd0c) => Some("Neoverse N1"),
        (Some(0x41) | None, 0xd0d) => Some("Cortex-A77"),
        (Some(0x41) | None, 0xd40) => Some("Neoverse V1"),
        (Some(0x41) | None, 0xd41) => Some("Cortex-A78"),
        (Some(0x41) | None, 0xd49) => Some("Neoverse N2"),
        (Some(0x41) | None, 0xd4f) => Some("Neoverse V2"),
        _ => None,
    };
    match name {
        Some(n) => format!("ARM {n}"),
        None => format!("ARM CPU (part {part:#x})"),
    }
}

/// Count CPUs in a kernel list such as `0-3,6,8-9`.
pub fn count_cpu_list(text: &str) -> Option<u32> {
    let mut count = 0u32;
    for part in text.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (u32, u32) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
                count = count.checked_add(b.checked_sub(a)?.checked_add(1)?)?;
            }
            None => {
                part.trim().parse::<u32>().ok()?;
                count = count.checked_add(1)?;
            }
        }
    }
    (count > 0).then_some(count)
}

/// Fields of interest from `/proc/self/status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProcStatus {
    /// Effective user id.
    pub uid: Option<u32>,
    /// Effective group id followed by the supplementary groups.
    pub gids: Vec<u32>,
    /// CPUs this process may run on (`Cpus_allowed_list`), which reflects
    /// `--cpuset-cpus` and CPU affinity but not a CPU quota.
    pub cpus_allowed: Option<u32>,
}

/// Parse `/proc/self/status`. Pure, for tests.
pub fn parse_proc_status(text: &str) -> ProcStatus {
    let mut status = ProcStatus::default();
    let mut groups = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        // `Uid:` and `Gid:` list real, effective, saved and filesystem ids.
        let effective = || value.split_whitespace().nth(1)?.parse::<u32>().ok();
        match key.trim() {
            "Uid" => status.uid = effective(),
            "Gid" => {
                if let Some(gid) = effective() {
                    status.gids.insert(0, gid);
                }
            }
            "Groups" => {
                groups = value
                    .split_whitespace()
                    .filter_map(|g| g.parse::<u32>().ok())
                    .collect();
            }
            "Cpus_allowed_list" => status.cpus_allowed = count_cpu_list(value),
            _ => {}
        }
    }
    for gid in groups {
        if !status.gids.contains(&gid) {
            status.gids.push(gid);
        }
    }
    status
}

fn read_proc_status(root: &Path) -> ProcStatus {
    read_text(&root.join("proc/self/status"))
        .as_deref()
        .map(parse_proc_status)
        .unwrap_or_default()
}

/// Read CPU details under `root`.
///
/// Logical cores are the CPUs this process may run on
/// (`Cpus_allowed_list`), else the processors in `proc/cpuinfo`, else the
/// online CPUs, else `fallback_logical`. A container CPU quota is reported
/// separately in `cgroup_limit`, not subtracted here.
pub fn read_cpu(root: &Path, fallback_logical: Option<u32>) -> CpuInfo {
    let parsed = read_text(&root.join("proc/cpuinfo"))
        .as_deref()
        .map(parse_cpuinfo)
        .unwrap_or_default();

    let device_tree_model = || {
        read_trimmed(&root.join("proc/device-tree/model"))
            .or_else(|| read_trimmed(&root.join("sys/firmware/devicetree/base/model")))
    };
    let model = parsed
        .model_name
        .clone()
        .or_else(|| parsed.board_model.clone())
        .or_else(device_tree_model)
        .or_else(|| parsed.hardware.clone())
        .or_else(|| parsed.processor_name.clone())
        .or_else(|| {
            parsed
                .arm_part
                .map(|part| arm_core_name(parsed.arm_implementer, part))
        })
        .unwrap_or_else(|| "Unknown CPU".to_string());

    let logical = read_proc_status(root)
        .cpus_allowed
        .or((parsed.processors > 0).then_some(parsed.processors))
        .or_else(|| {
            read_text(&root.join("sys/devices/system/cpu/online"))
                .as_deref()
                .and_then(count_cpu_list)
        })
        .or(fallback_logical.filter(|n| *n > 0))
        .unwrap_or(1);

    CpuInfo {
        model,
        logical_cores: logical,
        // `cpuinfo` describes the whole host; with `--cpuset-cpus` fewer
        // cores are usable, and never more physical than logical ones.
        physical_cores: parsed.physical_cores.map(|p| p.min(logical)),
        cgroup_limit: cgroup_cpu_limit(root),
    }
}

// ---------------------------------------------------------------------------
// cgroups
// ---------------------------------------------------------------------------

/// One line of `/proc/self/cgroup`: `hierarchy:controllers:path`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CgroupEntry {
    hierarchy: String,
    controllers: Vec<String>,
    path: String,
}

fn parse_proc_cgroup(text: &str) -> Vec<CgroupEntry> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let hierarchy = parts.next()?.to_string();
            let controllers = parts
                .next()?
                .split(',')
                .filter(|c| !c.is_empty())
                .map(String::from)
                .collect();
            let path = parts.next()?.trim().to_string();
            Some(CgroupEntry {
                hierarchy,
                controllers,
                path,
            })
        })
        .collect()
}

/// This process's cgroup memberships under `root`.
fn own_cgroups(root: &Path) -> Vec<CgroupEntry> {
    read_text(&root.join("proc/self/cgroup"))
        .as_deref()
        .map(parse_proc_cgroup)
        .unwrap_or_default()
}

fn v2_path(entries: &[CgroupEntry]) -> &str {
    entries
        .iter()
        .find(|e| e.hierarchy == "0" && e.controllers.is_empty())
        .map_or("/", |e| e.path.as_str())
}

fn v1_path<'a>(entries: &'a [CgroupEntry], controller: &str) -> &'a str {
    entries
        .iter()
        .find(|e| e.controllers.iter().any(|c| c == controller))
        .map_or("/", |e| e.path.as_str())
}

/// `base/rel`, then each parent up to `base`. Limits anywhere on the way
/// apply, so callers take the smallest. Inside a container the cgroup is
/// usually mounted at `base` itself, which is always included.
fn cgroup_dirs(base: &Path, rel: &str) -> Vec<PathBuf> {
    let parts: Vec<&str> = rel
        .split('/')
        .filter(|c| !c.is_empty() && *c != "." && *c != "..")
        .collect();
    (0..=parts.len())
        .rev()
        .map(|n| {
            let mut dir = base.to_path_buf();
            dir.extend(&parts[..n]);
            dir
        })
        .collect()
}

/// Parse cgroup v2 `cpu.max` (`"max 100000"` or `"200000 100000"`) into cores.
pub fn parse_cpu_max(text: &str) -> Option<f64> {
    let mut parts = text.split_whitespace();
    let quota = parts.next()?;
    if quota == "max" {
        return None;
    }
    let quota: f64 = quota.parse().ok()?;
    let period: f64 = parts.next().map_or(Some(100_000.0), |p| p.parse().ok())?;
    (quota > 0.0 && period > 0.0).then(|| quota / period)
}

/// Parse cgroup v1 `cpu.cfs_quota_us` / `cpu.cfs_period_us` into cores.
pub fn parse_cfs_quota(quota: &str, period: &str) -> Option<f64> {
    let quota: f64 = quota.trim().parse().ok()?;
    let period: f64 = period.trim().parse().ok()?;
    (quota > 0.0 && period > 0.0).then(|| quota / period)
}

/// CPU quota in cores from cgroup v2 or v1, whichever is set and smallest.
pub fn cgroup_cpu_limit(root: &Path) -> Option<f64> {
    let entries = own_cgroups(root);
    let mut limits = Vec::new();

    let v2_base = root.join("sys/fs/cgroup");
    for dir in cgroup_dirs(&v2_base, v2_path(&entries)) {
        if let Some(limit) = read_text(&dir.join("cpu.max"))
            .as_deref()
            .and_then(parse_cpu_max)
        {
            limits.push(limit);
        }
    }

    let rel = v1_path(&entries, "cpu");
    for mount in ["cpu", "cpu,cpuacct", "cpuacct,cpu"] {
        for dir in cgroup_dirs(&v2_base.join(mount), rel) {
            let quota = read_text(&dir.join("cpu.cfs_quota_us"));
            let period = read_text(&dir.join("cpu.cfs_period_us"));
            if let (Some(q), Some(p)) = (quota, period)
                && let Some(limit) = parse_cfs_quota(&q, &p)
            {
                limits.push(limit);
            }
        }
    }
    limits.into_iter().reduce(f64::min)
}

/// Parse a cgroup memory limit (`memory.max` or `memory.limit_in_bytes`).
/// `max`, zero and values of 2^60 or more mean "no limit".
pub fn parse_memory_limit(text: &str) -> Option<u64> {
    let text = text.trim();
    if text == "max" {
        return None;
    }
    text.parse::<u64>()
        .ok()
        .filter(|v| *v > 0 && *v < UNLIMITED_BYTES)
}

/// Smallest cgroup memory limit and the usage of that cgroup.
fn cgroup_memory(root: &Path) -> (Option<u64>, Option<u64>) {
    let entries = own_cgroups(root);
    let base = root.join("sys/fs/cgroup");
    let mut best: Option<(u64, Option<u64>)> = None;
    let mut consider = |limit: Option<u64>, usage: Option<u64>| {
        if let Some(limit) = limit
            && best.is_none_or(|(b, _)| limit < b)
        {
            best = Some((limit, usage));
        }
    };

    for dir in cgroup_dirs(&base, v2_path(&entries)) {
        let limit = read_text(&dir.join("memory.max"))
            .as_deref()
            .and_then(parse_memory_limit);
        let usage = read_trimmed(&dir.join("memory.current")).and_then(|u| u.parse().ok());
        consider(limit, usage);
    }
    for dir in cgroup_dirs(&base.join("memory"), v1_path(&entries, "memory")) {
        let limit = read_text(&dir.join("memory.limit_in_bytes"))
            .as_deref()
            .and_then(parse_memory_limit);
        let usage = read_trimmed(&dir.join("memory.usage_in_bytes")).and_then(|u| u.parse().ok());
        consider(limit, usage);
    }
    match best {
        Some((limit, usage)) => (Some(limit), usage),
        None => (None, None),
    }
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

/// `MemTotal` and `MemAvailable` (falling back to `MemFree`) in bytes.
pub fn parse_meminfo(text: &str) -> (Option<u64>, Option<u64>) {
    let field = |name: &str| {
        text.lines().find_map(|line| {
            let rest = line.strip_prefix(name)?.strip_prefix(':')?;
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            kb.checked_mul(1024)
        })
    };
    let total = field("MemTotal");
    let available = field("MemAvailable").or_else(|| field("MemFree"));
    (total, available)
}

/// Read memory details under `root`. Available memory honours the cgroup
/// limit: a container limited to 8 GB never has more than that available.
pub fn read_memory(root: &Path) -> MemoryInfo {
    let (total, available) = read_text(&root.join("proc/meminfo"))
        .as_deref()
        .map(parse_meminfo)
        .unwrap_or((None, None));
    let (limit, usage) = cgroup_memory(root);
    let total_bytes = total.unwrap_or(0);
    let cgroup_room = limit.map(|l| l.saturating_sub(usage.unwrap_or(0)));
    let available_bytes = match (available, cgroup_room) {
        (Some(a), Some(r)) => a.min(r),
        (Some(a), None) => a,
        (None, Some(r)) => r,
        (None, None) => total_bytes,
    };
    MemoryInfo {
        total_bytes,
        available_bytes,
        cgroup_limit_bytes: limit,
    }
}

// ---------------------------------------------------------------------------
// Container
// ---------------------------------------------------------------------------

/// Whether a cgroup listing mentions a container runtime.
pub fn cgroup_mentions_container(text: &str) -> bool {
    [
        "docker",
        "containerd",
        "kubepods",
        "lxc",
        "libpod",
        "podman",
    ]
    .iter()
    .any(|k| text.contains(k))
}

/// Whether this process runs inside a container.
pub fn detect_container(root: &Path) -> bool {
    if root.join(".dockerenv").exists() || root.join("run/.containerenv").exists() {
        return true;
    }
    if read_text(&root.join("proc/1/cgroup")).is_some_and(|t| cgroup_mentions_container(&t)) {
        return true;
    }
    // Podman, LXC and systemd-nspawn set `container=` for PID 1.
    fs::read(root.join("proc/1/environ")).is_ok_and(|env| {
        env.split(|b| *b == 0)
            .any(|kv| kv.starts_with(b"container="))
    })
}

// ---------------------------------------------------------------------------
// GPUs
// ---------------------------------------------------------------------------

/// Normalize a PCI address to `dddd:bb:dd.f` in lowercase. Accepts the
/// 8-digit domain `nvidia-smi` prints and addresses without a domain.
pub fn normalize_pci_slot(text: &str) -> Option<String> {
    let text = text.trim().to_ascii_lowercase();
    let parts: Vec<&str> = text.split(':').collect();
    let (domain, bus, devfn) = match parts.as_slice() {
        [domain, bus, devfn] => (u32::from_str_radix(domain, 16).ok()?, *bus, *devfn),
        [bus, devfn] => (0, *bus, *devfn),
        _ => return None,
    };
    let (dev, func) = devfn.split_once('.')?;
    let bus = u8::from_str_radix(bus, 16).ok()?;
    let dev = u8::from_str_radix(dev, 16).ok()?;
    let func = u8::from_str_radix(func, 16).ok()?;
    Some(format!("{domain:04x}:{bus:02x}:{dev:02x}.{func:x}"))
}

/// Check whether a device file can be opened for reading and writing by a
/// process with the ids in `status`.
fn check_node(path: &Path, status: &ProcStatus) -> (NodeAccess, Option<u32>) {
    let meta = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) => return (access_from_error(&err), None),
    };
    let gid = file_gid(&meta);
    let access = match fs::OpenOptions::new().read(true).write(true).open(path) {
        Ok(_) => NodeAccess::Ok,
        // Refused although the file permissions allow it: adding a group
        // won't help, so it isn't reported as a group problem.
        Err(err) => match access_from_error(&err) {
            NodeAccess::PermissionDenied if mode_allows_rw(&meta, status) => NodeAccess::Blocked,
            other => other,
        },
    };
    (access, gid)
}

#[cfg(unix)]
fn file_gid(meta: &fs::Metadata) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.gid())
}

#[cfg(not(unix))]
fn file_gid(_meta: &fs::Metadata) -> Option<u32> {
    None
}

/// Whether the file's owner, group and mode let a process with the ids in
/// `status` read and write it. `false` when the ids are unknown.
#[cfg(unix)]
pub fn mode_allows_rw(meta: &fs::Metadata, status: &ProcStatus) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(uid) = status.uid else {
        return false;
    };
    let bits = if uid == meta.uid() {
        0o600
    } else if status.gids.contains(&meta.gid()) {
        0o060
    } else {
        0o006
    };
    meta.mode() & bits == bits
}

/// Whether the file's permissions allow reading and writing (unknown off
/// Unix, so `false`).
#[cfg(not(unix))]
pub fn mode_allows_rw(_meta: &fs::Metadata, _status: &ProcStatus) -> bool {
    false
}

/// Classify an error from opening a device file.
///
/// `EPERM` (the container's device rules) is [`NodeAccess::Blocked`];
/// `EACCES` (file permissions) is [`NodeAccess::PermissionDenied`]. Rust maps
/// both to [`io::ErrorKind::PermissionDenied`], so the raw code decides.
pub fn access_from_error(err: &io::Error) -> NodeAccess {
    if cfg!(unix) && err.raw_os_error() == Some(EPERM) {
        return NodeAccess::Blocked;
    }
    match err.kind() {
        io::ErrorKind::NotFound => NodeAccess::Missing,
        io::ErrorKind::PermissionDenied => NodeAccess::PermissionDenied,
        _ => NodeAccess::Failed(err.to_string()),
    }
}

/// A sysfs name worth showing: `product_name`, or a `label` that looks like a
/// GPU model rather than a slot description such as "Onboard IGD".
fn sysfs_gpu_name(device_dir: &Path) -> Option<String> {
    if let Some(name) = read_trimmed(&device_dir.join("product_name")) {
        return Some(name);
    }
    let label = read_trimmed(&device_dir.join("label"))?;
    let lower = label.to_ascii_lowercase();
    [
        "graphics", "radeon", "geforce", "quadro", "arc ", "iris", "uhd",
    ]
    .iter()
    .any(|w| lower.contains(w))
    .then_some(label)
}

/// GPUs with a DRM render node. `status` holds this process's ids, used to
/// tell permission problems apart.
fn scan_drm(root: &Path, status: &ProcStatus) -> Vec<DetectedGpu> {
    let mut gpus: Vec<DetectedGpu> = Vec::new();
    for (name, class_dir) in sorted_entries(&root.join("sys/class/drm")) {
        if !name.starts_with("renderD") {
            continue;
        }
        let dev = class_dir.join("device");
        let Some(vendor_id) = read_hex_u16(&dev.join("vendor")) else {
            continue;
        };
        let vendor = vendor_from_pci_id(vendor_id);
        if vendor == GpuVendor::Other {
            // Virtual and embedded display GPUs can't encode video.
            continue;
        }
        let pci_slot = fs::canonicalize(&dev)
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .and_then(|n| normalize_pci_slot(&n));
        if pci_slot.is_some() && gpus.iter().any(|g| g.pci_slot == pci_slot) {
            continue;
        }
        let (access, gid) = check_node(&root.join("dev/dri").join(&name), status);
        gpus.push(DetectedGpu {
            vendor,
            name: None,
            sysfs_name: sysfs_gpu_name(&dev),
            pci_slot,
            device_id: read_hex_u16(&dev.join("device")),
            driver: driver_name(&dev),
            render_node: Some(RenderNode {
                path: format!("/dev/dri/{name}"),
                access,
                gid,
            }),
            hidden: false,
        });
    }
    gpus
}

/// Display controllers on the PCI bus that have no render node: GPUs whose
/// driver isn't loaded, or NVIDIA GPUs without the DRM module. Returns
/// whether the PCI bus listed any device at all.
fn scan_pci(root: &Path, gpus: &mut Vec<DetectedGpu>) -> bool {
    let entries = sorted_entries(&root.join("sys/bus/pci/devices"));
    let listed = !entries.is_empty();
    for (name, dir) in entries {
        let Some(slot) = normalize_pci_slot(&name) else {
            continue;
        };
        if gpus
            .iter()
            .any(|g| g.pci_slot.as_deref() == Some(slot.as_str()))
        {
            continue;
        }
        // Class 0x03xxxx: VGA, 3D or other display controller.
        if !read_trimmed(&dir.join("class")).is_some_and(|c| c.starts_with("0x03")) {
            continue;
        }
        let Some(vendor_id) = read_hex_u16(&dir.join("vendor")) else {
            continue;
        };
        let vendor = vendor_from_pci_id(vendor_id);
        if vendor == GpuVendor::Other {
            continue;
        }
        let driver = driver_name(&dir);
        if driver
            .as_deref()
            .is_some_and(|d| VM_PASSTHROUGH_DRIVERS.contains(&d))
        {
            continue;
        }
        gpus.push(DetectedGpu {
            vendor,
            name: None,
            sysfs_name: sysfs_gpu_name(&dir),
            pci_slot: Some(slot),
            device_id: read_hex_u16(&dir.join("device")),
            driver,
            render_node: None,
            hidden: false,
        });
    }
    listed
}

/// The `Model:` line of `/proc/driver/nvidia/gpus/*/information`.
pub fn parse_nvidia_information(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let model = line.strip_prefix("Model:")?.trim();
        (!model.is_empty()).then(|| model.to_string())
    })
}

/// The `Device Minor:` line of the same file: N in `/dev/nvidiaN`.
pub fn parse_nvidia_minor(text: &str) -> Option<u32> {
    text.lines()
        .find_map(|line| line.strip_prefix("Device Minor:")?.trim().parse().ok())
}

/// Prefix "NVIDIA " when a name from the driver lacks it.
fn nvidia_name(name: &str) -> String {
    let name = name.trim();
    if name.to_ascii_lowercase().starts_with("nvidia") {
        name.to_string()
    } else {
        format!("NVIDIA {name}")
    }
}

/// Add or update an NVIDIA GPU identified by PCI slot.
fn merge_nvidia_gpu(gpus: &mut Vec<DetectedGpu>, slot: Option<String>, name: Option<String>) {
    let existing = slot
        .as_deref()
        .and_then(|s| gpus.iter_mut().find(|g| g.pci_slot.as_deref() == Some(s)));
    match existing {
        Some(gpu) => {
            gpu.vendor = GpuVendor::Nvidia;
            if name.is_some() {
                gpu.name = name;
            }
            if gpu.driver.is_none() {
                gpu.driver = Some("nvidia".into());
            }
        }
        None => {
            let mut gpu = DetectedGpu::new(GpuVendor::Nvidia);
            gpu.name = name;
            gpu.pci_slot = slot;
            gpu.driver = Some("nvidia".into());
            gpus.push(gpu);
        }
    }
}

/// One GPU listed by the NVIDIA driver in `/proc/driver/nvidia/gpus`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcNvidiaGpu {
    /// Normalized PCI address (the directory name).
    slot: Option<String>,
    /// N in `/dev/nvidiaN`.
    minor: Option<u32>,
}

/// NVIDIA GPUs reported by the kernel driver in `/proc/driver/nvidia/gpus`.
/// Inside a container the NVIDIA runtime lists only the GPUs it exposes.
fn merge_nvidia_proc(root: &Path, gpus: &mut Vec<DetectedGpu>) -> Vec<ProcNvidiaGpu> {
    let mut listed = Vec::new();
    for (name, dir) in sorted_entries(&root.join("proc/driver/nvidia/gpus")) {
        let information = read_text(&dir.join("information"));
        let model = information
            .as_deref()
            .and_then(parse_nvidia_information)
            .map(|m| nvidia_name(&m));
        let slot = normalize_pci_slot(&name);
        merge_nvidia_gpu(gpus, slot.clone(), model);
        listed.push(ProcNvidiaGpu {
            slot,
            minor: information.as_deref().and_then(parse_nvidia_minor),
        });
    }
    listed
}

/// PCI slots of the NVIDIA GPUs this process can use, or `None` when that
/// can't be told (then every NVIDIA GPU counts).
///
/// The driver's `/proc` listing is used first, keeping only GPUs whose
/// `/dev/nvidiaN` exists when any do (a container given one device by hand
/// still sees every GPU in `/proc`). Without it, `/dev/nvidiaN` numbers are
/// matched to NVIDIA GPUs in PCI order, which is how the driver numbers them.
fn visible_nvidia_slots(
    gpus: &[DetectedGpu],
    proc_listed: &[ProcNvidiaGpu],
    minors: &[u32],
) -> Option<BTreeSet<String>> {
    let visible: BTreeSet<String> = if proc_listed.is_empty() {
        if minors.is_empty() {
            return None;
        }
        let mut slots: Vec<&str> = gpus
            .iter()
            .filter(|g| g.vendor == GpuVendor::Nvidia)
            .filter_map(|g| g.pci_slot.as_deref())
            .collect();
        slots.sort_unstable();
        let mut visible = BTreeSet::new();
        for minor in minors {
            let slot = usize::try_from(*minor).ok().and_then(|m| slots.get(m))?;
            visible.insert((*slot).to_string());
        }
        visible
    } else {
        proc_listed
            .iter()
            .filter(|p| minors.is_empty() || p.minor.is_none_or(|m| minors.contains(&m)))
            .filter_map(|p| p.slot.clone())
            .collect()
    };
    // Hiding every GPU would only mean the numbering was misread.
    (!visible.is_empty()).then_some(visible)
}

/// Hide NVIDIA GPUs outside `visible` (PCI slots): the host has them, but
/// this container can't use them.
fn apply_nvidia_visibility(gpus: &mut [DetectedGpu], visible: &BTreeSet<String>) {
    for gpu in gpus.iter_mut().filter(|g| g.vendor == GpuVendor::Nvidia) {
        gpu.hidden = gpu
            .pci_slot
            .as_ref()
            .is_some_and(|slot| !visible.contains(slot));
        if gpu.hidden {
            tracing::debug!(slot = ?gpu.pci_slot, "NVIDIA GPU not available to this container");
        }
    }
}

/// Whether the board has a Rockchip video engine. Docker hides the device
/// tree (`/sys/firmware`), so the MPP driver's sysfs entry is checked first.
fn detect_rockchip_vpu(root: &Path) -> bool {
    if root.join("sys/class/misc/mpp_service").exists() {
        return true;
    }
    [
        "proc/device-tree/compatible",
        "sys/firmware/devicetree/base/compatible",
    ]
    .iter()
    .filter_map(|rel| fs::read(root.join(rel)).ok())
    .any(|bytes| bytes.windows(8).any(|w| w == b"rockchip"))
}

/// N of every `/dev/nvidiaN` file (not `nvidiactl`, `nvidia-uvm`...), in
/// ascending order.
fn nvidia_minors(root: &Path) -> Vec<u32> {
    sorted_entries(&root.join("dev"))
        .iter()
        .filter_map(|(name, _)| {
            let n = name.strip_prefix("nvidia")?;
            if n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            n.parse().ok()
        })
        .collect()
}

/// One row of `nvidia-smi --query-gpu=name,driver_version,pci.bus_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NvidiaSmiGpu {
    /// Marketing name, e.g. "NVIDIA GeForce RTX 3060".
    pub name: String,
    /// Driver version, e.g. "550.54.14".
    pub driver_version: String,
    /// PCI address with an 8-digit domain, e.g. "00000000:01:00.0".
    pub bus_id: String,
}

/// Parse `nvidia-smi --query-gpu=name,driver_version,pci.bus_id
/// --format=csv,noheader` output.
pub fn parse_nvidia_smi(text: &str) -> Vec<NvidiaSmiGpu> {
    text.lines()
        .filter_map(|line| {
            // The name comes first and is the only field that could contain
            // a comma, so split from the right.
            let mut parts = line.rsplitn(3, ',');
            let bus_id = parts.next()?.trim();
            let driver_version = parts.next()?.trim();
            let name = parts.next()?.trim();
            (!name.is_empty() && !bus_id.is_empty()).then(|| NvidiaSmiGpu {
                name: name.to_string(),
                driver_version: driver_version.to_string(),
                bus_id: bus_id.to_string(),
            })
        })
        .collect()
}

async fn merge_nvidia_smi(devices: &mut Devices) {
    let result = run_capture(
        "nvidia-smi",
        [
            "--query-gpu=name,driver_version,pci.bus_id",
            "--format=csv,noheader",
        ],
        TOOL_TIMEOUT,
    )
    .await;
    match result {
        Ok(out) if out.status.success() => {
            let listed = parse_nvidia_smi(&out.stdout);
            if listed.is_empty() {
                return;
            }
            devices.gpus.retain(|g| !g.is_nvidia_placeholder());
            let mut visible = BTreeSet::new();
            for gpu in listed {
                tracing::debug!(name = %gpu.name, driver = %gpu.driver_version, bus = %gpu.bus_id, "nvidia-smi GPU");
                let slot = normalize_pci_slot(&gpu.bus_id);
                if let Some(slot) = &slot {
                    visible.insert(slot.clone());
                }
                merge_nvidia_gpu(&mut devices.gpus, slot, Some(nvidia_name(&gpu.name)));
            }
            // nvidia-smi lists exactly the GPUs this container may use.
            if !visible.is_empty() {
                apply_nvidia_visibility(&mut devices.gpus, &visible);
            }
        }
        Ok(out) => {
            tracing::debug!(stderr = %out.stderr.trim(), stdout = %out.stdout.trim(), "nvidia-smi failed");
        }
        Err(RunError::NotFound) => {}
        Err(err) => tracing::debug!(error = %err, "nvidia-smi could not run"),
    }
}

/// Build a GPU name from one line of `lspci -mm` output, e.g.
/// `00:02.0 "VGA compatible controller" "Intel Corporation" "Alder Lake-S GT1
/// [UHD Graphics 730]" ...` becomes "Intel UHD Graphics 730".
pub fn gpu_name_from_lspci(line: &str, vendor: GpuVendor) -> Option<String> {
    let quoted: Vec<&str> = line.split('"').skip(1).step_by(2).collect();
    let device = quoted.get(2)?.trim();
    if device.is_empty() || device.starts_with("Device ") {
        // pci.ids doesn't know this device; "Device 56a5" is no help.
        return None;
    }
    // Prefer the marketing name in the last [brackets]: "GA106 [GeForce RTX
    // 3060]" -> "GeForce RTX 3060".
    let core = match (device.rfind('['), device.rfind(']')) {
        (Some(open), Some(close)) if close > open + 1 => device[open + 1..close].trim(),
        _ => device,
    };
    let prefix = vendor_label(vendor);
    if prefix.is_empty()
        || core
            .to_ascii_lowercase()
            .starts_with(&prefix.to_ascii_lowercase())
    {
        Some(core.to_string())
    } else {
        Some(format!("{prefix} {core}"))
    }
}

async fn name_gpus_with_lspci(devices: &mut Devices) {
    for gpu in devices.gpus.iter_mut() {
        if gpu.name.is_some() {
            continue;
        }
        let Some(slot) = gpu.pci_slot.clone() else {
            continue;
        };
        match run_capture("lspci", ["-mm", "-s", slot.as_str()], TOOL_TIMEOUT).await {
            Ok(out) if out.status.success() => {
                gpu.name = out
                    .stdout
                    .lines()
                    .next()
                    .and_then(|line| gpu_name_from_lspci(line, gpu.vendor));
            }
            // lspci isn't installed; don't try again for the next GPU.
            Err(RunError::NotFound) => break,
            Ok(_) | Err(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------
// macOS
// ---------------------------------------------------------------------------

async fn sysctl(name: &str) -> Option<String> {
    let out = run_capture("sysctl", ["-n", name], TOOL_TIMEOUT)
        .await
        .ok()?;
    let value = out.stdout.trim();
    (out.status.success() && !value.is_empty()).then(|| value.to_string())
}

/// Available memory from `vm_stat`: free, inactive and speculative pages.
pub fn parse_vm_stat(text: &str) -> Option<u64> {
    let page_size: u64 = text
        .lines()
        .next()?
        .split("page size of ")
        .nth(1)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;
    let pages = |name: &str| -> u64 {
        text.lines()
            .find_map(|line| {
                let rest = line.strip_prefix(name)?.strip_prefix(':')?;
                rest.trim().trim_end_matches('.').parse::<u64>().ok()
            })
            .unwrap_or(0)
    };
    let total = pages("Pages free") + pages("Pages inactive") + pages("Pages speculative");
    total.checked_mul(page_size)
}

/// CPU, memory and the built-in GPU on a Mac, where `/proc` doesn't exist.
async fn apply_macos_details(devices: &mut Devices) {
    let brand = sysctl("machdep.cpu.brand_string").await;
    if let Some(brand) = &brand {
        devices.cpu.model = brand.clone();
    }
    if let Some(cores) = sysctl("hw.physicalcpu").await.and_then(|v| v.parse().ok()) {
        devices.cpu.physical_cores = Some(cores);
    }
    if let Some(total) = sysctl("hw.memsize").await.and_then(|v| v.parse().ok()) {
        devices.memory.total_bytes = total;
        devices.memory.available_bytes = total;
    }
    if let Ok(out) = run_capture("vm_stat", std::iter::empty::<&str>(), TOOL_TIMEOUT).await
        && let Some(available) = parse_vm_stat(&out.stdout)
    {
        devices.memory.available_bytes = match devices.memory.total_bytes {
            0 => available,
            total => available.min(total),
        };
    }
    let mut gpu = DetectedGpu::new(GpuVendor::Apple);
    gpu.name = Some(match brand {
        Some(b) if b.starts_with("Apple") => format!("{b} GPU"),
        _ => "Mac GPU".to_string(),
    });
    devices.gpus.push(gpu);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::path::Path;

    /// Write `contents` to `root/rel`, creating parent directories.
    pub(crate) fn put(root: &Path, rel: &str, contents: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[cfg(unix)]
    pub(crate) fn link(root: &Path, rel: &str, target: &str) {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(target, path).unwrap();
    }

    /// A PCI GPU under `sys/devices`, reachable from `sys/bus/pci/devices`.
    #[cfg(unix)]
    pub(crate) fn pci_device(
        root: &Path,
        parent: &str,
        slot: &str,
        vendor: &str,
        device: &str,
        class: &str,
        driver: Option<&str>,
    ) {
        let dir = format!("sys/devices/{parent}/{slot}");
        put(root, &format!("{dir}/vendor"), &format!("{vendor}\n"));
        put(root, &format!("{dir}/device"), &format!("{device}\n"));
        put(root, &format!("{dir}/class"), &format!("{class}\n"));
        if let Some(driver) = driver {
            link(
                root,
                &format!("{dir}/driver"),
                &format!("../../../bus/pci/drivers/{driver}"),
            );
        }
        link(
            root,
            &format!("sys/bus/pci/devices/{slot}"),
            &format!("../../../devices/{parent}/{slot}"),
        );
    }

    /// Fake machine: 8-core/16-thread Intel CPU with an iGPU and an NVIDIA
    /// RTX 3060, running in a Docker container limited to 4 CPUs and 8 GiB.
    #[cfg(unix)]
    pub(crate) fn intel_nvidia_container(root: &Path) {
        let mut cpuinfo = String::new();
        for i in 0..16 {
            cpuinfo.push_str(&format!(
                "processor\t: {i}\nvendor_id\t: GenuineIntel\nmodel\t\t: 165\n\
                 model name\t: Intel(R) Core(TM) i7-10700 CPU @ 2.90GHz\n\
                 physical id\t: 0\nsiblings\t: 16\ncore id\t\t: {}\ncpu cores\t: 8\n\n",
                i % 8
            ));
        }
        put(root, "proc/cpuinfo", &cpuinfo);
        put(
            root,
            "proc/meminfo",
            "MemTotal:       32768000 kB\nMemFree:         1000000 kB\nMemAvailable:   20480000 kB\n",
        );
        put(root, "proc/self/cgroup", "0::/\n");
        put(root, "proc/1/cgroup", "0::/\n");
        put(root, ".dockerenv", "");
        put(root, "sys/fs/cgroup/cpu.max", "400000 100000\n");
        put(root, "sys/fs/cgroup/memory.max", "8589934592\n");
        put(root, "sys/fs/cgroup/memory.current", "1073741824\n");

        // Intel UHD 630 on the CPU, bound to i915, with a render node.
        pci_device(
            root,
            "pci0000:00",
            "0000:00:02.0",
            "0x8086",
            "0x9bc5",
            "0x030000",
            Some("i915"),
        );
        link(
            root,
            "sys/class/drm/renderD128/device",
            "../../../devices/pci0000:00/0000:00:02.0",
        );
        link(
            root,
            "sys/class/drm/card0/device",
            "../../../devices/pci0000:00/0000:00:02.0",
        );
        // NVIDIA RTX 3060 behind a bridge, with nvidia-drm's render node.
        pci_device(
            root,
            "pci0000:00/0000:00:01.0",
            "0000:01:00.0",
            "0x10de",
            "0x2504",
            "0x030000",
            Some("nvidia"),
        );
        link(
            root,
            "sys/class/drm/renderD129/device",
            "../../../devices/pci0000:00/0000:00:01.0/0000:01:00.0",
        );
        // An AMD GPU reserved for a VM: must be ignored.
        pci_device(
            root,
            "pci0000:00/0000:00:03.0",
            "0000:02:00.0",
            "0x1002",
            "0x73ff",
            "0x030000",
            Some("vfio-pci"),
        );
        // A host bridge: not a GPU.
        pci_device(
            root,
            "pci0000:00",
            "0000:00:00.0",
            "0x8086",
            "0x9b33",
            "0x060000",
            None,
        );

        put(
            root,
            "proc/driver/nvidia/gpus/0000:01:00.0/information",
            "Model: \t\t NVIDIA GeForce RTX 3060\nIRQ:   \t\t 150\nGPU UUID: \t GPU-1234\nBus Location: \t 0000:01:00.0\n",
        );
        put(root, "dev/dri/renderD128", "");
        put(root, "dev/dri/renderD129", "");
        put(root, "dev/dri/card0", "");
        put(root, "dev/nvidia0", "");
        put(root, "dev/nvidiactl", "");
        put(root, "dev/nvidia-uvm", "");
    }

    #[cfg(unix)]
    #[test]
    fn scans_intel_and_nvidia_in_a_limited_container() {
        let tmp = tempfile::tempdir().unwrap();
        intel_nvidia_container(tmp.path());
        let d = scan_system(tmp.path(), None);

        assert_eq!(d.cpu.model, "Intel(R) Core(TM) i7-10700 CPU @ 2.90GHz");
        assert_eq!(d.cpu.logical_cores, 16);
        assert!(d.pci_listed);
        assert_eq!(d.cpu.physical_cores, Some(8));
        assert_eq!(d.cpu.cgroup_limit, Some(4.0));

        assert_eq!(d.memory.total_bytes, 32_768_000 * 1024);
        assert_eq!(d.memory.cgroup_limit_bytes, Some(8 << 30));
        // 8 GiB limit minus 1 GiB in use, below MemAvailable.
        assert_eq!(d.memory.available_bytes, 7 << 30);

        assert!(d.in_container);
        assert!(d.dri_present);
        assert_eq!(d.nvidia_device_nodes, 1);
        assert!(d.nvidia_visible());

        assert_eq!(d.gpus.len(), 2, "{:#?}", d.gpus);
        let intel = &d.gpus[0];
        assert_eq!(intel.vendor, GpuVendor::Intel);
        assert_eq!(intel.driver.as_deref(), Some("i915"));
        assert_eq!(intel.pci_slot.as_deref(), Some("0000:00:02.0"));
        assert_eq!(intel.device_id, Some(0x9bc5));
        let node = intel.render_node.as_ref().unwrap();
        assert_eq!(node.path, "/dev/dri/renderD128");
        assert_eq!(node.access, NodeAccess::Ok);
        assert_eq!(intel.display_name(), "Intel GPU (renderD128)");

        // The NVIDIA GPU appears once even though it has a render node,
        // a PCI entry and a /proc/driver/nvidia entry.
        let nvidia = &d.gpus[1];
        assert_eq!(nvidia.vendor, GpuVendor::Nvidia);
        assert_eq!(nvidia.display_name(), "NVIDIA GeForce RTX 3060");
        assert_eq!(nvidia.driver.as_deref(), Some("nvidia"));

        let public = d.gpu_devices();
        assert_eq!(
            public[0].render_node.as_deref(),
            Some("/dev/dri/renderD128")
        );
        assert_eq!(public[1].render_node, None);
        assert_eq!(public[1].name, "NVIDIA GeForce RTX 3060");
    }

    #[cfg(unix)]
    #[test]
    fn render_node_missing_from_dev_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        pci_device(
            root,
            "pci0000:00",
            "0000:00:02.0",
            "0x8086",
            "0x46a6",
            "0x030000",
            Some("i915"),
        );
        link(
            root,
            "sys/class/drm/renderD128/device",
            "../../../devices/pci0000:00/0000:00:02.0",
        );
        let d = scan_system(root, Some(4));
        assert_eq!(d.cpu.logical_cores, 4);
        assert!(!d.dri_present);
        assert_eq!(d.gpus.len(), 1);
        assert_eq!(
            d.gpus[0].render_node.as_ref().unwrap().access,
            NodeAccess::Missing
        );
        assert_eq!(d.gpus[0].usable_node(), None);
    }

    #[cfg(unix)]
    #[test]
    fn gpu_without_driver_is_found_on_the_pci_bus() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        pci_device(
            root,
            "pci0000:00",
            "0000:00:02.0",
            "0x8086",
            "0x3e92",
            "0x030000",
            None,
        );
        let d = scan_system(root, None);
        assert_eq!(d.gpus.len(), 1);
        assert_eq!(d.gpus[0].vendor, GpuVendor::Intel);
        assert_eq!(d.gpus[0].driver, None);
        assert!(d.gpus[0].render_node.is_none());
        assert_eq!(d.gpus[0].display_name(), "Intel GPU");
        // Nothing in cpuinfo: sensible defaults.
        assert_eq!(d.cpu.model, "Unknown CPU");
        assert_eq!(d.cpu.logical_cores, 1);
        assert!(!d.in_container);
    }

    #[test]
    fn detects_rockchip_boards() {
        let tmp = tempfile::tempdir().unwrap();
        let d = scan_system(tmp.path(), None);
        assert!(!d.rockchip_vpu && !d.mpp_present);
        put(tmp.path(), "sys/class/misc/mpp_service/dev", "241:0\n");
        let d = scan_system(tmp.path(), None);
        assert!(d.rockchip_vpu && !d.mpp_present);

        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "proc/device-tree/compatible",
            "radxa,rock-5b\0rockchip,rk3588\0",
        );
        put(tmp.path(), "dev/mpp_service", "");
        let d = scan_system(tmp.path(), None);
        assert!(d.rockchip_vpu && d.mpp_present);
    }

    #[test]
    fn nvidia_device_file_alone_gives_a_generic_gpu() {
        let tmp = tempfile::tempdir().unwrap();
        put(tmp.path(), "dev/nvidia0", "");
        put(tmp.path(), "dev/nvidia1", "");
        put(tmp.path(), "dev/nvidiactl", "");
        let d = scan_system(tmp.path(), None);
        assert_eq!(d.nvidia_device_nodes, 2);
        assert_eq!(d.gpus.len(), 2);
        assert!(d.gpus.iter().all(|g| g.display_name() == "NVIDIA GPU"));
        assert!(d.gpus[0].is_nvidia_placeholder());
    }

    #[test]
    fn permission_errors_are_classified() {
        let denied = io::Error::from(io::ErrorKind::PermissionDenied);
        assert_eq!(access_from_error(&denied), NodeAccess::PermissionDenied);
        let eacces = io::Error::from_raw_os_error(13);
        assert_eq!(access_from_error(&eacces), NodeAccess::PermissionDenied);
        // EPERM comes from the container's device rules, not file modes.
        let eperm = io::Error::from_raw_os_error(1);
        assert_eq!(access_from_error(&eperm), NodeAccess::Blocked);
        let missing = io::Error::from(io::ErrorKind::NotFound);
        assert_eq!(access_from_error(&missing), NodeAccess::Missing);
        let busy = io::Error::other("device busy");
        assert!(matches!(access_from_error(&busy), NodeAccess::Failed(_)));
    }

    #[test]
    fn parses_arm_cpuinfo() {
        let pi = "processor\t: 0\nBogoMIPS\t: 108.00\nCPU implementer\t: 0x41\nCPU part\t: 0xd08\n\n\
                  processor\t: 1\nCPU implementer\t: 0x41\nCPU part\t: 0xd08\n\n\
                  Hardware\t: BCM2835\nRevision\t: c03114\nModel\t\t: Raspberry Pi 4 Model B Rev 1.4\n";
        let parsed = parse_cpuinfo(pi);
        assert_eq!(parsed.processors, 2);
        assert_eq!(parsed.physical_cores, None);
        assert_eq!(
            parsed.board_model.as_deref(),
            Some("Raspberry Pi 4 Model B Rev 1.4")
        );
        assert_eq!(parsed.hardware.as_deref(), Some("BCM2835"));

        let tmp = tempfile::tempdir().unwrap();
        // A Rockchip board: only CPU parts, plus a device-tree model.
        put(
            tmp.path(),
            "proc/cpuinfo",
            "processor\t: 0\nCPU implementer\t: 0x41\nCPU part\t: 0xd05\n\n",
        );
        assert_eq!(read_cpu(tmp.path(), None).model, "ARM Cortex-A55");
        put(tmp.path(), "proc/device-tree/model", "Radxa ROCK 5B\0");
        assert_eq!(read_cpu(tmp.path(), None).model, "Radxa ROCK 5B");
    }

    #[test]
    fn parses_cgroup_limits() {
        assert_eq!(parse_cpu_max("max 100000\n"), None);
        assert_eq!(parse_cpu_max("200000 100000\n"), Some(2.0));
        assert_eq!(parse_cpu_max("150000 100000"), Some(1.5));
        assert_eq!(parse_cfs_quota("-1\n", "100000\n"), None);
        assert_eq!(parse_cfs_quota("50000", "100000"), Some(0.5));
        assert_eq!(parse_memory_limit("max\n"), None);
        assert_eq!(parse_memory_limit("9223372036854771712\n"), None);
        assert_eq!(parse_memory_limit("4294967296\n"), Some(4 << 30));
        assert_eq!(parse_memory_limit("0"), None);
    }

    #[test]
    fn cgroup_v1_limits_are_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        put(
            root,
            "proc/self/cgroup",
            "12:memory:/docker/abc\n4:cpu,cpuacct:/docker/abc\n0::/\n",
        );
        put(
            root,
            "sys/fs/cgroup/cpu,cpuacct/cpu.cfs_quota_us",
            "150000\n",
        );
        put(
            root,
            "sys/fs/cgroup/cpu,cpuacct/cpu.cfs_period_us",
            "100000\n",
        );
        put(
            root,
            "sys/fs/cgroup/memory/memory.limit_in_bytes",
            "2147483648\n",
        );
        put(
            root,
            "sys/fs/cgroup/memory/memory.usage_in_bytes",
            "536870912\n",
        );
        put(
            root,
            "proc/meminfo",
            "MemTotal: 16000000 kB\nMemAvailable: 12000000 kB\n",
        );
        put(root, "proc/1/cgroup", "12:memory:/docker/abc\n");
        assert_eq!(cgroup_cpu_limit(root), Some(1.5));
        let mem = read_memory(root);
        assert_eq!(mem.cgroup_limit_bytes, Some(2 << 30));
        assert_eq!(mem.available_bytes, (2 << 30) - (512 << 20));
        assert!(detect_container(root));
    }

    #[test]
    fn nested_cgroup_v2_takes_the_smallest_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        put(root, "proc/self/cgroup", "0::/system.slice/app.service\n");
        put(
            root,
            "sys/fs/cgroup/system.slice/cpu.max",
            "300000 100000\n",
        );
        put(
            root,
            "sys/fs/cgroup/system.slice/app.service/cpu.max",
            "max 100000\n",
        );
        put(
            root,
            "sys/fs/cgroup/system.slice/app.service/memory.max",
            "max\n",
        );
        assert_eq!(cgroup_cpu_limit(root), Some(3.0));
        assert_eq!(read_memory(root).cgroup_limit_bytes, None);
    }

    #[test]
    fn container_markers() {
        assert!(cgroup_mentions_container(
            "12:pids:/kubepods/burstable/pod1\n"
        ));
        assert!(cgroup_mentions_container(
            "0::/system.slice/docker-abc.scope\n"
        ));
        assert!(!cgroup_mentions_container("0::/init.scope\n"));
        let tmp = tempfile::tempdir().unwrap();
        assert!(!detect_container(tmp.path()));
        put(tmp.path(), "run/.containerenv", "");
        assert!(detect_container(tmp.path()));
        let tmp = tempfile::tempdir().unwrap();
        put(
            tmp.path(),
            "proc/1/environ",
            "PATH=/bin\0container=podman\0",
        );
        assert!(detect_container(tmp.path()));
    }

    #[test]
    fn parses_meminfo_and_cpu_lists() {
        let (total, available) =
            parse_meminfo("MemTotal:  1024 kB\nMemFree: 10 kB\nMemAvailable: 512 kB\n");
        assert_eq!(total, Some(1024 * 1024));
        assert_eq!(available, Some(512 * 1024));
        let (_, free_only) = parse_meminfo("MemTotal: 1024 kB\nMemFree: 10 kB\n");
        assert_eq!(free_only, Some(10 * 1024));
        assert_eq!(count_cpu_list("0-3,6,8-9\n"), Some(7));
        assert_eq!(count_cpu_list("0\n"), Some(1));
        assert_eq!(count_cpu_list("garbage"), None);
    }

    #[test]
    fn normalizes_pci_slots() {
        assert_eq!(
            normalize_pci_slot("00000000:01:00.0").as_deref(),
            Some("0000:01:00.0")
        );
        assert_eq!(
            normalize_pci_slot("0000:0A:00.1").as_deref(),
            Some("0000:0a:00.1")
        );
        assert_eq!(
            normalize_pci_slot("01:00.0").as_deref(),
            Some("0000:01:00.0")
        );
        assert_eq!(normalize_pci_slot("fde60000.gpu"), None);
        assert_eq!(normalize_pci_slot("renderD128"), None);
    }

    #[test]
    fn parses_nvidia_tools() {
        let smi = "NVIDIA GeForce RTX 3060, 550.54.14, 00000000:01:00.0\n\
                   Tesla T4, 550.54.14, 00000000:3B:00.0\n";
        let gpus = parse_nvidia_smi(smi);
        assert_eq!(gpus.len(), 2);
        assert_eq!(gpus[0].name, "NVIDIA GeForce RTX 3060");
        assert_eq!(gpus[0].driver_version, "550.54.14");
        assert_eq!(
            normalize_pci_slot(&gpus[1].bus_id).as_deref(),
            Some("0000:3b:00.0")
        );
        assert_eq!(nvidia_name(&gpus[1].name), "NVIDIA Tesla T4");

        let mut list = vec![{
            let mut g = DetectedGpu::new(GpuVendor::Nvidia);
            g.pci_slot = Some("0000:01:00.0".into());
            g
        }];
        merge_nvidia_gpu(
            &mut list,
            Some("0000:01:00.0".into()),
            Some("NVIDIA GeForce RTX 3060".into()),
        );
        merge_nvidia_gpu(
            &mut list,
            Some("0000:3b:00.0".into()),
            Some("NVIDIA Tesla T4".into()),
        );
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].display_name(), "NVIDIA GeForce RTX 3060");

        let info = "Model: \t\t NVIDIA GeForce GTX 1660 SUPER\nIRQ: 16\n";
        assert_eq!(
            parse_nvidia_information(info).as_deref(),
            Some("NVIDIA GeForce GTX 1660 SUPER")
        );
    }

    #[test]
    fn names_gpus_from_lspci() {
        let intel = r#"00:02.0 "VGA compatible controller" "Intel Corporation" "Alder Lake-S GT1 [UHD Graphics 730]" -r0c -p00 "Dell" "Device 0a54""#;
        assert_eq!(
            gpu_name_from_lspci(intel, GpuVendor::Intel).as_deref(),
            Some("Intel UHD Graphics 730")
        );
        let amd = r#"03:00.0 "VGA compatible controller" "Advanced Micro Devices, Inc. [AMD/ATI]" "Navi 23 [Radeon RX 6600/6600 XT/6600M]" -rc7 "XFX Limited" "Device 6505""#;
        assert_eq!(
            gpu_name_from_lspci(amd, GpuVendor::Amd).as_deref(),
            Some("AMD Radeon RX 6600/6600 XT/6600M")
        );
        let nvidia = r#"01:00.0 "VGA compatible controller" "NVIDIA Corporation" "GA106 [GeForce RTX 3060 Lite Hash Rate]" -ra1 "ASUSTeK Computer Inc." "Device 881d""#;
        assert_eq!(
            gpu_name_from_lspci(nvidia, GpuVendor::Nvidia).as_deref(),
            Some("NVIDIA GeForce RTX 3060 Lite Hash Rate")
        );
        let plain =
            r#"00:02.0 "VGA compatible controller" "Intel Corporation" "HD Graphics 630" -r04"#;
        assert_eq!(
            gpu_name_from_lspci(plain, GpuVendor::Intel).as_deref(),
            Some("Intel HD Graphics 630")
        );
        let unknown =
            r#"00:02.0 "VGA compatible controller" "Intel Corporation" "Device 56a5" -r05"#;
        assert_eq!(gpu_name_from_lspci(unknown, GpuVendor::Intel), None);
        assert_eq!(gpu_name_from_lspci("garbage", GpuVendor::Intel), None);
    }

    #[test]
    fn parses_vm_stat() {
        let text = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\n\
                    Pages free:                               10000.\n\
                    Pages active:                            200000.\n\
                    Pages inactive:                           5000.\n\
                    Pages speculative:                        1000.\n";
        assert_eq!(parse_vm_stat(text), Some(16_000 * 16_384));
        assert_eq!(parse_vm_stat("nonsense"), None);
    }

    /// Two NVIDIA cards in the host (say one for Plex, one for
    /// Chrysopoeia); the container was given one.
    #[cfg(unix)]
    fn two_nvidia_host(root: &Path) {
        for slot in ["0000:01:00.0", "0000:02:00.0"] {
            pci_device(
                root,
                "pci0000:00",
                slot,
                "0x10de",
                "0x2504",
                "0x030000",
                Some("nvidia"),
            );
        }
        put(root, ".dockerenv", "");
    }

    #[cfg(unix)]
    #[test]
    fn nvidia_gpus_outside_the_container_are_hidden() {
        // The NVIDIA runtime lists only the GPU it exposes in /proc.
        let tmp = tempfile::tempdir().unwrap();
        two_nvidia_host(tmp.path());
        put(
            tmp.path(),
            "proc/driver/nvidia/gpus/0000:02:00.0/information",
            "Model: \t\t NVIDIA GeForce RTX 3060\nDevice Minor: \t 1\n",
        );
        put(tmp.path(), "dev/nvidia1", "");
        let d = scan_system(tmp.path(), Some(8));
        assert_eq!(d.gpus.len(), 2);
        assert!(d.gpus[0].hidden, "{:#?}", d.gpus);
        assert!(!d.gpus[1].hidden);
        let public = d.gpu_devices();
        assert_eq!(public.len(), 1);
        assert_eq!(public[0].name, "NVIDIA GeForce RTX 3060");

        // Only /dev/nvidia1: numbers follow PCI order, so the second card.
        let tmp = tempfile::tempdir().unwrap();
        two_nvidia_host(tmp.path());
        put(tmp.path(), "dev/nvidia1", "");
        put(tmp.path(), "dev/nvidiactl", "");
        let d = scan_system(tmp.path(), Some(8));
        assert_eq!(d.nvidia_device_nodes, 1);
        assert_eq!(
            d.gpus.iter().map(|g| g.hidden).collect::<Vec<_>>(),
            [true, false]
        );

        // /proc lists both (a device passed by hand, without the runtime),
        // but only /dev/nvidia0 exists.
        let tmp = tempfile::tempdir().unwrap();
        two_nvidia_host(tmp.path());
        for (slot, minor) in [("0000:01:00.0", 0), ("0000:02:00.0", 1)] {
            put(
                tmp.path(),
                &format!("proc/driver/nvidia/gpus/{slot}/information"),
                &format!("Model: \t\t NVIDIA T400\nDevice Minor: \t {minor}\n"),
            );
        }
        put(tmp.path(), "dev/nvidia0", "");
        let d = scan_system(tmp.path(), Some(8));
        assert_eq!(d.gpu_devices().len(), 1);
        assert!(!d.gpus[0].hidden && d.gpus[1].hidden);

        // No NVIDIA runtime at all: nothing is known, so nothing is hidden
        // (the hints explain why NVENC fails).
        let tmp = tempfile::tempdir().unwrap();
        two_nvidia_host(tmp.path());
        let d = scan_system(tmp.path(), Some(8));
        assert_eq!(d.gpu_devices().len(), 2);

        // Numbering that doesn't match hides nothing.
        let tmp = tempfile::tempdir().unwrap();
        two_nvidia_host(tmp.path());
        put(tmp.path(), "dev/nvidia5", "");
        let d = scan_system(tmp.path(), Some(8));
        assert!(d.gpus.iter().all(|g| !g.hidden));
    }

    #[test]
    fn cpu_count_follows_the_cpuset() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cpuinfo = String::new();
        for i in 0..16 {
            cpuinfo.push_str(&format!(
                "processor\t: {i}\nmodel name\t: Test CPU\nphysical id\t: 0\ncore id\t\t: {}\n\n",
                i % 8
            ));
        }
        put(tmp.path(), "proc/cpuinfo", &cpuinfo);
        assert_eq!(read_cpu(tmp.path(), Some(2)).logical_cores, 16);
        // `--cpuset-cpus=0-3`: 4 usable CPUs, however many the host has.
        put(
            tmp.path(),
            "proc/self/status",
            "Name:\tchrysopoeia\nUid:\t99\t99\t99\t99\nGid:\t100\t100\t100\t100\n\
             Groups:\t44 100 105\nCpus_allowed:\tf\nCpus_allowed_list:\t0-3\n",
        );
        // A fractional quota is kept as is; `recommend` rounds it up.
        put(tmp.path(), "sys/fs/cgroup/cpu.max", "250000 100000\n");
        let cpu = read_cpu(tmp.path(), Some(2));
        assert_eq!(cpu.logical_cores, 4);
        assert_eq!(cpu.physical_cores, Some(4));
        assert_eq!(cpu.cgroup_limit, Some(2.5));

        let status = parse_proc_status(&read_text(&tmp.path().join("proc/self/status")).unwrap());
        assert_eq!(status.uid, Some(99));
        assert_eq!(status.gids, [100, 44, 105]);
        assert_eq!(status.cpus_allowed, Some(4));
        assert_eq!(parse_proc_status("garbage"), ProcStatus::default());
    }

    #[cfg(unix)]
    #[test]
    fn file_modes_decide_whether_a_refusal_is_a_group_problem() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let tmp = tempfile::tempdir().unwrap();
        let node = tmp.path().join("renderD128");
        fs::write(&node, "").unwrap();
        let meta = |mode: u32| {
            fs::set_permissions(&node, fs::Permissions::from_mode(mode)).unwrap();
            fs::metadata(&node).unwrap()
        };
        let m = meta(0o660);
        let (owner, group) = (m.uid(), m.gid());
        let stranger = |gids: Vec<u32>| ProcStatus {
            uid: Some(owner.wrapping_add(1)),
            gids,
            cpus_allowed: None,
        };
        // Owner: owner bits.
        let me = ProcStatus {
            uid: Some(owner),
            gids: Vec::new(),
            cpus_allowed: None,
        };
        assert!(mode_allows_rw(&meta(0o600), &me));
        assert!(!mode_allows_rw(&meta(0o400), &me));
        // In the owning group: group bits.
        assert!(mode_allows_rw(&meta(0o660), &stranger(vec![group])));
        assert!(!mode_allows_rw(&meta(0o600), &stranger(vec![group])));
        // Anyone else: other bits (a 0666 node is open to everyone).
        let outsider = stranger(vec![group.wrapping_add(1)]);
        assert!(!mode_allows_rw(&meta(0o660), &outsider));
        assert!(mode_allows_rw(&meta(0o666), &outsider));
        // Unknown ids: never assume it is allowed.
        assert!(!mode_allows_rw(&meta(0o666), &ProcStatus::default()));
    }

    #[test]
    fn empty_pci_bus_is_noted() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!scan_system(tmp.path(), None).pci_listed);
    }

    #[test]
    fn sorts_render_nodes_numerically() {
        let mut names = ["renderD1000", "renderD129", "renderD128"];
        names.sort_by_key(|n| natural_key(n));
        assert_eq!(names, ["renderD128", "renderD129", "renderD1000"]);
    }
}
