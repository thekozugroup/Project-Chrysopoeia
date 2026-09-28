//! Setup hints: what's wrong or worth knowing about the hardware setup,
//! written for people who are not video or Docker experts, each with a
//! copy-paste fix where one exists.
//!
//! A GPU that can't be used is a warning only when nothing better is
//! working: with another hardware encoder verified, the same hint becomes a
//! tip that says which one Chrysopoeia uses instead.

use std::collections::BTreeSet;

use chrysopoeia_core::{
    EncoderStatus, FfmpegInfo, GpuVendor, HardwareInfo, HwApi, HwPreference, SetupHint,
    SetupHintLevel, VideoCodec,
};

use crate::devices::{DetectedGpu, Devices, NodeAccess, Platform, vendor_label};
use crate::encoders::{EncoderCheck, FailureKind, VERIFICATION_FILTERS};
use crate::recommend::{BYTES_PER_JOB, HW_API_ORDER, format_gb, memory_budget};

/// Docker flags that give a container the NVIDIA driver.
pub const NVIDIA_DOCKER_FIX: &str =
    "--runtime=nvidia -e NVIDIA_VISIBLE_DEVICES=all -e NVIDIA_DRIVER_CAPABILITIES=all";

/// Docker flags that pass a Rockchip video engine into a container.
pub const ROCKCHIP_DOCKER_FIX: &str = "--device=/dev/mpp_service --device=/dev/dri";

/// Docker flag that passes Intel and AMD GPUs into a container.
pub const DRI_DOCKER_FIX: &str = "--device=/dev/dri";

/// Docker flag that gives a container enough memory for a few encodes.
pub const MEMORY_DOCKER_FIX: &str = "--memory=4g";

/// Title of the hint shown when every NVENC session was taken during the
/// test encodes.
pub const NVIDIA_BUSY_TITLE: &str = "NVIDIA GPU is busy";

/// Everything hints are derived from.
#[derive(Debug, Clone, Copy)]
pub struct HintInput<'a> {
    /// Whether ffmpeg and ffprobe run.
    pub ffmpeg: &'a FfmpegInfo,
    /// What the hardware scan found.
    pub devices: &'a Devices,
    /// One result per registry encoder.
    pub checks: &'a [EncoderCheck],
    /// Verification filters this ffmpeg has.
    pub filters: &'a [String],
    /// The platform Chrysopoeia runs on.
    pub platform: Platform,
}

impl HintInput<'_> {
    fn any_hw_verified(&self) -> bool {
        self.checks
            .iter()
            .any(|c| c.status.verified && c.status.api.is_hardware())
    }

    fn hw_verified_for(&self, codec: VideoCodec) -> bool {
        self.checks
            .iter()
            .any(|c| c.status.verified && c.status.api.is_hardware() && c.status.codec == codec)
    }

    fn listed(&self, api: HwApi) -> impl Iterator<Item = &EncoderCheck> {
        self.checks
            .iter()
            .filter(move |c| c.status.api == api && c.status.available)
    }

    /// How a hint about an unusable GPU ends: the preferred hardware API
    /// that works anyway, ignoring encoders `on_this_gpu` says belong to the
    /// GPU in question.
    fn fallback(&self, on_this_gpu: impl Fn(&EncoderStatus) -> bool) -> Fallback {
        let working = HW_API_ORDER.iter().copied().find(|api| {
            self.checks
                .iter()
                .any(|c| c.status.verified && c.status.api == *api && !on_this_gpu(&c.status))
        });
        Fallback { working }
    }
}

/// What Chrysopoeia encodes with while a GPU can't be used.
#[derive(Debug, Clone, Copy)]
struct Fallback {
    /// A hardware API that works, if any.
    working: Option<HwApi>,
}

impl Fallback {
    /// A warning when encoding falls back to the CPU; a tip otherwise.
    fn level(self) -> SetupHintLevel {
        match self.working {
            Some(_) => SetupHintLevel::Info,
            None => SetupHintLevel::Warning,
        }
    }

    /// "so encoding runs on the CPU" or "so Chrysopoeia uses NVIDIA NVENC
    /// instead".
    fn clause(self) -> String {
        match self.working {
            Some(api) => format!("so Chrysopoeia uses {} instead", api.label()),
            None => "so encoding runs on the CPU".to_string(),
        }
    }
}

fn hint(
    level: SetupHintLevel,
    title: impl Into<String>,
    detail: impl Into<String>,
    fix: Option<String>,
) -> SetupHint {
    SetupHint {
        level,
        title: title.into(),
        detail: detail.into(),
        fix,
    }
}

/// Failures that come from a test encode (not "not tested" or "no device").
fn tested_failure(kind: FailureKind) -> bool {
    !matches!(
        kind,
        FailureKind::NotInBuild | FailureKind::NotTested | FailureKind::NoDevice
    )
}

/// Test failures that only show the GPU hung: every one timed out or,
/// before that, found a codec the GPU can't do.
fn only_hung(kinds: &[FailureKind]) -> bool {
    kinds.contains(&FailureKind::TimedOut)
        && kinds
            .iter()
            .all(|k| matches!(k, FailureKind::TimedOut | FailureKind::CodecUnsupported))
}

/// "a", "a and b", "a, b and c".
fn join_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// "isn't" or "aren't" for `n` things.
fn isnt(n: usize) -> &'static str {
    if n == 1 { "isn't" } else { "aren't" }
}

/// Build every hint that applies, errors first, then warnings, then tips.
pub fn build_hints(input: &HintInput<'_>) -> Vec<SetupHint> {
    let mut hints = Vec::new();
    ffmpeg_hints(input, &mut hints);
    if input.ffmpeg.found {
        nvidia_hints(input, &mut hints);
        permission_hints(input, &mut hints);
        passthrough_hints(input, &mut hints);
        intel_amd_hints(input, &mut hints);
        rockchip_hint(input, &mut hints);
        missing_codec_hints(input, &mut hints);
        filter_hints(input, &mut hints);
        memory_hint(input, &mut hints);
        av1_tip(input, &mut hints);
        cpu_only_hint(input, &mut hints);
    }
    hints.sort_by_key(|h| match h.level {
        SetupHintLevel::Error => 0,
        SetupHintLevel::Warning => 1,
        SetupHintLevel::Info => 2,
    });
    hints
}

/// Title of the hint shown when the hardware chosen in Settings has no
/// working encoder (see [`preference_hint`]).
pub const PREFERENCE_UNAVAILABLE_TITLE: &str = "The hardware you chose isn't working";

/// How an API's encoders are named in a sentence: "no working NVIDIA
/// encoder was found".
fn encoder_noun(api: HwApi) -> &'static str {
    match api {
        HwApi::Software => "CPU encoder",
        HwApi::Nvenc => "NVIDIA encoder",
        HwApi::Qsv => "Intel Quick Sync encoder",
        HwApi::Vaapi => "VA-API encoder",
        HwApi::VideoToolbox => "VideoToolbox encoder",
        HwApi::Amf => "AMD encoder",
        HwApi::Rkmpp => "Rockchip encoder",
        HwApi::V4l2m2m => "V4L2 encoder",
    }
}

/// Why the hardware chosen in Settings (`preference`) can't be used, as the
/// start of a sentence: "You chose NVIDIA NVENC, but no working NVIDIA
/// encoder was found here". With a `codec`, also when that hardware works
/// but can't make the codec ("…, but it can't make AV1 video here").
/// `None` when nothing is pinned (Automatic, CPU) or the hardware works.
pub fn preference_problem(
    hw: &HardwareInfo,
    preference: HwPreference,
    codec: Option<VideoCodec>,
) -> Option<String> {
    let api = preference.api().filter(|a| a.is_hardware())?;
    let works = |codec: Option<VideoCodec>| {
        hw.encoders.iter().any(|e| {
            e.api == api && e.available && e.verified && codec.is_none_or(|c| e.codec == c)
        })
    };
    if !works(None) {
        return Some(format!(
            "You chose {}, but no working {} was found here",
            api.label(),
            encoder_noun(api)
        ));
    }
    let codec = codec?;
    (!works(Some(codec))).then(|| {
        format!(
            "You chose {}, but it can't make {} video here",
            api.label(),
            codec.label()
        )
    })
}

/// A hint when the hardware chosen in Settings has no working encoder at
/// all: files are converted on the CPU instead, or, with CPU fallback off,
/// not at all. `None` while it works (or nothing is pinned).
pub fn preference_hint(
    hw: &HardwareInfo,
    preference: HwPreference,
    cpu_fallback: bool,
) -> Option<SetupHint> {
    let problem = preference_problem(hw, preference, None)?;
    let (level, detail) = if cpu_fallback {
        (
            SetupHintLevel::Warning,
            format!(
                "{problem}, so files are converted on the CPU. The other hints here say how to \
                 make it work, or choose Automatic under Hardware in Settings."
            ),
        )
    } else {
        (
            SetupHintLevel::Error,
            format!(
                "{problem}, and converting on the CPU instead is turned off, so files can't be \
                 converted. Choose Automatic under Hardware in Settings, or allow CPU fallback."
            ),
        )
    };
    Some(hint(level, PREFERENCE_UNAVAILABLE_TITLE, detail, None))
}

fn ffmpeg_hints(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    let f = input.ffmpeg;
    let fix = if input.platform.macos {
        Some("brew install ffmpeg".to_string())
    } else if input.platform.linux && !input.devices.in_container {
        Some("sudo apt install ffmpeg".to_string())
    } else {
        None
    };
    match (f.found, f.ffprobe_found) {
        (true, true) => {}
        (false, false) => out.push(hint(
            SetupHintLevel::Error,
            "ffmpeg isn't installed",
            format!(
                "Chrysopoeia needs ffmpeg and ffprobe to convert files, but couldn't run \"{}\" or \"{}\". \
                 The official Docker image includes both; elsewhere, install ffmpeg or set FFMPEG_PATH and FFPROBE_PATH.",
                f.ffmpeg_path, f.ffprobe_path
            ),
            fix,
        )),
        (false, true) => out.push(hint(
            SetupHintLevel::Error,
            "ffmpeg isn't working",
            format!(
                "Chrysopoeia needs ffmpeg to convert files, but couldn't run \"{}\". \
                 Install ffmpeg or set FFMPEG_PATH to where it is.",
                f.ffmpeg_path
            ),
            fix,
        )),
        (true, false) => out.push(hint(
            SetupHintLevel::Error,
            "ffprobe isn't working",
            format!(
                "Chrysopoeia needs ffprobe, which comes with ffmpeg, to read your files, but couldn't run \"{}\". \
                 Install ffmpeg or set FFPROBE_PATH to where ffprobe is.",
                f.ffprobe_path
            ),
            fix,
        )),
    }
}

fn nvidia_hints(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    let devices = input.devices;
    if !devices.nvidia_visible() {
        return;
    }
    // Name the GPU this container was given, if the host has several.
    let first = devices
        .gpus_from(GpuVendor::Nvidia)
        .find(|g| !g.hidden)
        .or_else(|| devices.gpus_from(GpuVendor::Nvidia).next());
    let name = first.map_or_else(|| "NVIDIA GPU".to_string(), DetectedGpu::display_name);
    let fallback = input.fallback(|s| s.api == HwApi::Nvenc);
    let clause = fallback.clause();

    let listed: Vec<&EncoderCheck> = input.listed(HwApi::Nvenc).collect();
    if listed.is_empty() {
        out.push(hint(
            fallback.level(),
            "This ffmpeg can't use NVIDIA GPUs",
            format!(
                "Chrysopoeia found your {name}, but its ffmpeg was built without NVIDIA support (NVENC), {clause}. \
                 The official Docker image includes an ffmpeg that has it."
            ),
            None,
        ));
        return;
    }
    if listed.iter().any(|c| c.status.verified) {
        return;
    }
    let kinds: Vec<FailureKind> = listed
        .iter()
        .filter_map(|c| c.failure)
        .filter(|k| tested_failure(*k))
        .collect();
    if kinds.is_empty() {
        return;
    }
    let all = |kind: FailureKind| kinds.iter().all(|k| *k == kind);
    let any = |kind: FailureKind| kinds.contains(&kind);

    if any(FailureKind::NvencSessionLimit) && !any(FailureKind::NvidiaDriverMissing) {
        out.push(hint(
            fallback.level(),
            NVIDIA_BUSY_TITLE,
            format!(
                "Your {name} has no free encoding sessions because other apps, such as Plex or Jellyfin, are using them all, \
                 {clause} for now. Chrysopoeia checks again by itself in a few minutes, or select Check again in Settings once they finish."
            ),
            None,
        ));
    } else if any(FailureKind::NvidiaDriverTooOld) {
        out.push(hint(
            fallback.level(),
            "NVIDIA driver needs an update",
            format!(
                "Your {name} was found, but its driver is too old for this ffmpeg, {clause}. \
                 Update the NVIDIA driver on the host (on Unraid, in the Nvidia-Driver plugin), then restart the container."
            ),
            None,
        ));
    } else if all(FailureKind::CodecUnsupported) {
        out.push(hint(
            SetupHintLevel::Info,
            "Your NVIDIA GPU can't encode video",
            format!("Your {name} has no hardware video encoder for these formats, {clause}."),
            None,
        ));
    } else if only_hung(&kinds) {
        out.push(hung_hint("NVIDIA", &name, fallback));
    } else if devices.in_container {
        let next = if fallback.working.is_some() {
            "To use it as well, install the Nvidia-Driver plugin on Unraid and add the settings below to the container"
        } else {
            "On Unraid, install the Nvidia-Driver plugin, then add the settings below to the container"
        };
        out.push(hint(
            fallback.level(),
            "NVIDIA GPU found, but it can't be used",
            format!(
                "Your {name} is in this server, but the container can't reach the NVIDIA driver, {clause}. \
                 {next} (--runtime=nvidia goes in Extra Parameters)."
            ),
            Some(NVIDIA_DOCKER_FIX.to_string()),
        ));
    } else {
        let nouveau = first.and_then(|g| g.driver.as_deref()) == Some("nouveau");
        let detail = if nouveau {
            format!(
                "Your {name} uses the open-source nouveau driver, which can't encode video, {clause}. \
                 Install NVIDIA's own driver to use it."
            )
        } else {
            format!(
                "Your {name} was found, but the NVIDIA driver isn't working, {clause}. \
                 Check that NVIDIA's driver is installed and that nvidia-smi works."
            )
        };
        out.push(hint(
            fallback.level(),
            "NVIDIA GPU found, but it can't be used",
            detail,
            None,
        ));
    }
}

/// A GPU whose test encodes hung.
fn hung_hint(label: &str, name: &str, fallback: Fallback) -> SetupHint {
    hint(
        fallback.level(),
        format!("{label} GPU didn't respond"),
        format!(
            "Your {name} didn't finish a one-second test encode in time, {}. \
             Restart the container (or the server, if that doesn't help), then select Check again in Settings.",
            fallback.clause()
        ),
        None,
    )
}

/// Render nodes of Intel/AMD GPUs this process may not open.
fn permission_hints(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    let refused = |access: NodeAccess| -> Vec<(String, Option<u32>)> {
        input
            .devices
            .gpus
            .iter()
            .filter(|g| g.vendor != GpuVendor::Nvidia)
            .filter_map(|g| g.render_node.as_ref())
            .filter(|n| n.access == access)
            .filter(|n| {
                // A verified encoder on the node means it works after all.
                !input.checks.iter().any(|c| {
                    c.status.verified && c.status.device.as_deref() == Some(n.path.as_str())
                })
            })
            .map(|n| (n.path.clone(), n.gid))
            .collect()
    };
    let fallback_for = |nodes: &[(String, Option<u32>)]| {
        input.fallback(|s| {
            s.device
                .as_deref()
                .is_some_and(|d| nodes.iter().any(|(p, _)| p == d))
        })
    };
    let in_container = input.devices.in_container;

    // Refused whatever the file permissions: a group won't help.
    let blocked = refused(NodeAccess::Blocked);
    if !blocked.is_empty() {
        let fallback = fallback_for(&blocked);
        let paths = join_list(&blocked.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>());
        let it = if blocked.len() == 1 { "it" } else { "them" };
        let (title, detail, fix) = if in_container {
            (
                "The GPU isn't passed in as a device",
                format!(
                    "Chrysopoeia can see {paths} but isn't allowed to use {it}, {}. This happens when /dev/dri is added \
                     as a folder (a Path) instead of a device: on Unraid, remove that Path from the template and add a \
                     Device with the value /dev/dri.",
                    fallback.clause()
                ),
                Some(DRI_DOCKER_FIX.to_string()),
            )
        } else {
            (
                "The system blocks the GPU",
                format!(
                    "Chrysopoeia isn't allowed to open {paths} even though the file permissions allow it, {}. \
                     A security policy, such as SELinux or a systemd DevicePolicy setting, is probably blocking it.",
                    fallback.clause()
                ),
                None,
            )
        };
        out.push(hint(fallback.level(), title, detail, fix));
    }

    // Refused by the file permissions: a group problem.
    let denied = refused(NodeAccess::PermissionDenied);
    let Some((first_path, _)) = denied.first() else {
        return;
    };
    let fallback = fallback_for(&denied);
    let paths = join_list(&denied.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>());
    let gids: BTreeSet<u32> = denied.iter().filter_map(|(_, g)| *g).collect();

    let (detail, fix) = if in_container {
        let owner = match gids.iter().next() {
            Some(gid) if gids.len() == 1 => format!(", which belongs to group {gid}"),
            _ => String::new(),
        };
        let fix = if gids.is_empty() {
            format!("--group-add $(stat -c %g {first_path})")
        } else {
            gids.iter()
                .map(|g| format!("--group-add {g}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        (
            format!(
                "Chrysopoeia isn't allowed to open {paths}{owner}, {}. \
                 Add the container to that group with the setting below (on Unraid, it goes in Extra Parameters).",
                fallback.clause()
            ),
            fix,
        )
    } else {
        (
            format!(
                "Chrysopoeia isn't allowed to open {paths}, {}. \
                 Add the user that runs it to the render and video groups, then log in again.",
                fallback.clause()
            ),
            "sudo usermod -aG render,video $USER".to_string(),
        )
    };
    out.push(hint(
        fallback.level(),
        "No permission to use the GPU",
        detail,
        Some(fix),
    ));
}

/// Intel/AMD GPUs the host has but whose device file is missing here.
fn passthrough_hints(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    if !input.platform.linux {
        return;
    }
    let missing: Vec<&DetectedGpu> = input
        .devices
        .gpus
        .iter()
        .filter(|g| g.vendor != GpuVendor::Nvidia)
        .filter(|g| {
            g.render_node
                .as_ref()
                .is_some_and(|n| n.access == NodeAccess::Missing)
        })
        .collect();
    if missing.is_empty() {
        return;
    }
    let paths: Vec<String> = missing
        .iter()
        .filter_map(|g| g.render_node.as_ref().map(|n| n.path.clone()))
        .collect();
    let fallback = input.fallback(|s| {
        s.device
            .as_deref()
            .is_some_and(|d| paths.iter().any(|p| p == d))
    });
    let names = join_list(&missing.iter().map(|g| g.display_name()).collect::<Vec<_>>());
    let clause = fallback.clause();
    if input.devices.in_container {
        let add = if fallback.working.is_some() {
            "To use it as well, add the device below"
        } else {
            "Add the device below"
        };
        out.push(hint(
            fallback.level(),
            "Your GPU isn't passed into the container",
            format!(
                "Your {names} {} passed into the container, {clause}. \
                 {add} (on Unraid, add a Device with the value /dev/dri to the template).",
                isnt(missing.len())
            ),
            Some(DRI_DOCKER_FIX.to_string()),
        ));
    } else {
        let (has, are) = if paths.len() == 1 {
            ("has", "is")
        } else {
            ("have", "are")
        };
        out.push(hint(
            fallback.level(),
            "Your GPU's device file is missing",
            format!(
                "Your {names} {has} no device file ({} {are} missing), {clause}. Check that the GPU driver is loaded.",
                join_list(&paths)
            ),
            None,
        ));
    }
}

/// Intel and AMD GPUs whose driver is missing or whose test encodes fail.
fn intel_amd_hints(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    let in_container = input.devices.in_container;
    for vendor in [GpuVendor::Intel, GpuVendor::Amd] {
        let gpus: Vec<&DetectedGpu> = input.devices.gpus_from(vendor).collect();
        let Some(first) = gpus.first() else {
            continue;
        };
        let label = vendor_label(vendor);
        let name = first.display_name();
        let plugin = if vendor == GpuVendor::Intel {
            "Intel GPU TOP"
        } else {
            "Radeon TOP"
        };

        if gpus.iter().all(|g| g.render_node.is_none()) {
            // Nothing can run on a GPU without a driver, so anything
            // verified runs elsewhere.
            let fallback = input.fallback(|_| false);
            out.push(hint(
                fallback.level(),
                format!("{label} GPU has no driver"),
                format!(
                    "Your {name} has no driver loaded on the host, {}. \
                     On Unraid, install the {plugin} plugin, then restart the container.",
                    fallback.clause()
                ),
                None,
            ));
            continue;
        }

        let nodes: Vec<&str> = gpus.iter().filter_map(|g| g.usable_node()).collect();
        if nodes.is_empty() {
            // Missing or forbidden nodes have their own hints.
            continue;
        }
        let fallback = input.fallback(|s| {
            s.device.as_deref().is_some_and(|d| nodes.contains(&d))
                || (vendor == GpuVendor::Amd && s.api == HwApi::Amf)
        });
        let clause = fallback.clause();

        let in_build: Vec<&EncoderCheck> = match vendor {
            GpuVendor::Intel => input
                .listed(HwApi::Qsv)
                .chain(input.listed(HwApi::Vaapi))
                .collect(),
            _ => input.listed(HwApi::Vaapi).collect(),
        };
        if in_build.is_empty() {
            if vendor == GpuVendor::Amd && input.listed(HwApi::Amf).any(|c| c.status.verified) {
                continue;
            }
            let what = if vendor == GpuVendor::Intel {
                "Quick Sync or VA-API support"
            } else {
                "VA-API support"
            };
            out.push(hint(
                fallback.level(),
                format!("This ffmpeg can't use {label} GPUs"),
                format!(
                    "Chrysopoeia found your {name}, but its ffmpeg was built without {what}, {clause}. \
                     The official Docker image includes an ffmpeg that has it."
                ),
                None,
            ));
            continue;
        }

        let on_these_nodes: Vec<&EncoderCheck> = in_build
            .iter()
            .copied()
            .filter(|c| {
                c.status
                    .device
                    .as_deref()
                    .is_some_and(|d| nodes.contains(&d))
            })
            .collect();
        if on_these_nodes.iter().any(|c| c.status.verified) {
            continue;
        }
        let kinds: Vec<FailureKind> = on_these_nodes
            .iter()
            .filter_map(|c| c.failure)
            .filter(|k| tested_failure(*k))
            .collect();
        let all_of = |allowed: &[FailureKind]| kinds.iter().all(|k| allowed.contains(k));
        if kinds.is_empty()
            || all_of(&[FailureKind::Permission, FailureKind::DeviceBlocked])
            || all_of(&[FailureKind::CodecUnsupported])
        {
            continue;
        }
        if only_hung(&kinds) {
            out.push(hung_hint(label, &name, fallback));
            continue;
        }

        let (detail, fix) = match (vendor, in_container) {
            (GpuVendor::Intel, true) => (
                format!(
                    "The test encode on your {name} failed, {clause}. Chrysopoeia's image already includes Intel's media \
                     driver, so the host is the likely cause: on Unraid, install the {plugin} plugin and reboot; \
                     elsewhere, update the kernel and the GPU firmware."
                ),
                None,
            ),
            (GpuVendor::Intel, false) => (
                format!(
                    "The test encode on your {name} failed, {clause}. \
                     This usually means the Intel media driver (iHD) is missing or too old for this GPU."
                ),
                Some("sudo apt install intel-media-va-driver-non-free".to_string()),
            ),
            (_, true) => (
                format!(
                    "The test encode on your {name} failed, {clause}. Chrysopoeia's image already includes the Mesa \
                     VA-API driver, so the host is the likely cause: on Unraid, install the {plugin} plugin and reboot; \
                     elsewhere, update the kernel and the GPU firmware."
                ),
                None,
            ),
            (_, false) => (
                format!(
                    "The test encode on your {name} failed, {clause}. \
                     This usually means the Mesa VA-API driver is missing or too old for this GPU."
                ),
                Some("sudo apt install mesa-va-drivers".to_string()),
            ),
        };
        out.push(hint(
            fallback.level(),
            format!("{label} GPU found, but encoding failed"),
            detail,
            fix,
        ));
    }
}

/// A Rockchip board whose video engine isn't passed into the container.
fn rockchip_hint(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    let d = input.devices;
    if !input.platform.arm || !d.rockchip_vpu || d.mpp_present || !d.in_container {
        return;
    }
    if input.listed(HwApi::Rkmpp).next().is_none() {
        return;
    }
    let fallback = input.fallback(|s| s.api == HwApi::Rkmpp);
    out.push(hint(
        fallback.level(),
        "Your Rockchip video engine isn't passed into the container",
        format!(
            "This board can encode video in hardware, but /dev/mpp_service isn't passed into the container, {}. \
             Add the devices below (and /dev/rga and /dev/dma_heap if your board has them).",
            fallback.clause()
        ),
        Some(ROCKCHIP_DOCKER_FIX.to_string()),
    ));
}

/// Target codecs no encoder (hardware or software) can produce.
fn missing_codec_hints(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    if input.checks.is_empty() {
        return;
    }
    let missing: Vec<String> = VideoCodec::ALL
        .iter()
        .filter(|codec| {
            !input
                .checks
                .iter()
                .any(|c| c.status.codec == **codec && c.status.verified)
        })
        .map(|codec| codec.label().to_string())
        .collect();
    if missing.is_empty() {
        return;
    }
    let (title, pronoun) = match missing.as_slice() {
        [one] => (format!("{one} can't be encoded"), "it"),
        _ => ("Some formats can't be encoded".to_string(), "them"),
    };
    out.push(hint(
        SetupHintLevel::Warning,
        title,
        format!(
            "This ffmpeg can't encode {}, so goals that use {pronoun} won't work. \
             The official Docker image includes every encoder Chrysopoeia needs.",
            join_list(&missing)
        ),
        None,
    ));
}

/// Filters that verification or deinterlacing needs.
fn filter_hints(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    let missing = |names: &[&str]| -> Vec<String> {
        names
            .iter()
            .filter(|n| VERIFICATION_FILTERS.contains(n) && !input.filters.iter().any(|f| f == *n))
            .map(|n| (*n).to_string())
            .collect()
    };
    let noun = |list: &[String]| if list.len() == 1 { "filter" } else { "filters" };

    let visual = missing(&["ssim", "psnr"]);
    if !visual.is_empty() {
        out.push(hint(
            SetupHintLevel::Warning,
            "Visual checks aren't available",
            format!(
                "This ffmpeg lacks the {} {}, so finished files can't be compared with the originals. The other checks still run.",
                join_list(&visual),
                noun(&visual)
            ),
            None,
        ));
    }
    let frames = missing(&["blackdetect", "freezedetect"]);
    if !frames.is_empty() {
        out.push(hint(
            SetupHintLevel::Info,
            "Black and frozen frame checks aren't available",
            format!(
                "This ffmpeg lacks the {} {}, so the Thorough check level skips them.",
                join_list(&frames),
                noun(&frames)
            ),
            None,
        ));
    }
    if !missing(&["bwdif"]).is_empty() {
        out.push(hint(
            SetupHintLevel::Warning,
            "Interlaced video can't be cleaned up",
            "This ffmpeg lacks the bwdif filter, so interlaced videos (such as old TV recordings) can't be deinterlaced.",
            None,
        ));
    }
}

/// Less memory than one encode needs.
fn memory_hint(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    let memory = &input.devices.memory;
    let Some(budget) = memory_budget(memory) else {
        return;
    };
    if budget >= BYTES_PER_JOB {
        return;
    }
    let amount = format_gb(budget);
    let limited = memory.cgroup_limit_bytes == Some(budget);
    let (detail, fix) = match (limited, input.devices.in_container) {
        (true, true) => (
            format!(
                "The container is limited to {amount} of memory, but one encode can need about 1.5 GB, \
                 so large or 4K files may be stopped partway. Raise the limit (on Unraid, --memory in Extra Parameters)."
            ),
            Some(MEMORY_DOCKER_FIX.to_string()),
        ),
        (true, false) => (
            format!(
                "Chrysopoeia is limited to {amount} of memory, but one encode can need about 1.5 GB, \
                 so large or 4K files may be stopped partway. Raise the memory limit of its service."
            ),
            None,
        ),
        (false, _) => (
            format!(
                "This server has {amount} of memory, but one encode can need about 1.5 GB, \
                 so large or 4K files may fail."
            ),
            None,
        ),
    };
    out.push(hint(
        SetupHintLevel::Warning,
        "Not much memory for encoding",
        detail,
        fix,
    ));
}

/// The GPU does HEVC but not AV1: point at the goal that stays fast.
fn av1_tip(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    // A busy or hung GPU says nothing about what it can encode.
    let unsure = input.checks.iter().any(|c| {
        c.status.codec == VideoCodec::Av1
            && matches!(
                c.failure,
                Some(FailureKind::TimedOut | FailureKind::NvencSessionLimit)
            )
    });
    if input.hw_verified_for(VideoCodec::Hevc) && !input.hw_verified_for(VideoCodec::Av1) && !unsure
    {
        out.push(hint(
            SetupHintLevel::Info,
            "Your GPU can't encode AV1",
            "The Save space and Archive goals use AV1, which will run on the CPU and take much longer. \
             The Balanced goal uses HEVC, which your GPU encodes quickly.",
            None,
        ));
    }
}

/// No GPU at all: CPU encoding is fine, and says which goal is quicker.
fn cpu_only_hint(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    if input.any_hw_verified() || !input.devices.gpus.is_empty() || input.devices.nvidia_visible() {
        return;
    }
    // Intel and AMD GPUs live in x86 machines; ARM boards have their own
    // hints. The host's PCI bus is visible in a container, so when it lists
    // devices but no GPU, there is no GPU to pass in.
    let pass_dri = input.platform.linux
        && !input.platform.arm
        && input.devices.in_container
        && !input.devices.dri_present
        && !input.devices.pci_listed;
    let mut detail = "No GPU was found, so files are converted on the CPU. That works well; \
                      AV1 (the Save space goal) is the slowest to encode on a CPU, while Balanced (HEVC) finishes sooner."
        .to_string();
    if pass_dri {
        detail.push_str(" If this server has an Intel or AMD GPU, pass it to the container with the setting below.");
    }
    out.push(hint(
        SetupHintLevel::Info,
        "Encoding on the CPU",
        detail,
        pass_dri.then(|| DRI_DOCKER_FIX.to_string()),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::RenderNode;
    use chrysopoeia_core::{EncoderStatus, VIDEO_ENCODERS};

    const LINUX: Platform = Platform {
        linux: true,
        macos: false,
        arm: false,
    };

    fn ffmpeg_ok() -> FfmpegInfo {
        FfmpegInfo {
            ffmpeg_path: "ffmpeg".into(),
            ffprobe_path: "ffprobe".into(),
            found: true,
            ffprobe_found: true,
            version: Some("ffmpeg version 7.0".into()),
        }
    }

    fn all_filters() -> Vec<String> {
        VERIFICATION_FILTERS
            .iter()
            .map(|f| (*f).to_string())
            .collect()
    }

    /// Every registry encoder: software verified, hardware failing with
    /// `hw_failure` (or verified when `None`) on `device`.
    fn checks(
        hw_failure: impl Fn(HwApi) -> Option<FailureKind>,
        device: Option<&str>,
    ) -> Vec<EncoderCheck> {
        VIDEO_ENCODERS
            .iter()
            .map(|e| {
                let failure = if e.api.is_hardware() {
                    hw_failure(e.api)
                } else {
                    None
                };
                EncoderCheck {
                    status: EncoderStatus {
                        name: e.name.into(),
                        codec: e.codec,
                        api: e.api,
                        available: failure != Some(FailureKind::NotInBuild),
                        verified: failure.is_none(),
                        device: match e.api {
                            HwApi::Vaapi | HwApi::Qsv => device.map(String::from),
                            _ => None,
                        },
                        error: failure.map(|_| "failed".into()),
                    },
                    failure,
                }
            })
            .collect()
    }

    fn gpu(vendor: GpuVendor, node: Option<(&str, NodeAccess)>, driver: &str) -> DetectedGpu {
        DetectedGpu {
            vendor,
            name: Some(format!("{} Test GPU", vendor_label(vendor))),
            sysfs_name: None,
            pci_slot: None,
            device_id: None,
            driver: Some(driver.into()),
            render_node: node.map(|(path, access)| RenderNode {
                path: path.into(),
                access,
                gid: Some(44),
            }),
            hidden: false,
        }
    }

    fn build(devices: &Devices, checks: &[EncoderCheck]) -> Vec<SetupHint> {
        let filters = all_filters();
        build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices,
            checks,
            filters: &filters,
            platform: LINUX,
        })
    }

    fn titles(hints: &[SetupHint]) -> Vec<&str> {
        hints.iter().map(|h| h.title.as_str()).collect()
    }

    #[test]
    fn missing_ffmpeg_is_an_error() {
        let info = FfmpegInfo {
            found: false,
            ffprobe_found: false,
            version: None,
            ..ffmpeg_ok()
        };
        let devices = Devices::default();
        let hints = build_hints(&HintInput {
            ffmpeg: &info,
            devices: &devices,
            checks: &[],
            filters: &[],
            platform: LINUX,
        });
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].level, SetupHintLevel::Error);
        assert_eq!(hints[0].fix.as_deref(), Some("sudo apt install ffmpeg"));
    }

    #[test]
    fn no_gpu_in_container_suggests_passing_dri() {
        let devices = Devices {
            in_container: true,
            ..Devices::default()
        };
        let checks = checks(|_| Some(FailureKind::NoDevice), None);
        let filters = all_filters();
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &checks,
            filters: &filters,
            platform: LINUX,
        });
        assert_eq!(titles(&hints), ["Encoding on the CPU"]);
        assert_eq!(hints[0].level, SetupHintLevel::Info);
        assert_eq!(hints[0].fix.as_deref(), Some(DRI_DOCKER_FIX));
        assert!(!hints[0].detail.contains('%'), "no invented speed numbers");
    }

    #[test]
    fn nvidia_without_runtime_gets_the_docker_fix() {
        let devices = Devices {
            in_container: true,
            dri_present: false,
            gpus: vec![gpu(GpuVendor::Nvidia, None, "nvidia")],
            ..Devices::default()
        };
        let checks = checks(
            |api| match api {
                HwApi::Nvenc => Some(FailureKind::NvidiaDriverMissing),
                _ => Some(FailureKind::NoDevice),
            },
            None,
        );
        let filters = all_filters();
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &checks,
            filters: &filters,
            platform: LINUX,
        });
        assert_eq!(titles(&hints), ["NVIDIA GPU found, but it can't be used"]);
        assert_eq!(hints[0].fix.as_deref(), Some(NVIDIA_DOCKER_FIX));
        assert!(hints[0].detail.contains("Nvidia-Driver plugin"));
        assert!(hints[0].detail.contains("NVIDIA Test GPU"));
    }

    #[test]
    fn nvidia_session_limit_is_explained() {
        let devices = Devices {
            nvidia_device_nodes: 1,
            gpus: vec![gpu(GpuVendor::Nvidia, None, "nvidia")],
            ..Devices::default()
        };
        let checks = checks(
            |api| match api {
                HwApi::Nvenc => Some(FailureKind::NvencSessionLimit),
                _ => Some(FailureKind::NoDevice),
            },
            None,
        );
        let filters = all_filters();
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &checks,
            filters: &filters,
            platform: LINUX,
        });
        assert_eq!(titles(&hints), ["NVIDIA GPU is busy"]);
    }

    #[test]
    fn forbidden_render_node_gets_group_advice() {
        let devices = Devices {
            in_container: true,
            dri_present: true,
            gpus: vec![gpu(
                GpuVendor::Intel,
                Some(("/dev/dri/renderD128", NodeAccess::PermissionDenied)),
                "i915",
            )],
            ..Devices::default()
        };
        let checks = checks(
            |api| match api {
                HwApi::Qsv | HwApi::Vaapi => Some(FailureKind::Permission),
                _ => Some(FailureKind::NoDevice),
            },
            Some("/dev/dri/renderD128"),
        );
        let filters = all_filters();
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &checks,
            filters: &filters,
            platform: LINUX,
        });
        assert_eq!(titles(&hints), ["No permission to use the GPU"]);
        assert_eq!(hints[0].level, SetupHintLevel::Warning);
        assert_eq!(hints[0].fix.as_deref(), Some("--group-add 44"));
        assert!(hints[0].detail.contains("/dev/dri/renderD128"));
        assert!(hints[0].detail.contains("belongs to group 44"));
        assert!(hints[0].detail.contains("Extra Parameters"));
        assert!(!hints[0].detail.contains("PGID"));
    }

    #[test]
    fn gpu_not_passed_into_container() {
        let devices = Devices {
            in_container: true,
            dri_present: false,
            gpus: vec![gpu(
                GpuVendor::Intel,
                Some(("/dev/dri/renderD128", NodeAccess::Missing)),
                "i915",
            )],
            ..Devices::default()
        };
        let checks = checks(
            |api| match api {
                HwApi::Qsv | HwApi::Vaapi => Some(FailureKind::DeviceMissing),
                _ => Some(FailureKind::NoDevice),
            },
            Some("/dev/dri/renderD128"),
        );
        let filters = all_filters();
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &checks,
            filters: &filters,
            platform: LINUX,
        });
        assert_eq!(titles(&hints), ["Your GPU isn't passed into the container"]);
        assert_eq!(hints[0].level, SetupHintLevel::Warning);
        assert_eq!(hints[0].fix.as_deref(), Some(DRI_DOCKER_FIX));
    }

    #[test]
    fn intel_gpu_with_broken_driver_and_no_av1() {
        let node = "/dev/dri/renderD128";
        let devices = Devices {
            in_container: true,
            dri_present: true,
            gpus: vec![gpu(GpuVendor::Intel, Some((node, NodeAccess::Ok)), "i915")],
            ..Devices::default()
        };
        let broken = checks(
            |api| match api {
                HwApi::Qsv => Some(FailureKind::QsvRuntimeMissing),
                HwApi::Vaapi => Some(FailureKind::VaapiUnavailable),
                _ => Some(FailureKind::NoDevice),
            },
            Some(node),
        );
        let filters = all_filters();
        let input = HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &broken,
            filters: &filters,
            platform: LINUX,
        };
        let hints = build_hints(&input);
        assert_eq!(titles(&hints), ["Intel GPU found, but encoding failed"]);
        assert_eq!(hints[0].level, SetupHintLevel::Warning);
        assert!(hints[0].detail.contains("Intel GPU TOP plugin"));
        assert!(hints[0].detail.contains("encoding runs on the CPU"));

        // Working HEVC but no AV1 (e.g. an older iGPU): a tip about Balanced.
        let mut older = checks(
            |api| match api {
                HwApi::Qsv | HwApi::Vaapi => None,
                _ => Some(FailureKind::NoDevice),
            },
            Some(node),
        );
        for c in older
            .iter_mut()
            .filter(|c| c.status.codec == VideoCodec::Av1 && c.status.api.is_hardware())
        {
            c.status.verified = false;
            c.failure = Some(FailureKind::CodecUnsupported);
        }
        let hints = build_hints(&HintInput {
            checks: &older,
            ..input
        });
        assert_eq!(titles(&hints), ["Your GPU can't encode AV1"]);
        assert_eq!(hints[0].level, SetupHintLevel::Info);
        assert!(hints[0].detail.contains("Balanced"));
    }

    #[test]
    fn intel_gpu_without_driver() {
        let devices = Devices {
            gpus: vec![gpu(GpuVendor::Intel, None, "i915")],
            ..Devices::default()
        };
        let checks = checks(|_| Some(FailureKind::DriverNotLoaded), None);
        let filters = all_filters();
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &checks,
            filters: &filters,
            platform: LINUX,
        });
        assert_eq!(titles(&hints), ["Intel GPU has no driver"]);
        assert!(hints[0].detail.contains("Intel GPU TOP"));
    }

    #[test]
    fn missing_encoders_and_filters_are_reported() {
        let devices = Devices {
            nvidia_device_nodes: 1,
            gpus: vec![gpu(GpuVendor::Nvidia, None, "nvidia")],
            ..Devices::default()
        };
        // NVENC works; the software AV1 encoders are missing; so is ssim.
        let mut list = checks(
            |api| match api {
                HwApi::Nvenc => None,
                _ => Some(FailureKind::NotInBuild),
            },
            None,
        );
        for c in list
            .iter_mut()
            .filter(|c| c.status.codec == VideoCodec::Av1)
        {
            c.status.verified = false;
            c.status.available = false;
            c.failure = Some(FailureKind::NotInBuild);
        }
        let filters: Vec<String> = ["psnr", "blackdetect", "freezedetect", "bwdif"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &list,
            filters: &filters,
            platform: LINUX,
        });
        assert_eq!(
            titles(&hints),
            [
                "AV1 can't be encoded",
                "Visual checks aren't available",
                "Your GPU can't encode AV1"
            ]
        );
        assert!(hints[1].detail.contains("ssim filter"));
    }

    #[test]
    fn working_nvidia_needs_no_hints() {
        let devices = Devices {
            nvidia_device_nodes: 1,
            in_container: true,
            gpus: vec![gpu(GpuVendor::Nvidia, None, "nvidia")],
            ..Devices::default()
        };
        let list = checks(
            |api| match api {
                HwApi::Nvenc => None,
                _ => Some(FailureKind::NoDevice),
            },
            None,
        );
        let filters = all_filters();
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &list,
            filters: &filters,
            platform: LINUX,
        });
        assert!(hints.is_empty(), "{hints:#?}");
    }

    #[test]
    fn rockchip_board_without_mpp_passthrough() {
        let devices = Devices {
            in_container: true,
            rockchip_vpu: true,
            mpp_present: false,
            ..Devices::default()
        };
        let checks = checks(|_| Some(FailureKind::NoDevice), None);
        let filters = all_filters();
        let arm = Platform { arm: true, ..LINUX };
        let hints = build_hints(&HintInput {
            ffmpeg: &ffmpeg_ok(),
            devices: &devices,
            checks: &checks,
            filters: &filters,
            platform: arm,
        });
        assert_eq!(
            titles(&hints),
            [
                "Your Rockchip video engine isn't passed into the container",
                "Encoding on the CPU"
            ]
        );
        assert_eq!(hints[0].fix.as_deref(), Some(ROCKCHIP_DOCKER_FIX));
        // No x86 GPU advice on an ARM board.
        assert_eq!(hints[1].fix, None);
    }

    /// Intel iGPU + NVIDIA card, the most common Unraid setup: when one GPU
    /// works, problems with the other are tips, not warnings, and never
    /// claim that encoding runs on the CPU.
    #[test]
    fn a_working_gpu_turns_the_other_gpus_problems_into_tips() {
        let node = "/dev/dri/renderD128";
        let nvenc_only = |fail: FailureKind| {
            checks(
                move |api| match api {
                    HwApi::Nvenc => None,
                    _ => Some(fail),
                },
                Some(node),
            )
        };
        let mut unnamed_intel = gpu(GpuVendor::Intel, Some((node, NodeAccess::Missing)), "i915");
        unnamed_intel.name = None;

        // NVENC works; the iGPU isn't passed into the container.
        let devices = Devices {
            in_container: true,
            nvidia_device_nodes: 1,
            gpus: vec![unnamed_intel, gpu(GpuVendor::Nvidia, None, "nvidia")],
            ..Devices::default()
        };
        let hints = build(&devices, &nvenc_only(FailureKind::DeviceMissing));
        let pass = hints
            .iter()
            .find(|h| h.title == "Your GPU isn't passed into the container")
            .expect("passthrough tip");
        assert_eq!(pass.level, SetupHintLevel::Info);
        assert_eq!(
            pass.detail,
            "Your Intel GPU (renderD128) isn't passed into the container, so Chrysopoeia uses NVIDIA NVENC instead. \
             To use it as well, add the device below (on Unraid, add a Device with the value /dev/dri to the template)."
        );
        assert_eq!(pass.fix.as_deref(), Some(DRI_DOCKER_FIX));
        assert!(
            hints.iter().all(|h| h.level != SetupHintLevel::Warning),
            "{hints:#?}"
        );
        assert!(
            hints.iter().all(|h| !h.detail.contains("on the CPU")),
            "{hints:#?}"
        );

        // NVENC works; the iGPU has no driver on the host.
        let devices = Devices {
            nvidia_device_nodes: 1,
            gpus: vec![
                gpu(GpuVendor::Intel, None, "i915"),
                gpu(GpuVendor::Nvidia, None, "nvidia"),
            ],
            ..Devices::default()
        };
        let hints = build(&devices, &nvenc_only(FailureKind::DriverNotLoaded));
        let driver = hints
            .iter()
            .find(|h| h.title == "Intel GPU has no driver")
            .expect("driver tip");
        assert_eq!(driver.level, SetupHintLevel::Info);
        assert!(driver.detail.contains("uses NVIDIA NVENC instead"));

        // The other way round: Quick Sync works; the NVIDIA card has no
        // runtime.
        let devices = Devices {
            in_container: true,
            dri_present: true,
            gpus: vec![
                gpu(GpuVendor::Intel, Some((node, NodeAccess::Ok)), "i915"),
                gpu(GpuVendor::Nvidia, None, "nvidia"),
            ],
            ..Devices::default()
        };
        let list = checks(
            |api| match api {
                HwApi::Nvenc => Some(FailureKind::NvidiaDriverMissing),
                HwApi::Qsv | HwApi::Vaapi => None,
                _ => Some(FailureKind::NoDevice),
            },
            Some(node),
        );
        let hints = build(&devices, &list);
        let nvidia = hints
            .iter()
            .find(|h| h.title == "NVIDIA GPU found, but it can't be used")
            .expect("NVIDIA tip");
        assert_eq!(nvidia.level, SetupHintLevel::Info);
        assert!(
            nvidia
                .detail
                .contains("so Chrysopoeia uses Intel Quick Sync instead. To use it as well"),
            "{}",
            nvidia.detail
        );
        assert_eq!(nvidia.fix.as_deref(), Some(NVIDIA_DOCKER_FIX));
    }

    #[test]
    fn dri_mounted_as_a_folder_needs_a_device_not_a_group() {
        let node = "/dev/dri/renderD128";
        let devices = Devices {
            in_container: true,
            dri_present: true,
            gpus: vec![gpu(
                GpuVendor::Intel,
                Some((node, NodeAccess::Blocked)),
                "i915",
            )],
            ..Devices::default()
        };
        let list = checks(
            |api| match api {
                HwApi::Qsv | HwApi::Vaapi => Some(FailureKind::DeviceBlocked),
                _ => Some(FailureKind::NoDevice),
            },
            Some(node),
        );
        let hints = build(&devices, &list);
        assert_eq!(titles(&hints), ["The GPU isn't passed in as a device"]);
        assert_eq!(hints[0].level, SetupHintLevel::Warning);
        assert_eq!(hints[0].fix.as_deref(), Some(DRI_DOCKER_FIX));
        assert!(hints[0].detail.contains("Path"));
        assert!(!hints[0].detail.contains("group"));
    }

    #[test]
    fn a_hung_gpu_gets_restart_advice_not_driver_advice() {
        let node = "/dev/dri/renderD128";
        let devices = Devices {
            in_container: true,
            dri_present: true,
            nvidia_device_nodes: 1,
            gpus: vec![
                gpu(GpuVendor::Intel, Some((node, NodeAccess::Ok)), "i915"),
                gpu(GpuVendor::Nvidia, None, "nvidia"),
            ],
            ..Devices::default()
        };
        let list = checks(
            |api| match api {
                HwApi::Nvenc | HwApi::Qsv | HwApi::Vaapi => Some(FailureKind::TimedOut),
                _ => Some(FailureKind::NoDevice),
            },
            Some(node),
        );
        let hints = build(&devices, &list);
        assert_eq!(
            titles(&hints),
            ["NVIDIA GPU didn't respond", "Intel GPU didn't respond"]
        );
        for h in &hints {
            assert_eq!(h.fix, None);
            assert!(h.detail.contains("Restart the container"), "{}", h.detail);
        }
    }

    #[test]
    fn busy_or_hung_av1_is_no_reason_for_the_av1_tip() {
        let devices = Devices {
            nvidia_device_nodes: 1,
            gpus: vec![gpu(GpuVendor::Nvidia, None, "nvidia")],
            ..Devices::default()
        };
        let mut list = checks(
            |api| match api {
                HwApi::Nvenc => None,
                _ => Some(FailureKind::NoDevice),
            },
            None,
        );
        for c in list.iter_mut().filter(|c| c.status.name == "av1_nvenc") {
            c.status.verified = false;
            c.failure = Some(FailureKind::NvencSessionLimit);
        }
        assert!(build(&devices, &list).is_empty());
    }

    #[test]
    fn low_memory_is_a_warning() {
        let devices = Devices {
            in_container: true,
            memory: chrysopoeia_core::MemoryInfo {
                total_bytes: 16 << 30,
                available_bytes: 1 << 30,
                cgroup_limit_bytes: Some(1 << 30),
            },
            pci_listed: true,
            ..Devices::default()
        };
        let list = checks(|_| Some(FailureKind::NoDevice), None);
        let hints = build(&devices, &list);
        assert_eq!(
            titles(&hints),
            ["Not much memory for encoding", "Encoding on the CPU"]
        );
        assert_eq!(hints[0].level, SetupHintLevel::Warning);
        assert!(hints[0].detail.contains("limited to 1 GB"));
        assert_eq!(hints[0].fix.as_deref(), Some(MEMORY_DOCKER_FIX));

        // Enough memory: no hint.
        let roomy = Devices {
            memory: chrysopoeia_core::MemoryInfo {
                total_bytes: 16 << 30,
                available_bytes: 8 << 30,
                cgroup_limit_bytes: Some(4 << 30),
            },
            ..devices
        };
        assert_eq!(titles(&build(&roomy, &list)), ["Encoding on the CPU"]);
    }

    /// Docker shows the host's PCI bus; when it lists no GPU, there is
    /// nothing to pass in (and `--device=/dev/dri` would stop the container
    /// from starting).
    #[test]
    fn no_dri_advice_when_the_host_has_no_gpu() {
        let devices = Devices {
            in_container: true,
            pci_listed: true,
            ..Devices::default()
        };
        let hints = build(&devices, &checks(|_| Some(FailureKind::NoDevice), None));
        assert_eq!(titles(&hints), ["Encoding on the CPU"]);
        assert_eq!(hints[0].fix, None);
        assert!(!hints[0].detail.contains("pass it"));
    }

    #[test]
    fn lists_read_naturally() {
        assert_eq!(join_list(&["a".into()]), "a");
        assert_eq!(join_list(&["a".into(), "b".into()]), "a and b");
        assert_eq!(
            join_list(&["a".into(), "b".into(), "c".into()]),
            "a, b and c"
        );
    }

    fn hardware(hw_failure: impl Fn(HwApi) -> Option<FailureKind>) -> HardwareInfo {
        HardwareInfo {
            cpu: Default::default(),
            memory: Default::default(),
            gpus: Vec::new(),
            encoders: checks(hw_failure, None)
                .into_iter()
                .map(|c| c.status)
                .collect(),
            audio_encoders: Vec::new(),
            filters: all_filters(),
            ffmpeg: ffmpeg_ok(),
            recommended_jobs: chrysopoeia_core::JobRecommendation {
                cpu_jobs: 1,
                gpu_jobs: 0,
                total: 1,
                reason: String::new(),
            },
            hints: Vec::new(),
            in_container: true,
            detecting: false,
            detected_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn a_chosen_api_without_a_working_encoder_is_explained() {
        let no_gpu = hardware(|_| Some(FailureKind::NoDevice));
        assert_eq!(
            preference_problem(&no_gpu, HwPreference::Nvenc, None).as_deref(),
            Some("You chose NVIDIA NVENC, but no working NVIDIA encoder was found here")
        );
        let hint = preference_hint(&no_gpu, HwPreference::Nvenc, true).unwrap();
        assert_eq!(hint.title, PREFERENCE_UNAVAILABLE_TITLE);
        assert_eq!(hint.level, SetupHintLevel::Warning);
        assert!(
            hint.detail.starts_with(
                "You chose NVIDIA NVENC, but no working NVIDIA encoder was found here, so files \
                 are converted on the CPU."
            ),
            "{}",
            hint.detail
        );
        // Without CPU fallback nothing is converted: an error.
        let hint = preference_hint(&no_gpu, HwPreference::Nvenc, false).unwrap();
        assert_eq!(hint.level, SetupHintLevel::Error);
        assert!(
            hint.detail.contains("can't be converted"),
            "{}",
            hint.detail
        );
        // Automatic and CPU pin nothing.
        assert!(preference_hint(&no_gpu, HwPreference::Auto, true).is_none());
        assert!(preference_hint(&no_gpu, HwPreference::Cpu, false).is_none());

        // A working API gets no hint, but a codec it can't make is named.
        let mut working = hardware(|_| None);
        assert!(preference_hint(&working, HwPreference::Qsv, true).is_none());
        for e in working
            .encoders
            .iter_mut()
            .filter(|e| e.api == HwApi::Qsv && e.codec == VideoCodec::Av1)
        {
            e.verified = false;
        }
        assert_eq!(
            preference_problem(&working, HwPreference::Qsv, Some(VideoCodec::Av1)).as_deref(),
            Some("You chose Intel Quick Sync, but it can't make AV1 video here")
        );
        assert!(preference_problem(&working, HwPreference::Qsv, Some(VideoCodec::Hevc)).is_none());
    }
}
