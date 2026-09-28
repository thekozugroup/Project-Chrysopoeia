//! Setup hints: what's wrong or worth knowing about the hardware setup,
//! written for people who are not video or Docker experts, each with a
//! copy-paste fix where one exists.

use std::collections::BTreeSet;

use chrysopoeia_core::{FfmpegInfo, GpuVendor, HwApi, SetupHint, SetupHintLevel, VideoCodec};

use crate::devices::{DetectedGpu, Devices, NodeAccess, Platform, vendor_label};
use crate::encoders::{EncoderCheck, FailureKind, VERIFICATION_FILTERS};

/// Docker flags that give a container the NVIDIA driver.
pub const NVIDIA_DOCKER_FIX: &str =
    "--runtime=nvidia -e NVIDIA_VISIBLE_DEVICES=all -e NVIDIA_DRIVER_CAPABILITIES=all";

/// Docker flags that pass a Rockchip video engine into a container.
pub const ROCKCHIP_DOCKER_FIX: &str = "--device=/dev/mpp_service --device=/dev/dri";

/// Docker flag that passes Intel and AMD GPUs into a container.
pub const DRI_DOCKER_FIX: &str = "--device=/dev/dri";

/// Everything hints are derived from.
#[derive(Debug, Clone, Copy)]
pub struct HintInput<'a> {
    pub ffmpeg: &'a FfmpegInfo,
    pub devices: &'a Devices,
    pub checks: &'a [EncoderCheck],
    /// Verification filters this ffmpeg has.
    pub filters: &'a [String],
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

/// "a", "a and b", "a, b and c".
fn join_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
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
    let first = devices.gpus_from(GpuVendor::Nvidia).next();
    let name = first.map_or_else(|| "NVIDIA GPU".to_string(), DetectedGpu::display_name);

    let listed: Vec<&EncoderCheck> = input.listed(HwApi::Nvenc).collect();
    if listed.is_empty() {
        out.push(hint(
            SetupHintLevel::Warning,
            "This ffmpeg can't use NVIDIA GPUs",
            format!(
                "Chrysopoeia found your {name}, but its ffmpeg was built without NVIDIA support (NVENC). \
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
            SetupHintLevel::Warning,
            "NVIDIA GPU is busy",
            format!(
                "Your {name} has no free encoding sessions because other apps, such as Plex or Jellyfin, are using them. \
                 Chrysopoeia uses the CPU for now; run detection again later."
            ),
            None,
        ));
    } else if any(FailureKind::NvidiaDriverTooOld) {
        out.push(hint(
            SetupHintLevel::Warning,
            "NVIDIA driver needs an update",
            format!(
                "Your {name} was found, but its driver is too old for this ffmpeg. \
                 Update the NVIDIA driver on the host (on Unraid, in the Nvidia-Driver plugin), then restart the container."
            ),
            None,
        ));
    } else if all(FailureKind::CodecUnsupported) {
        out.push(hint(
            SetupHintLevel::Info,
            "Your NVIDIA GPU can't encode video",
            format!("Your {name} has no hardware video encoder for these formats, so Chrysopoeia encodes on the CPU."),
            None,
        ));
    } else if devices.in_container {
        out.push(hint(
            SetupHintLevel::Warning,
            "NVIDIA GPU found, but it can't be used",
            format!(
                "Your {name} is in this server, but the container can't reach the NVIDIA driver, so encoding runs on the CPU. \
                 On Unraid, install the Nvidia-Driver plugin, then add the settings below to the container \
                 (--runtime=nvidia goes in Extra Parameters)."
            ),
            Some(NVIDIA_DOCKER_FIX.to_string()),
        ));
    } else {
        let nouveau = first.and_then(|g| g.driver.as_deref()) == Some("nouveau");
        let detail = if nouveau {
            format!(
                "Your {name} uses the open-source nouveau driver, which can't encode video. \
                 Install NVIDIA's own driver to use it."
            )
        } else {
            format!(
                "Your {name} was found, but the NVIDIA driver isn't working, so encoding runs on the CPU. \
                 Check that NVIDIA's driver is installed and that nvidia-smi works."
            )
        };
        out.push(hint(
            SetupHintLevel::Warning,
            "NVIDIA GPU found, but it can't be used",
            detail,
            None,
        ));
    }
}

/// Render nodes of Intel/AMD GPUs this process may not open.
fn permission_hints(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    let denied: Vec<_> = input
        .devices
        .gpus
        .iter()
        .filter(|g| g.vendor != GpuVendor::Nvidia)
        .filter_map(|g| g.render_node.as_ref())
        .filter(|n| n.access == NodeAccess::PermissionDenied)
        .filter(|n| {
            // A verified encoder on the node means it works after all.
            !input
                .checks
                .iter()
                .any(|c| c.status.verified && c.status.device.as_deref() == Some(n.path.as_str()))
        })
        .collect();
    let Some(first) = denied.first() else {
        return;
    };
    let paths = join_list(&denied.iter().map(|n| n.path.clone()).collect::<Vec<_>>());
    let gids: BTreeSet<u32> = denied.iter().filter_map(|n| n.gid).collect();

    let (detail, fix) = if input.devices.in_container {
        let group = match gids.iter().next() {
            Some(gid) if gids.len() == 1 => format!(" (group {gid})"),
            _ => String::new(),
        };
        let fix = if gids.is_empty() {
            format!("--group-add $(stat -c %g {})", first.path)
        } else {
            gids.iter()
                .map(|g| format!("--group-add {g}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        (
            format!(
                "Chrysopoeia isn't allowed to open {paths}. \
                 Add the container to the group that owns it{group}, or set PGID to that group."
            ),
            fix,
        )
    } else {
        (
            format!(
                "Chrysopoeia isn't allowed to open {paths}. \
                 Add the user that runs it to the render and video groups, then log in again."
            ),
            "sudo usermod -aG render,video $USER".to_string(),
        )
    };
    out.push(hint(
        SetupHintLevel::Warning,
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
    let names = join_list(&missing.iter().map(|g| g.display_name()).collect::<Vec<_>>());
    if input.devices.in_container {
        out.push(hint(
            SetupHintLevel::Warning,
            "Your GPU isn't passed into the container",
            format!(
                "This server has {names}, but /dev/dri isn't passed into the container, so encoding runs on the CPU. \
                 Add the device below (on Unraid, add a Device with the value /dev/dri to the template)."
            ),
            Some(DRI_DOCKER_FIX.to_string()),
        ));
    } else {
        let paths = join_list(
            &missing
                .iter()
                .filter_map(|g| g.render_node.as_ref().map(|n| n.path.clone()))
                .collect::<Vec<_>>(),
        );
        out.push(hint(
            SetupHintLevel::Warning,
            "Your GPU's device file is missing",
            format!("This server has {names}, but {paths} doesn't exist, so it can't be used. Check that the GPU driver is loaded."),
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

        if gpus.iter().all(|g| g.render_node.is_none()) {
            let plugin = if vendor == GpuVendor::Intel {
                "Intel GPU TOP"
            } else {
                "Radeon TOP"
            };
            out.push(hint(
                SetupHintLevel::Warning,
                format!("{label} GPU has no driver"),
                format!(
                    "Your {name} isn't set up on the host, so Chrysopoeia can't use it. \
                     On Unraid, install the {plugin} plugin, then restart the container."
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

        let in_build: Vec<&EncoderCheck> = match vendor {
            GpuVendor::Intel => input
                .listed(HwApi::Qsv)
                .chain(input.listed(HwApi::Vaapi))
                .collect(),
            _ => input.listed(HwApi::Vaapi).collect(),
        };
        if in_build.is_empty() {
            if input.listed(HwApi::Amf).any(|c| c.status.verified) {
                continue;
            }
            let what = if vendor == GpuVendor::Intel {
                "Quick Sync or VA-API support"
            } else {
                "VA-API support"
            };
            out.push(hint(
                SetupHintLevel::Warning,
                format!("This ffmpeg can't use {label} GPUs"),
                format!(
                    "Chrysopoeia found your {name}, but its ffmpeg was built without {what}. \
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
        let all = |kind: FailureKind| kinds.iter().all(|k| *k == kind);
        if kinds.is_empty() || all(FailureKind::Permission) || all(FailureKind::CodecUnsupported) {
            continue;
        }

        let (detail, fix) = match (vendor, in_container) {
            (GpuVendor::Intel, true) => (
                format!(
                    "The test encode on your {name} failed, which usually means the Intel media driver is missing or too old for this GPU. \
                     The official image includes a recent one; on Unraid, the Intel GPU TOP plugin also helps."
                ),
                None,
            ),
            (GpuVendor::Intel, false) => (
                format!(
                    "The test encode on your {name} failed, which usually means the Intel media driver (iHD) is missing or too old for this GPU."
                ),
                Some("sudo apt install intel-media-va-driver-non-free".to_string()),
            ),
            (_, true) => (
                format!(
                    "The test encode on your {name} failed, which usually means the Mesa VA-API driver is missing or too old for this GPU. \
                     The official image includes it."
                ),
                None,
            ),
            (_, false) => (
                format!(
                    "The test encode on your {name} failed, which usually means the Mesa VA-API driver is missing or too old for this GPU."
                ),
                Some("sudo apt install mesa-va-drivers".to_string()),
            ),
        };
        out.push(hint(
            SetupHintLevel::Warning,
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
    out.push(hint(
        SetupHintLevel::Warning,
        "Your Rockchip video engine isn't passed into the container",
        "This board can encode video in hardware, but /dev/mpp_service isn't passed into the container, \
         so encoding runs on the CPU. Add the devices below (and /dev/rga and /dev/dma_heap if your board has them).",
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

/// The GPU does HEVC but not AV1: point at the goal that stays fast.
fn av1_tip(input: &HintInput<'_>, out: &mut Vec<SetupHint>) {
    if input.hw_verified_for(VideoCodec::Hevc) && !input.hw_verified_for(VideoCodec::Av1) {
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
    // Intel and AMD GPUs live in x86 machines; ARM boards have their own hints.
    let pass_dri = input.platform.linux
        && !input.platform.arm
        && input.devices.in_container
        && !input.devices.dri_present;
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
        }
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
        assert_eq!(hints[0].fix.as_deref(), Some("--group-add 44"));
        assert!(hints[0].detail.contains("/dev/dri/renderD128"));
        assert!(hints[0].detail.contains("(group 44)"));
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
        assert!(hints[0].detail.contains("Intel media driver"));

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

    #[test]
    fn lists_read_naturally() {
        assert_eq!(join_list(&["a".into()]), "a");
        assert_eq!(join_list(&["a".into(), "b".into()]), "a and b");
        assert_eq!(
            join_list(&["a".into(), "b".into(), "c".into()]),
            "a, b and c"
        );
    }
}
